//! Upload request headers that the JSON and XML adapters read the same way: the `Content-Type`
//! an upload stores, custom metadata and the digests `X-Goog-Hash` declares. The adapters keep
//! their rules for repeated values and their error wording.

use super::{Checksum, ExpectedHashes, HashAlgorithm, media_headers::CUSTOM_METADATA_PREFIX};
use crate::hashing::{InvalidDigest, decode_hash};
use crate::http_header::{TextError, TextOrder, sole};
use hyper::{HeaderMap, header::HeaderName};

#[derive(Clone, Copy, Debug)]
pub(crate) enum HashHeader {
    Put,
    Session,
}

impl HashHeader {
    fn malformed_message(self) -> &'static str {
        match self {
            Self::Put => "Invalid x-goog-hash header",
            Self::Session => "Invalid x-goog-hash",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum UploadHeaderError {
    #[error("{kind} {name}", kind = match .source {
        TextError::Repeated(_) => "Duplicate",
        TextError::Invalid(_) => "Invalid",
    })]
    CustomMetadata {
        name: HeaderName,
        #[source]
        source: TextError,
    },
    #[error("Duplicate Content-Type header")]
    DuplicateContentType,
    #[error("Invalid Content-Type header")]
    InvalidContentType,
    #[error("{}", .0.malformed_message())]
    MalformedHash(HashHeader),
    #[error("Duplicate x-goog-hash algorithm")]
    DuplicateAlgorithm,
    #[error(
        "{}",
        crate::input::unsupported_object_setting("x-goog-hash algorithm")
    )]
    UnsupportedAlgorithm,
    #[error(transparent)]
    Digest(#[from] InvalidDigest),
    #[error("Conflicting {} declarations", .0.name())]
    Conflicting(HashAlgorithm),
}

pub(crate) struct CustomMetadataHeader<'a> {
    pub(crate) name: &'a HeaderName,
    pub(crate) metadata: Option<Result<(&'a str, &'a str), UploadHeaderError>>,
}

/// Visits the original header order so callers can interleave their other header refusals.
/// A bare prefix names the empty key (recorded 2026-11-17). XML forms have their own grammar.
pub(crate) fn custom_metadata_headers(
    headers: &HeaderMap,
    order: TextOrder,
) -> impl Iterator<Item = CustomMetadataHeader<'_>> {
    headers.keys().map(move |name| {
        let metadata = name
            .as_str()
            .strip_prefix(CUSTOM_METADATA_PREFIX)
            .and_then(|key| {
                sole(headers, name)
                    .text(order)
                    .map_err(|source| UploadHeaderError::CustomMetadata {
                        name: name.clone(),
                        source,
                    })
                    .transpose()
                    .map(|value| value.map(|value| (key, value)))
            });
        CustomMetadataHeader { name, metadata }
    })
}

/// The `Content-Type` an upload stores: empty when the header is absent (recorded: the resource
/// omits `contentType`), otherwise a single visible-ASCII value whose media type is
/// `type/subtype` made of token characters.
pub(crate) fn content_type(headers: &HeaderMap) -> Result<String, UploadHeaderError> {
    let content_type = sole(headers, "content-type")
        .text(TextOrder::RepetitionFirst)
        .map_err(|error| match error {
            TextError::Repeated(_) => UploadHeaderError::DuplicateContentType,
            TextError::Invalid(_) => UploadHeaderError::InvalidContentType,
        })?
        .unwrap_or_default();
    let mime = content_type.split(';').next().unwrap_or_default().trim();
    let token = |value: &str| {
        !value.is_empty()
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
    };
    if headers.contains_key("content-type")
        && !mime
            .split_once('/')
            .is_some_and(|(kind, subtype)| token(kind) && token(subtype))
    {
        return Err(UploadHeaderError::InvalidContentType);
    }
    Ok(content_type.to_owned())
}

/// The hashes an XML PUT declares with `X-Goog-Hash`. The JSON upload routes do not read the
/// header this way: recorded media and multipart uploads and resumable initiations ignored it,
/// and only the requests on a resumable session URI declare hashes with it.
pub(crate) fn declared_hashes(headers: &HeaderMap) -> Result<ExpectedHashes, UploadHeaderError> {
    let mut hashes = ExpectedHashes::default();
    // A repeated algorithm is refused even with an equal value, before it is decoded.
    each_declared_hash(headers, HashHeader::Put, |algorithm, value| {
        if hashes.declares(algorithm) {
            return Err(UploadHeaderError::DuplicateAlgorithm);
        }
        hashes.set(decode(algorithm, value)?);
        Ok(())
    })?;
    Ok(hashes)
}

/// Decodes a base64 digest of the algorithm's width.
pub(crate) fn decode(algorithm: HashAlgorithm, text: &str) -> Result<Checksum, UploadHeaderError> {
    Ok(match algorithm {
        HashAlgorithm::Md5 => Checksum::Md5(decode_hash(text)?),
        HashAlgorithm::Crc32c => Checksum::Crc32c(u32::from_be_bytes(decode_hash(text)?)),
    })
}

