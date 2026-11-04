//! HTTP entity-tag comparison shared by protocol adapters.

use hyper::HeaderMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReadCondition {
    Proceed,
    NotModified,
    PreconditionFailed,
}

pub(crate) struct Validators<'a> {
    pub(crate) etag: &'a str,
    pub(crate) last_modified: SystemTime,
}

/// XML read conditions follow RFC 9110 section 13.2.2 on an already authorized snapshot.
pub(crate) fn evaluate_read(
    headers: &HeaderMap,
    validators: &Validators<'_>,
) -> Result<ReadCondition, EtagError> {
    let modified = validators
        .last_modified
        .duration_since(UNIX_EPOCH)
        .map(|duration| UNIX_EPOCH + Duration::from_secs(duration.as_secs()))
        .unwrap_or(validators.last_modified);
    match matches_field(headers, "if-match", validators.etag, Comparison::Strong)? {
        Some(false) => return Ok(ReadCondition::PreconditionFailed),
        None if date_field(headers, "if-unmodified-since").is_some_and(|date| modified > date) => {
            return Ok(ReadCondition::PreconditionFailed);
        }
        _ => {}
    }
    match matches_field(headers, "if-none-match", validators.etag, Comparison::Weak)? {
        Some(true) => Ok(ReadCondition::NotModified),
        None if date_field(headers, "if-modified-since").is_some_and(|date| modified <= date) => {
            Ok(ReadCondition::NotModified)
        }
        _ => Ok(ReadCondition::Proceed),
    }
}

fn date_field(headers: &HeaderMap, name: &str) -> Option<SystemTime> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    httpdate::parse_http_date(value.to_str().ok()?).ok()
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EtagError {
    Invalid,
    PreconditionFailed,
}

/// Returns true for a cache hit; the caller must first authorize and select a representation.
pub(crate) fn etag_not_modified(headers: &HeaderMap, etag: &str) -> Result<bool, EtagError> {
    require_etag_match(headers, etag)?;
    Ok(matches_field(headers, "if-none-match", etag, Comparison::Weak)? == Some(true))
}

/// As [`etag_not_modified`], but a matching `If-None-Match` wins over a failing `If-Match`.
/// Cloud Storage answered a JSON media read carrying both with 304 where RFC 9110 orders the
/// failing `If-Match` first.
pub(crate) fn etag_not_modified_first(headers: &HeaderMap, etag: &str) -> Result<bool, EtagError> {
    if matches_field(headers, "if-none-match", etag, Comparison::Weak)? == Some(true) {
        return Ok(true);
    }
    require_etag_match(headers, etag)?;
    Ok(false)
}

/// Call inside the mutation transaction when a write uses the selected representation.
pub(crate) fn require_etag_match(headers: &HeaderMap, etag: &str) -> Result<(), EtagError> {
    if matches_field(headers, "if-match", etag, Comparison::Strong)? == Some(false) {
        return Err(EtagError::PreconditionFailed);
    }
    Ok(())
}

/// RFC 9110 comparison: If-Match uses strong comparison, If-None-Match weak comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Comparison {
    Strong,
    Weak,
}

/// Whether the selected representation's validator is an RFC 9110 quoted entity tag or an
/// unquoted JSON API token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TagSyntax {
    Quoted,
    Opaque,
}

