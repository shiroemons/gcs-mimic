//! How a media read selects the stored bytes it sends: the `Range` it honors, whether it
//! gunzips the stored representation and whether opening the object fetches its body. The JSON
//! and XML reads negotiate the same way, mark the selected range with the same `Content-Range`
//! and answer the rest of the outcome in their own formats.

use super::body::RequestBody;
use crate::{
    decimal::Grammar,
    http_header::{TextOrder, sole},
    input::InputError,
    object::ObjectRecord,
};
use hyper::{HeaderMap, Method, Request};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RangeRequest<'a> {
    Whole,
    Multiple(&'a str),
    Single(&'a str),
}

/// Classifies fields before size-dependent resolution, preserving adapter error ordering.
pub(crate) fn range_request(headers: &HeaderMap) -> RangeRequest<'_> {
    let Ok(Some(value)) = sole(headers, "range").text(TextOrder::RepetitionFirst) else {
        return RangeRequest::Whole;
    };
    let Some((unit, range)) = value.trim().split_once('=') else {
        return RangeRequest::Whole;
    };
    if !unit.eq_ignore_ascii_case("bytes") {
        return RangeRequest::Whole;
    }
    if range.contains(',') {
        RangeRequest::Multiple(value)
    } else {
        RangeRequest::Single(range)
    }
}

/// A range position. Saturation preserves comparisons with the bounded object size for
/// arbitrarily large numerals.
fn range_number(value: &str) -> Option<u64> {
    Grammar::Digits.admits(value).then(|| {
        value.bytes().fold(0_u64, |n, byte| {
            n.saturating_mul(10).saturating_add(u64::from(byte - b'0'))
        })
    })
}

impl RangeRequest<'_> {
    /// Whether this is a nonzero suffix range (`bytes=-N`) of an empty object. `resolve`
    /// ignores it, as RFC 9110 permits; the JSON API answers it with an empty `206`.
    pub(crate) fn is_suffix_of_empty(self, size: u64) -> bool {
        let Self::Single(range) = self else {
            return false;
        };
        size == 0
            && range
                .trim()
                .strip_prefix('-')
                .and_then(range_number)
                .is_some_and(|suffix| suffix != 0)
    }

    /// The `(offset, length)` the range selects, or `None` for a range the read ignores and
    /// answers with the whole object.
    pub(crate) fn resolve(self, size: u64) -> Result<Option<(u64, u64)>, UnsatisfiableRange> {
        let Self::Single(range) = self else {
            return Ok(None);
        };
        let Some((first, last)) = range.trim().split_once('-') else {
            return Ok(None);
        };
        let (offset, end) = if first.is_empty() {
            let Some(suffix) = range_number(last) else {
                return Ok(None);
            };
            // RFC 9110 permits ignoring an empty representation's satisfiable nonzero suffix.
            if size == 0 && suffix != 0 {
                return Ok(None);
            }
            (size.saturating_sub(suffix), size.saturating_sub(1))
        } else {
            let first_digits = first.trim_start_matches('0');
            let last_digits = last.trim_start_matches('0');
            if !last.is_empty()
                && (last_digits.len(), last_digits) < (first_digits.len(), first_digits)
            {
                return Ok(None);
            }
            let Some(first) = range_number(first) else {
                return Ok(None);
            };
            let last = if last.is_empty() {
                u64::MAX
            } else if let Some(last) = range_number(last) {
                last
            } else {
                return Ok(None);
            };
            (first, last.min(size.saturating_sub(1)))
        };
        if size == 0 || offset >= size {
            return Err(UnsatisfiableRange);
        }
        Ok(Some((offset, end - offset + 1)))
    }
}

/// A single range that starts at or past the end of the object, or any range of an empty
/// object other than a suffix `resolve` ignores.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the requested range cannot be satisfied")]
pub(crate) struct UnsatisfiableRange;

/// Whether the served representation of the object depends on `Accept-Encoding`: stored gzip
/// without `no-transform` is either sent compressed or gunzipped.
pub(crate) fn object_media_negotiable(object: &ObjectRecord) -> bool {
    object
        .content_encoding
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("gzip"))
        && !object.cache_control.as_deref().is_some_and(|value| {
            value
                .split(',')
                .any(|directive| directive.trim().eq_ignore_ascii_case("no-transform"))
        })
}

/// Whether a negotiable object is typed `application/gzip` as well as gzip-encoded: the doubly
/// compressed form the transcoding guide warns about. Cloud Storage gunzips it for every
/// client, one that accepts gzip included (recorded 2026-11-13).
pub(crate) fn object_double_compressed(object: &ObjectRecord) -> bool {
    object_media_negotiable(object)
        && object
            .content_type
            .split(';')
            .next()
            .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("application/gzip"))
}

