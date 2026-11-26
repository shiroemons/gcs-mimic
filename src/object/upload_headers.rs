//! Upload request headers that the JSON and XML adapters read the same way: the `Content-Type`
//! an upload stores and the digests `X-Goog-Hash` declares. Each caller keeps its own rule for
//! a repeated digest and its own wording, which recorded answers and tests pin, so this module
//! only reads the declarations.

use super::{Checksum, ExpectedHashes, HashAlgorithm};
use crate::hashing::{InvalidDigest, decode_hash};
use crate::http_header::{TextError, TextOrder, sole};
use hyper::HeaderMap;

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