fn matches_field(
    headers: &HeaderMap,
    name: &str,
    etag: &str,
    comparison: Comparison,
) -> Result<Option<bool>, EtagError> {
    let strong = comparison == Comparison::Strong;
    // A JSON API validator is an unquoted base64 token that clients copy verbatim from a
    // resource's `etag`, so a bare list member names the same tag as its quoted form. Quoted
    // validators (the XML API) keep strict RFC 9110 syntax.
    let (syntax, target) = match etag.strip_prefix('"').and_then(|tag| tag.strip_suffix('"')) {
        Some(inner) => (TagSyntax::Quoted, inner.as_bytes()),
        None => (TagSyntax::Opaque, etag.as_bytes()),
    };
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
                rest = rest.strip_prefix(b"W/").ok_or(EtagError::Invalid)?;
            }
            let (tag, tail) = if let Some(quoted) = rest.strip_prefix(b"\"") {
                let end = quoted
                    .iter()
                    .position(|byte| *byte == b'"')
                    .ok_or(EtagError::Invalid)?;
                let (tag, tail) = quoted.split_at_checked(end).ok_or(EtagError::Invalid)?;
                (tag, tail.strip_prefix(b"\"").ok_or(EtagError::Invalid)?)
            } else if syntax == TagSyntax::Opaque {
                let end = rest
                    .iter()
                    .position(|byte| *byte == b',' || byte.is_ascii_whitespace())
                    .unwrap_or(rest.len());
                // A wildcard is valid only as the entire field value.
                let (tag, tail) = rest.split_at_checked(end).ok_or(EtagError::Invalid)?;
                if tag.is_empty() || tag.contains(&b'"') || tag == b"*" {
                    return Err(EtagError::Invalid);
                }
                (tag, tail)
            } else {
                return Err(EtagError::Invalid);
            };
            if tag
                .iter()
                .any(|byte| !matches!(byte, 0x21 | 0x23..=0x7e | 0x80..=0xff))
            {
                return Err(EtagError::Invalid);
            }
            if (!strong || !weak) && tag == target {
                matched = Some(true);
            }
            rest = tail.trim_ascii_start();
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
    fn xml_read_conditions_follow_rfc_precedence_and_second_precision() {
        let modified = httpdate::parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT").unwrap();
        let validators = Validators {
            etag: "\"selected\"",
            last_modified: modified + Duration::from_millis(900),
        };
        for (fields, expected) in [
            (
                vec![("if-match", "\"other\"")],
                ReadCondition::PreconditionFailed,
            ),
            (
                vec![
                    ("if-match", "\"selected\""),
                    ("if-unmodified-since", "Sun, 06 Nov 1994 08:49:36 GMT"),
                ],
                ReadCondition::Proceed,
            ),
            (
                vec![("if-none-match", "W/\"selected\"")],
                ReadCondition::NotModified,
            ),
            (
                vec![
                    ("if-none-match", "\"other\""),
                    ("if-modified-since", "Sun, 06 Nov 1994 08:49:38 GMT"),
                ],
                ReadCondition::Proceed,
            ),
            (
                vec![("if-modified-since", "Sun, 06 Nov 1994 08:49:37 GMT")],
                ReadCondition::NotModified,
            ),
            (
                vec![("if-modified-since", "Sun, 06 Nov 1994 08:49:36 GMT")],
                ReadCondition::Proceed,
            ),
            (
                vec![("if-unmodified-since", "Sun, 06 Nov 1994 08:49:36 GMT")],
                ReadCondition::PreconditionFailed,
            ),
            (
                vec![("if-modified-since", "invalid")],
                ReadCondition::Proceed,
            ),
            (
                vec![
                    ("if-modified-since", "Sun, 06 Nov 1994 08:49:38 GMT"),
                    ("if-modified-since", "Sun, 06 Nov 1994 08:49:38 GMT"),
                ],
                ReadCondition::Proceed,
            ),
            (
                vec![("if-match", "\"other\""), ("if-none-match", "\"selected\"")],
                ReadCondition::PreconditionFailed,
            ),
        ] {
            let mut headers = HeaderMap::new();
            for (key, value) in fields {
                headers.append(key, value.parse().unwrap());
            }
            assert_eq!(
                evaluate_read(&headers, &validators),
                Ok(expected),
                "{headers:?}"
            );
        }
        let mut headers = HeaderMap::new();
        headers.insert("if-none-match", "unquoted".parse().unwrap());
        assert_eq!(
            evaluate_read(&headers, &validators),
            Err(EtagError::Invalid)
        );
    }

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
    fn unquoted_json_validators_match_bare_and_quoted_members() {
        for (value, strong, weak) in [
            ("CAE=", true, true),
            ("\"CAE=\"", true, true),
            ("W/CAE=", false, true),
            ("W/\"CAE=\"", false, true),
            ("CAI=, CAE=", true, true),
            ("CAI=,\"CAE=\"", true, true),
            ("CAI=", false, false),
            ("*", true, true),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("if-match", HeaderValue::from_str(value).unwrap());
            assert_eq!(
                matches_field(&headers, "if-match", "CAE=", Comparison::Strong),
                Ok(Some(strong)),
                "{value}"
            );
            assert_eq!(
                matches_field(&headers, "if-match", "CAE=", Comparison::Weak),
                Ok(Some(weak)),
                "{value}"
            );
        }
        for value in ["CAE= extra", "CA\"E=", "\"CAE=", "W/", "*, CAE=", "CAE=, *"] {
            let mut headers = HeaderMap::new();
            headers.insert("if-none-match", HeaderValue::from_str(value).unwrap());
            assert_eq!(
                etag_not_modified(&headers, "CAE="),
                Err(EtagError::Invalid),
                "{value}"
            );
        }
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
