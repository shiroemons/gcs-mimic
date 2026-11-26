//! Representation headers of an object media download, shared by the JSON and XML adapters.

use super::{ObjectRecord, content_type};
use crate::base64::encode as base64;
use crate::etag::object_etag;
use hyper::{HeaderMap, header::HeaderValue};

#[derive(Debug, thiserror::Error)]
#[error("{}", Self::MESSAGE)]
pub(crate) struct StoredHeaderError(#[from] hyper::header::InvalidHeaderValue);

impl StoredHeaderError {
    pub(crate) const MESSAGE: &'static str = "Invalid stored object response header";
}

/// The `Cache-Control` Cloud Storage sends with JSON API errors and media downloads.
pub(crate) const NO_STORE_CACHE_CONTROL: &str = "no-cache, no-store, max-age=0, must-revalidate";

/// Wire prefix for custom metadata on uploads and XML media responses.
pub(crate) const CUSTOM_METADATA_PREFIX: &str = "x-goog-meta-";

/// Representation headers for an untranscoded media download.
pub(crate) fn object_media_headers(object: &ObjectRecord) -> Result<HeaderMap, StoredHeaderError> {
    let mut headers = HeaderMap::new();
    for (key, value) in [
        // The transport substitutes stored/default cache metadata only for an allUsers read grant.
        ("cache-control", NO_STORE_CACHE_CONTROL.to_owned()),
        (
            "content-type",
            content_type::or_default(object.content_type.clone()),
        ),
        ("content-length", object.size.to_string()),
        ("x-goog-generation", object.generation.to_string()),
        ("x-goog-metageneration", object.metageneration.to_string()),
        (
            "x-goog-hash",
            if object.has_md5() {
                format!(
                    "crc32c={},md5={}",
                    base64(&object.crc32c.to_be_bytes()),
                    base64(&object.md5)
                )
            } else {
                format!("crc32c={}", base64(&object.crc32c.to_be_bytes()))
            },
        ),
        ("x-goog-stored-content-length", object.size.to_string()),
        (
            "x-goog-stored-content-encoding",
            object
                .content_encoding
                .as_deref()
                .unwrap_or("identity")
                .to_owned(),
        ),
        ("etag", object_etag(object)),
    ] {
        headers.insert(key, HeaderValue::from_str(&value)?);
    }
    for (key, value) in [
        ("content-encoding", &object.content_encoding),
        ("content-disposition", &object.content_disposition),
        ("content-language", &object.content_language),
    ] {
        if let Some(value) = value {
            headers.insert(key, HeaderValue::from_str(value)?);
        }
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::etag::object_etag;
    use hyper::StatusCode;

    #[test]
    fn obj_render_002_media_headers_match_selected_generation_and_hashes() {
        let object = crate::json::object_tests::record();
        let headers = object_media_headers(&object).unwrap();
        for (name, value) in [
            ("content-type", "text/plain; charset=utf-8"),
            ("content-length", "5"),
            ("x-goog-generation", "9007199254740993"),
            ("x-goog-metageneration", "2"),
            (
                "x-goog-hash",
                "crc32c=AQIDBA==,md5=AAAAAAAAAAAAAAAAAAAAAA==",
            ),
            ("x-goog-stored-content-length", "5"),
            ("x-goog-stored-content-encoding", "identity"),
            ("etag", "CIGAgICAgIAQEAI="),
        ] {
            assert_eq!(headers[name], value);
        }
        // Cloud Storage sends no accept-ranges on a JSON media response (observed 2026-10-31).
        assert!(!headers.contains_key("accept-ranges"));
        assert_eq!(
            headers["cache-control"],
            "no-cache, no-store, max-age=0, must-revalidate"
        );
        assert_eq!(headers.len(), 9);
        let mut object = object;
        object.size = u64::MAX;
        assert_eq!(
            object_media_headers(&object).unwrap()["content-length"],
            u64::MAX.to_string()
        );
    }

    #[test]
    fn invalid_stored_media_headers_keep_the_header_error_as_their_source() {
        let mut object = crate::storage::test_fixtures::bare_object_record();
        object.content_type = "text/plain\r\nx-injected: yes".to_owned();
        let error = object_media_headers(&object).unwrap_err();
        let source = std::error::Error::source(&error).expect("stored header error has a source");
        assert!(source.is::<hyper::header::InvalidHeaderValue>());
        assert_eq!(
            crate::ApiError::from(error).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn an_object_without_md5_declares_only_crc32c() {
        let mut object = crate::json::object_tests::record();
        object.md5_availability = crate::object::Md5Availability::Unavailable;
        let headers = object_media_headers(&object).unwrap();
        assert_eq!(headers["x-goog-hash"], "crc32c=AQIDBA==");
        assert!(!object_etag(&object).contains(&base64(&object.md5)));
    }
}
