//! HTTP entity-tag comparison shared by protocol adapters.

use hyper::HeaderMap;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EtagError {
    Invalid,
    PreconditionFailed,
}

/// Returns true for a cache hit; the caller must first authorize and select a representation.
pub(crate) fn etag_not_modified(headers: &HeaderMap, etag: &str) -> Result<bool, EtagError> {
    if matches_field(headers, "if-match", etag, Comparison::Strong)? == Some(false) {
        return Err(EtagError::PreconditionFailed);
    }
    Ok(matches_field(headers, "if-none-match", etag, Comparison::Weak)? == Some(true))
}

/// RFC 9110 comparison: If-Match uses strong comparison, If-None-Match weak comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Comparison {
    Strong,
    Weak,
}

fn matches_field(
    headers: &HeaderMap,
    name: &str,
    etag: &str,
    comparison: Comparison,
) -> Result<Option<bool>, EtagError> {
    let strong = comparison == Comparison::Strong;
    let values = headers.get_all(name);
    let mut iter = values.iter();
    if let (Some(only), None) = (iter.next(), iter.next())
        && only.as_bytes().trim_ascii() == b"*"
    {
        return Ok(Some(true));
    }
    let mut matched = None;
    for value in values {
        matched.get_or_insert(false);
        let mut rest = value.as_bytes();
        loop {
            rest = rest.trim_ascii_start();
            if rest.is_empty() {
                break;
            }
            // RFC 9110 #rule permits empty list elements, including a trailing comma.
            if let Some(tail) = rest.strip_prefix(b",") {
                rest = tail;
                continue;
            }
            let weak = rest.starts_with(b"W/");
            if weak {
                rest = &rest[2..];
            }
            if !rest.starts_with(b"\"") {
                return Err(EtagError::Invalid);
            }
            let end = rest[1..]
                .iter()
                .position(|byte| *byte == b'"')
                .map(|end| end + 1)
                .ok_or(EtagError::Invalid)?;
            if rest[1..end]
                .iter()
                .any(|byte| !matches!(byte, 0x21 | 0x23..=0x7e | 0x80..=0xff))
            {
                return Err(EtagError::Invalid);
            }
            if (!strong || !weak) && &rest[..=end] == etag.as_bytes() {
                matched = Some(true);
            }
            rest = rest[end + 1..].trim_ascii_start();
            if rest.is_empty() {
                break;
            }
            rest = rest.strip_prefix(b",").ok_or(EtagError::Invalid)?;
        }
    }
    Ok(matched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::HeaderValue;

    #[test]
    fn etag_comparison_uses_http_lists_and_strong_or_weak_matching() {
        for (value, strong, weak) in [
            ("\"selected\"", true, true),
            ("W/\"selected\"", false, true),
            ("\"other\", W/\"selected\"", false, true),
            ("\"other\", \"selected\"", true, true),
            ("*", true, true),
            ("", false, false),
            (", ,\"selected\",,", true, true),
            ("\"selected,inside\"", false, false),
            ("\"selected\\\"", false, false),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("if-match", HeaderValue::from_str(value).unwrap());
            assert_eq!(
                matches_field(&headers, "if-match", "\"selected\"", Comparison::Strong),
                Ok(Some(strong)),
                "{value}"
            );
            assert_eq!(
                matches_field(&headers, "if-match", "\"selected\"", Comparison::Weak),
                Ok(Some(weak)),
                "{value}"
            );
        }
        let mut headers = HeaderMap::new();
        headers.append("if-match", HeaderValue::from_static("\"other\""));
        headers.append("if-match", HeaderValue::from_static("\"selected\""));
        assert_eq!(etag_not_modified(&headers, "\"selected\""), Ok(false));
        headers.append("if-none-match", HeaderValue::from_static("W/\"selected\""));
        assert_eq!(etag_not_modified(&headers, "\"selected\""), Ok(true));
        headers.insert("if-match", HeaderValue::from_static("\"other\""));
        assert_eq!(
            etag_not_modified(&headers, "\"selected\""),
            Err(EtagError::PreconditionFailed)
        );
    }

    #[test]
    fn etag_comparison_rejects_invalid_tags_even_after_a_matching_member() {
        for value in [
            "selected",
            "W/selected",
            "w/\"selected\"",
            "\"open",
            "\"has space\"",
            "\"selected\" extra",
            "*, \"selected\"",
            "*,*",
            "\"selected\", bad",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("if-match", HeaderValue::from_str(value).unwrap());
            assert_eq!(
                etag_not_modified(&headers, "\"selected\""),
                Err(EtagError::Invalid),
                "{value}"
            );
        }
        let mut headers = HeaderMap::new();
        headers.append("if-none-match", HeaderValue::from_static("*"));
        headers.append("if-none-match", HeaderValue::from_static("*"));
        assert_eq!(
            etag_not_modified(&headers, "\"selected\""),
            Err(EtagError::Invalid)
        );
    }
}