/// Passes each `algorithm=value` entry to `declare` in header order. The upload route determines
/// the error message for malformed entries. Both routes accept only `md5` and `crc32c`.
pub(crate) fn each_declared_hash(
    headers: &HeaderMap,
    route: HashHeader,
    mut declare: impl FnMut(HashAlgorithm, &str) -> Result<(), UploadHeaderError>,
) -> Result<(), UploadHeaderError> {
    for header in headers.get_all("x-goog-hash") {
        let header = header
            .to_str()
            .map_err(|_| UploadHeaderError::MalformedHash(route))?;
        for entry in header.split(',') {
            let (token, value) = entry
                .trim()
                .split_once('=')
                .ok_or(UploadHeaderError::MalformedHash(route))?;
            let algorithm = match token {
                "md5" => HashAlgorithm::Md5,
                "crc32c" => HashAlgorithm::Crc32c,
                _ => return Err(UploadHeaderError::UnsupportedAlgorithm),
            };
            declare(algorithm, value)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::{StatusCode, header::HeaderName};

    fn header_map<'a>(entries: impl IntoIterator<Item = (&'static str, &'a str)>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in entries {
            headers.append(HeaderName::from_static(name), value.parse().unwrap());
        }
        headers
    }

    #[test]
    fn custom_metadata_preserves_empty_keys_values_and_original_text() {
        let headers = header_map([
            ("x-goog-meta-", ""),
            ("content-type", "text/plain"),
            ("x-goog-meta-value", " Mixed Case "),
        ]);
        let fields = custom_metadata_headers(&headers, TextOrder::ValueFirst)
            .filter_map(|field| field.metadata)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(fields, [("", ""), ("value", " Mixed Case ")]);
    }

    #[test]
    fn custom_metadata_keeps_the_selected_header_error_precedence() {
        let mut headers = HeaderMap::new();
        headers.append(
            "x-goog-meta-key",
            hyper::header::HeaderValue::from_bytes(&[0xff]).unwrap(),
        );
        headers.append("x-goog-meta-key", "second".parse().unwrap());
        for (order, message) in [
            (TextOrder::ValueFirst, "Invalid x-goog-meta-key"),
            (TextOrder::RepetitionFirst, "Duplicate x-goog-meta-key"),
        ] {
            let field = custom_metadata_headers(&headers, order).next().unwrap();
            let error = field.metadata.unwrap().unwrap_err();
            assert_eq!(error.to_string(), message);
            assert!(std::error::Error::source(&error).is_some());
        }
    }

    #[test]
    fn an_absent_content_type_stores_none() {
        assert_eq!(content_type(&HeaderMap::new()).unwrap(), "");
    }

    #[test]
    fn a_content_type_keeps_its_parameters() {
        let headers = header_map([("content-type", "text/plain; charset=utf-8")]);
        assert_eq!(content_type(&headers).unwrap(), "text/plain; charset=utf-8");
    }

    #[test]
    fn malformed_or_repeated_content_types_are_refused() {
        for value in [
            "",
            "text",
            "text/",
            "/plain",
            "text/a/b",
            "text/plain, text/html",
        ] {
            let headers = header_map([("content-type", value)]);
            assert!(content_type(&headers).is_err(), "{value}");
        }
        let headers = header_map([
            ("content-type", "text/plain"),
            ("content-type", "text/html"),
        ]);
        assert_eq!(
            crate::api_error::ApiError::from(content_type(&headers).unwrap_err()).status,
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn digests_are_declared_in_one_header_or_several() {
        for combined in [true, false] {
            let headers = if combined {
                header_map([(
                    "x-goog-hash",
                    "crc32c=AQIDBA==, md5=AAAAAAAAAAAAAAAAAAAAAA==",
                )])
            } else {
                header_map([
                    ("x-goog-hash", "crc32c=AQIDBA=="),
                    ("x-goog-hash", "md5=AAAAAAAAAAAAAAAAAAAAAA=="),
                ])
            };
            assert_eq!(
                declared_hashes(&headers).unwrap(),
                ExpectedHashes {
                    md5: Some([0; 16]),
                    crc32c: Some(0x01020304)
                },
                "combined: {combined}"
            );
        }
        let headers = header_map([("x-goog-hash", "md5=1B2M2Y8AsgTpgAmY7PhCfg==")]);
        assert_eq!(
            declared_hashes(&headers).unwrap().md5.unwrap(),
            [
                0xd4, 0x1d, 0x8c, 0xd9, 0x8f, 0x00, 0xb2, 0x04, 0xe9, 0x80, 0x09, 0x98, 0xec, 0xf8,
                0x42, 0x7e
            ]
        );
    }

    #[test]
    fn invalid_or_repeated_digests_are_refused() {
        for value in [
            "",
            "crc32c",
            "crc32c=",
            "crc32c=AQIDBA",
            "crc32c=AQIDBA===",
            "crc32c=AQIDB!==",
            "crc32c=AQIDBB==",
            "crc32c=AA==AA==",
            "crc32c=AQIDBA==,",
            "crc32c=AQIDBA==,crc32c=AQIDBA==",
            "md5=AQIDBA==",
            "md5=AAAAAAAAAAAAAAAAAAAAAB==",
        ] {
            let headers = header_map([("x-goog-hash", value)]);
            assert_eq!(
                crate::api_error::ApiError::from(declared_hashes(&headers).unwrap_err()).status,
                StatusCode::BAD_REQUEST,
                "{value}"
            );
        }
        let headers = header_map([
            ("x-goog-hash", "crc32c=AQIDBA=="),
            ("x-goog-hash", "crc32c=AQIDBA=="),
        ]);
        assert!(declared_hashes(&headers).is_err());
    }
}