/// How a media read serves the object's stored bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transcoding {
    /// The stored bytes, compressed or not.
    Stored,
    Gunzipped(Gunzip),
}

/// Why a media read gunzips the stored bytes; each reason sends its own headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gunzip {
    /// The client does not accept gzip.
    NotAccepted,
    /// A doubly compressed object, which a client that accepts gzip receives gunzipped too.
    DespiteAccept,
}

/// Select gzip expansion while honoring explicit quality overrides and no-transform.
pub(crate) fn object_media_transcoding(
    headers: &HeaderMap,
    object: &ObjectRecord,
) -> Result<Transcoding, InputError> {
    if !object_media_negotiable(object) {
        return Ok(Transcoding::Stored);
    }
    Ok(
        match (accepts_gzip(headers)?, object_double_compressed(object)) {
            (false, _) => Transcoding::Gunzipped(Gunzip::NotAccepted),
            (true, true) => Transcoding::Gunzipped(Gunzip::DespiteAccept),
            (true, false) => Transcoding::Stored,
        },
    )
}

/// Whether `Accept-Encoding` admits gzip, directly or through `*`.
fn accepts_gzip(headers: &HeaderMap) -> Result<bool, InputError> {
    let mut gzip = None;
    let mut wildcard = None;
    for header in headers.get_all("accept-encoding") {
        let value = header
            .to_str()
            .map_err(|_| InputError::parameter("Invalid Accept-Encoding"))?;
        for entry in value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let mut parts = entry.split(';');
            // `split` always yields at least one item, so the default is never used.
            let encoding = parts.next().unwrap_or_default().trim();
            let allowed = if let Some(parameter) = parts.next() {
                let (name, quality) = parameter
                    .trim()
                    .split_once('=')
                    .ok_or_else(|| InputError::parameter("Invalid Accept-Encoding quality"))?;
                if !name.eq_ignore_ascii_case("q") || parts.next().is_some() {
                    return Err(InputError::parameter("Invalid Accept-Encoding parameter"));
                }
                let quality = quality.trim();
                let (whole, fraction) = quality.split_once('.').unwrap_or((quality, ""));
                if !matches!(whole, "0" | "1")
                    || fraction.len() > 3
                    || !fraction.bytes().all(|byte| byte.is_ascii_digit())
                    || (whole == "1" && fraction.bytes().any(|byte| byte != b'0'))
                {
                    return Err(InputError::parameter("Invalid Accept-Encoding quality"));
                }
                whole == "1" || fraction.bytes().any(|byte| byte != b'0')
            } else {
                true
            };
            let selected = if encoding.eq_ignore_ascii_case("gzip") {
                &mut gzip
            } else if encoding == "*" {
                &mut wildcard
            } else {
                continue;
            };
            if selected.replace(allowed).is_some() {
                return Err(InputError::parameter("Duplicate Accept-Encoding token"));
            }
        }
    }
    Ok(gzip.or(wildcard).unwrap_or(false))
}

/// What a download should fetch while it opens the object.
pub(crate) fn body_read_ahead(request: &Request<RequestBody>) -> crate::object::ReadAhead {
    use crate::object::ReadAhead;
    const WITHOUT_FULL_BODY: [&str; 5] = [
        "range",
        "if-match",
        "if-none-match",
        "if-modified-since",
        "if-unmodified-since",
    ];
    let headers = request.headers();
    if request.method() == Method::GET
        && !WITHOUT_FULL_BODY
            .iter()
            .any(|name| headers.contains_key(*name))
    {
        ReadAhead::Body
    } else {
        ReadAhead::None
    }
}

/// Insert `Content-Range` for a satisfiable range. Decimal positions always form a valid header
/// value, so the conversion cannot fail in practice.
pub(crate) fn insert_content_range(
    headers: &mut hyper::HeaderMap,
    offset: u64,
    length: u64,
    size: u64,
) {
    if let Ok(value) = hyper::header::HeaderValue::try_from(format!(
        "bytes {}-{}/{}",
        offset,
        offset + length - 1,
        size
    )) {
        headers.insert("content-range", value);
    }
}

/// Insert `Content-Range` for an unsatisfiable range request.
pub(crate) fn insert_unsatisfied_content_range(headers: &mut hyper::HeaderMap, size: u64) {
    if let Ok(value) = hyper::header::HeaderValue::try_from(format!("bytes */{size}")) {
        headers.insert("content-range", value);
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let error = UnsatisfiableRange;
    assert_eq!(error.to_string(), "the requested range cannot be satisfied");
    assert!(std::error::Error::source(&error).is_none());
}
