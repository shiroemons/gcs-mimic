//! Upload request headers that the JSON and XML adapters read the same way: the `Content-Type`
//! an upload stores and the digests `X-Goog-Hash` declares. Each caller keeps its own rule for
//! a repeated digest and its own wording, which recorded answers and tests pin, so this module
//! only reads the declarations.

use super::{Checksum, ExpectedHashes, HashAlgorithm};
use crate::api_error::ApiError;
use crate::hashing::decode_hash;
use hyper::HeaderMap;

/// The `Content-Type` an upload stores: empty when the header is absent (recorded: the resource
/// omits `contentType`), otherwise a single visible-ASCII value whose media type is
/// `type/subtype` made of token characters.
pub(crate) fn upload_content_type(headers: &HeaderMap) -> Result<String, ApiError> {
    if headers.get_all("content-type").iter().count() > 1 {
        return Err(ApiError::invalid_parameter("Duplicate Content-Type header"));
    }
    let content_type = headers
        .get("content-type")
        .map(|value| {
            value
                .to_str()
                .map_err(|_| ApiError::invalid_parameter("Invalid Content-Type header"))
        })
        .transpose()?
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
        return Err(ApiError::invalid_parameter("Invalid Content-Type header"));
    }
    Ok(content_type.to_owned())
}

/// The hashes an XML PUT declares with `X-Goog-Hash`. The JSON upload routes do not read the
/// header this way: recorded media and multipart uploads and resumable initiations ignored it,
/// and only the requests on a resumable session URI declare hashes with it.
pub(crate) fn declared_hashes(headers: &HeaderMap) -> Result<ExpectedHashes, ApiError> {
    let mut hashes = ExpectedHashes::default();
    // A repeated algorithm is refused even with an equal value, before it is decoded.
    each_declared_hash(headers, "Invalid x-goog-hash header", |algorithm, value| {
        if hashes.declares(algorithm) {
            return Err(ApiError::invalid_parameter(
                "Duplicate x-goog-hash algorithm",
            ));
        }
        hashes.set(decode(algorithm, value)?);
        Ok(())
    })?;
    Ok(hashes)
}

/// Decodes a base64 digest of the algorithm's width.
pub(crate) fn decode(algorithm: HashAlgorithm, text: &str) -> Result<Checksum, ApiError> {
    Ok(match algorithm {
        HashAlgorithm::Md5 => Checksum::Md5(decode_hash(text)?),
        HashAlgorithm::Crc32c => Checksum::Crc32c(u32::from_be_bytes(decode_hash(text)?)),
    })
}

/// Passes each `algorithm=value` entry of the `X-Goog-Hash` headers, in order, to `declare`. A
/// header that is not visible ASCII, or an entry without `=`, fails with `malformed`; an
/// algorithm other than `md5` and `crc32c` is unsupported.
pub(crate) fn each_declared_hash(
    headers: &HeaderMap,
    malformed: &'static str,
    mut declare: impl FnMut(HashAlgorithm, &str) -> Result<(), ApiError>,
) -> Result<(), ApiError> {
    for header in headers.get_all("x-goog-hash") {
        let header = header
            .to_str()
            .map_err(|_| ApiError::invalid_parameter(malformed))?;
        for entry in header.split(',') {
            let (token, value) = entry
                .trim()
                .split_once('=')
                .ok_or_else(|| ApiError::invalid_parameter(malformed))?;
            let algorithm = match token {
                "md5" => HashAlgorithm::Md5,
                "crc32c" => HashAlgorithm::Crc32c,
                _ => return Err(ApiError::unsupported_object("x-goog-hash algorithm")),
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
        assert_eq!(upload_content_type(&HeaderMap::new()).unwrap(), "");
    }

    #[test]
    fn a_content_type_keeps_its_parameters() {
        let headers = header_map([("content-type", "text/plain; charset=utf-8")]);
        assert_eq!(
            upload_content_type(&headers).unwrap(),
            "text/plain; charset=utf-8"
        );
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
            assert!(upload_content_type(&headers).is_err(), "{value}");
        }
        let headers = header_map([
            ("content-type", "text/plain"),
            ("content-type", "text/html"),
        ]);
        assert_eq!(
            upload_content_type(&headers).unwrap_err().status,
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
                declared_hashes(&headers).unwrap_err().status,
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
