//! The headers of a request on a session URI: its `Content-Range`, and the custom metadata and
//! declared hashes a chunk adds to the session.

use crate::service::{
    ServiceError,
    object_headers::{single, upload_content_type},
};
use crate::{
    decimal::Grammar,
    http_header::TextOrder,
    input::InputError,
    object::{
        ChecksumConflict, check_custom_metadata_size,
        upload_headers::{self, HashHeader},
    },
    storage::{ResumableError, ResumableOptions},
};
use hyper::HeaderMap;

pub(crate) fn final_options(
    headers: &HeaderMap,
    mut options: ResumableOptions,
) -> Result<ResumableOptions, ServiceError> {
    let mut clean = headers.clone();
    clean.remove("content-range");
    clean.remove("x-goog-hash");
    let mut custom = std::collections::BTreeMap::new();
    for field in upload_headers::custom_metadata_headers(headers, TextOrder::ValueFirst) {
        if field.name.as_str().starts_with("x-goog-upload-") {
            return Err(InputError::unsupported_object(field.name.as_str()).into());
        }
        if let Some((key, value)) = field
            .metadata
            .transpose()
            .map_err(ServiceError::SessionHeader)?
        {
            custom.insert(key.to_owned(), Some(value.to_owned()));
            clean.remove(field.name);
        }
    }
    upload_content_type(&clean)?;
    // An equal value agrees with the metadata's or an earlier chunk's declaration; a different
    // one conflicts.
    upload_headers::each_declared_hash(headers, HashHeader::Session, |algorithm, value| {
        options
            .expected
            .agree(upload_headers::decode(algorithm, value)?)
            .map_err(|ChecksumConflict(algorithm)| {
                upload_headers::UploadHeaderError::Conflicting(algorithm)
            })
    })
    .map_err(ServiceError::SessionHeader)?;
    check_custom_metadata_size(&custom)?;
    for (key, value) in custom {
        options.metadata.metadata.merge_entry(key, value);
    }
    Ok(options)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Range {
    Status(Option<u64>),
    Data {
        start: u64,
        end: u64,
        total: Option<u64>,
    },
    /// `bytes start-*/total`: the request body carries the rest of the object from `start`,
    /// so the end of the body is the end of the object. The Node.js client sends this form
    /// (`bytes 0-*/*` by default) for single-request resumable uploads.
    Open {
        start: u64,
        total: Option<u64>,
    },
}

/// A `Content-Range` Cloud Storage refused: every malformed or inconsistent one got the same
/// bare text page.
pub(crate) fn range(headers: &HeaderMap) -> Result<Option<Range>, ResumableError> {
    parse_range(headers).map_err(|_| ResumableError::InvalidContentRange)
}

enum RangeError {
    Syntax,
    /// A chunk whose last byte lies at or past the total it declares.
    Overrun,
    Semantic,
}

/// Whether the `Content-Range` fails the whole JSON session, not just this request: malformed
/// syntax, and a chunk that overruns its own total (recorded 2026-11-13: every later request on
/// the session got the same text page and no object was created).
pub(crate) fn range_fails_session(headers: &HeaderMap) -> bool {
    matches!(
        parse_range(headers),
        Err(RangeError::Syntax | RangeError::Overrun)
    )
}

fn range_unsigned(value: &str) -> Result<u64, RangeError> {
    if !Grammar::Digits.admits(value) {
        return Err(RangeError::Syntax);
    }
    value.parse().map_err(|_| RangeError::Semantic)
}

fn parse_range(headers: &HeaderMap) -> Result<Option<Range>, RangeError> {
    single(headers, "content-range")
        .map_err(|_| RangeError::Semantic)?
        .map(|value| {
            let value = value.strip_prefix("bytes ").ok_or(RangeError::Syntax)?;
            let (range, total) = value.split_once('/').ok_or(RangeError::Syntax)?;
            let total = if total == "*" {
                None
            } else {
                Some(range_unsigned(total)?)
            };
            if range == "*" {
                return Ok(Range::Status(total));
            }
            let (start, end) = range.split_once('-').ok_or(RangeError::Syntax)?;
            if end == "*" {
                let start = range_unsigned(start)?;
                if total.is_some_and(|n| start > n) {
                    return Err(RangeError::Semantic);
                }
                return Ok(Range::Open { start, total });
            }
            let (start, end) = (range_unsigned(start)?, range_unsigned(end)?);
            if end < start || end == u64::MAX {
                return Err(RangeError::Semantic);
            }
            if total.is_some_and(|n| end >= n) {
                return Err(RangeError::Overrun);
            }
            Ok(Range::Data { start, end, total })
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bucket::BucketAccess, json::ApiError};
    use hyper::header::HeaderValue;

    #[test]
    fn final_metadata_errors_follow_the_request_header_order() {
        let invalid_metadata = ("x-goog-meta-key", HeaderValue::from_bytes(&[0xff]).unwrap());
        let unsupported = ("x-goog-upload-example", HeaderValue::from_static("ignored"));
        let invalid = ApiError::invalid_parameter("Invalid x-goog-meta-key");
        let unsupported_error = ApiError::unsupported_object(unsupported.0);
        for (fields, expected) in [
            ([invalid_metadata.clone(), unsupported.clone()], invalid),
            ([unsupported, invalid_metadata], unsupported_error),
        ] {
            let mut headers = HeaderMap::new();
            for (name, value) in fields {
                headers.insert(name, value);
            }
            let options = ResumableOptions::new(BucketAccess::Permissive);
            assert_eq!(
                ApiError::from(final_options(&headers, options).unwrap_err()),
                expected
            );
        }
    }
}
