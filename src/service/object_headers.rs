//! Request header rules of the JSON API's object routes, with the conditional header check that
//! every JSON route applies. The resumable engine applies the upload rules to every request on a
//! session URI, the XML API's included, so they live below the adapters.

use super::ServiceError;
use crate::{
    decimal::Grammar,
    http_header::{TextError, TextOrder, sole},
    input::InputError,
    object::{IdempotencyToken, media_headers::CUSTOM_METADATA_PREFIX},
};
use hyper::HeaderMap;

/// The one text value of `name`, if the request sends it.
pub(crate) fn single<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> Result<Option<&'a str>, InputError> {
    sole(headers, name)
        .text(TextOrder::ValueFirst)
        .map_err(|error| match error {
            TextError::Invalid(_) => InputError::parameter(format!("Invalid {name}")),
            TextError::Repeated(_) => InputError::parameter(format!("Duplicate {name}")),
        })
}

pub(crate) fn unsigned(value: &str) -> Result<u64, InputError> {
    if !Grammar::Digits.admits(value) {
        return Err(InputError::parameter(
            "Expected an unsigned decimal integer",
        ));
    }
    value
        .parse()
        .map_err(|_| InputError::parameter("Upload integer overflow"))
}

/// Rejects the headers an object route does not support, explicitly rather than ignoring them.
/// `keys` lists the customer-supplied key header prefixes the caller parses itself.
pub(crate) fn validate_keyed_object_headers(
    headers: &HeaderMap,
    keys: &[&str],
) -> Result<(), InputError> {
    IdempotencyToken::validate_opaque(headers).map_err(|_| {
        InputError::parameter("Expected a single non-empty x-goog-gcs-idempotency-token header")
    })?;
    for key in headers.keys() {
        let name = key.as_str();
        if keys
            .iter()
            .any(|prefix| crate::csek::is_key_header(name, prefix))
        {
            continue;
        }
        if name.starts_with(crate::csek::DESTINATION_PREFIX)
            || name.starts_with(crate::csek::SOURCE_PREFIX)
            || name.starts_with(CUSTOM_METADATA_PREFIX)
            || matches!(name, "x-goog-user-project" | "x-http-method-override")
        {
            return Err(InputError::unsupported_object(name));
        }
    }
    Ok(())
}

/// The `Content-Type` an upload stores, checked with the rest of its headers without consuming
/// or collecting the body. Callers read the destination customer-supplied key separately with
/// [`crate::csek::request_key`].
pub(crate) fn upload_content_type(headers: &HeaderMap) -> Result<String, ServiceError> {
    validate_keyed_object_headers(headers, &[crate::csek::DESTINATION_PREFIX])?;
    validate_read_condition_headers(headers, EtagConditions::Unsupported)?;
    if let Some(mime) = single(headers, "x-upload-content-type")? {
        crate::object::validate_fixed_metadata("contentType", mime)
            .map_err(crate::object::ObjectError::from)?;
        let mut advisory = HeaderMap::new();
        advisory.insert(
            "content-type",
            mime.parse()
                .map_err(|_| InputError::parameter("Invalid upload content type"))?,
        );
        crate::object::upload_headers::content_type(&advisory)?;
    }
    // `Content-MD5` is not examined: Cloud Storage stored a JSON upload whose header named the
    // wrong digest (recorded 200 with the computed hashes).
    for name in [
        "content-encoding",
        "content-range",
        "x-upload-content-length",
    ] {
        if headers.contains_key(name) {
            return Err(InputError::unsupported_object(name).into());
        }
    }
    // `X-Goog-Hash` is not examined either. Recorded JSON media uploads that named a wrong,
    // malformed or unknown-algorithm hash all stored the computed hashes with 200, so only a
    // multipart metadata `crc32c`/`md5Hash` is verified on this route.
    crate::object::upload_headers::content_type(headers).map_err(Into::into)
}

/// Whether a route evaluates `If-Match` and `If-None-Match` against an ETag it serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EtagConditions {
    Supported,
    /// A JSON media read: the entity tags are evaluated, while `If-Modified-Since`,
    /// `If-Unmodified-Since` and `If-Range` are ignored as Cloud Storage ignored them.
    MediaRead,
    MatchOnly,
    /// Accepted and not evaluated, matching an observed Cloud Storage write that ignores both.
    Ignored,
    Unsupported,
}

/// Keep unsupported conditional headers explicit on routes without validators.
pub(crate) fn validate_read_condition_headers(
    headers: &HeaderMap,
    etags: EtagConditions,
) -> Result<(), InputError> {
    if matches!(etags, EtagConditions::Supported | EtagConditions::MatchOnly) {
        for name in ["if-range", "if-modified-since", "if-unmodified-since"] {
            if (name == "if-modified-since" && headers.contains_key("if-none-match"))
                || (name == "if-unmodified-since" && headers.contains_key("if-match"))
            {
                continue;
            }
            if headers.contains_key(name) {
                return Err(InputError::unsupported_object(name));
            }
        }
    }
    for name in ["if-match", "if-none-match"] {
        if headers.contains_key(name)
            && (etags == EtagConditions::Unsupported
                || (etags == EtagConditions::MatchOnly && name == "if-none-match"))
        {
            return Err(InputError::unsupported_object(name));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_text_first_resumable_header_wins_over_repetition() {
        let mut headers = HeaderMap::new();
        headers.append(
            "content-range",
            hyper::header::HeaderValue::from_bytes(&[0xff]).unwrap(),
        );
        headers.append(
            "content-range",
            hyper::header::HeaderValue::from_static("bytes */0"),
        );
        assert_eq!(
            single(&headers, "content-range").unwrap_err().message,
            "Invalid content-range"
        );
    }
}
