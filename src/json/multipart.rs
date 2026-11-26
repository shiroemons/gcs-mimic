//! The JSON API's multipart/related upload: a JSON metadata part, a media part and an optional
//! JSON checksum part, read through the protocol-neutral [`crate::multipart`] parser.

use super::{ApiError, ObjectMetadataRequest, checksum, object_metadata_from_map};
use crate::multipart::{
    Boundary, MediaType, Multipart, ParamQuotes, Parameter, invalid, part_content_type,
};
use crate::object::{ChecksumConflict, ExpectedHashes, ObjectMetadataUpdate, Patch};
use bytes::Bytes;
use hyper::{StatusCode, body::Body};

const JSON_CAP: usize = crate::bounded_body::RESOURCE_BODY_LIMIT;
/// What a multipart upload may send besides its media: metadata and checksums at
/// `JSON_CAP` each, and headers, delimiter padding and discarded text, which stay under 256 KiB.
pub(crate) const FRAMING_LIMIT: u64 = 2 * JSON_CAP as u64 + 256 * 1024;

/// A JSON part of a multipart upload, collected whole up to `JSON_CAP`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JsonPart {
    Metadata,
    Checksums,
}

impl JsonPart {
    fn too_large(self) -> ApiError {
        let message = match self {
            Self::Metadata => format!(
                "Multipart metadata exceeds {}",
                crate::bounded_body::RESOURCE_BODY_LIMIT_TEXT
            ),
            Self::Checksums => format!(
                "Multipart checksums exceed {}",
                crate::bounded_body::RESOURCE_BODY_LIMIT_TEXT
            ),
        };
        ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "uploadTooLarge", message)
    }
}

/// Requires a metadata or checksum part to be `application/json`, at most with a UTF-8 charset.
fn json_part_type(value: Option<&str>) -> Result<(), ApiError> {
    let media = MediaType::parse(value.ok_or_else(invalid)?, ParamQuotes::Double)?;
    if media.essence != "application/json"
        || media.parameters.iter().any(|Parameter { name, value }| {
            name != "charset" || !value.eq_ignore_ascii_case("utf-8")
        })
    {
        return Err(invalid().into());
    }
    Ok(())
}

/// The media part and the metadata must agree on the type. The message is Cloud Storage's,
/// which spells out where each upload type puts its type.
pub(crate) fn check_declared_content_type(declared: &str, part: &str) -> Result<(), ApiError> {
    let essence = |value: &str| {
        value
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
    };
    if essence(declared) == essence(part) {
        return Ok(());
    }
    Err(ApiError::invalid(format!(
        "Content-Type specified in the upload ({part}) does not match Content-Type specified in metadata ({declared}). If it was a simple upload (uploadType=media), the Content-Type was specified as a bare header. If it was a multipart upload (uploadType=multipart), then the Content-Type was specified in the second part of the multipart. If it was a resumable upload (uploadType=resumable), then the Content-Type was specified with the X-Upload-Content-Type header with the start of the resumable session. For more information, see https://cloud.google.com/storage/docs/json_api/v1/how-tos/upload."
    ))
    .html_typed())
}

/// Which part of a multipart upload a header block introduces. The metadata part must be
/// `application/json`; the media part may declare any valid content type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PartKind {
    Metadata,
    Media,
}

/// A JSON multipart/related upload body, framed by CRLF or, throughout, by bare LF: the
/// metadata part, the media part and an optional checksum part, in that order.
pub(crate) struct RelatedUpload<B> {
    parser: Multipart<B>,
    /// The metadata part's `Content-Type`, named when a third part's type is refused.
    metadata_type: Option<String>,
}

impl<B: Body<Data = Bytes> + Unpin> RelatedUpload<B> {
    pub(crate) fn new(body: B, boundary: &Boundary) -> Self {
        Self::over(Multipart::related(body, boundary))
    }

    fn over(parser: Multipart<B>) -> Self {
        Self {
            parser,
            metadata_type: None,
        }
    }

    pub(crate) async fn headers(&mut self, part: PartKind) -> Result<Option<String>, ApiError> {
        match part {
            PartKind::Metadata => {
                let headers = self.parser.part_headers().await?;
                let value = part_content_type(&headers)?;
                json_part_type(value.as_deref())?;
                Ok(value)
            }
            PartKind::Media => self.parser.media_part_headers().await.map_err(Into::into),
        }
    }

    pub(crate) async fn metadata(&mut self) -> Result<Vec<u8>, ApiError> {
        self.parser.start().await?;
        self.metadata_type = self.headers(PartKind::Metadata).await?;
        let bytes = self.collect(JsonPart::Metadata).await?;
        // Only the metadata part arrived; Cloud Storage counts the payloads it received.
        if self.parser.closed() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalidPayloadSize",
                "Payload size invalid. Expected 2-3 payloads. Actual size: 1",
            ));
        }
        Ok(bytes)
    }

    /// The next decoded bytes of the current part, or `None` once its delimiter is read.
    pub(crate) async fn next_chunk(&mut self) -> Result<Option<Bytes>, ApiError> {
        self.parser.next_chunk().await.map_err(Into::into)
    }

    pub(crate) async fn finish(&mut self) -> Result<(), ApiError> {
        self.parser.finish().await.map_err(Into::into)
    }

    /// The rest of the current part, failing once it would pass `JSON_CAP`.
    async fn collect(&mut self, part: JsonPart) -> Result<Vec<u8>, ApiError> {
        let mut bytes = Vec::new();
        while let Some(chunk) = self.parser.next_chunk().await? {
            if chunk.len() > JSON_CAP - bytes.len() {
                return Err(part.too_large());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    pub(crate) async fn checksums(
        &mut self,
        expected: ExpectedHashes,
    ) -> Result<ExpectedHashes, ApiError> {
        if self.parser.closed() {
            return Ok(expected);
        }
        let headers = self.parser.part_headers().await?;
        let value = part_content_type(&headers)?;
        if json_part_type(value.as_deref()).is_err() {
            // Cloud Storage's wording (recorded 2026-11-13 for a text/plain third part).
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalidPayloadTypes",
                format!(
                    "Multipart writes with three parts must have application/json for the first and final part, but saw {} and {}",
                    self.metadata_type.as_deref().unwrap_or_default(),
                    value.as_deref().unwrap_or_default()
                ),
            ));
        }
        let bytes = self.collect(JsonPart::Checksums).await?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| ApiError::legacy_parse_error("Invalid checksum JSON"))?;
        let serde_json::Value::Object(mut values) = value else {
            return Err(invalid().into());
        };
        if values
            .keys()
            .any(|k| !matches!(k.as_str(), "md5Hash" | "crc32c"))
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid",
                "Only checksum fields are supported in the third part",
            ));
        }
        merge_hashes(&mut values, expected)
    }
}

/// What a multipart or resumable upload's metadata declares.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MultipartMetadata {
    pub(crate) name: String,
    pub(crate) update: ObjectMetadataUpdate,
    pub(crate) expected: ExpectedHashes,
}

pub(crate) fn multipart_metadata(
    bytes: &[u8],
    query_name: Option<String>,
    expected: ExpectedHashes,
) -> Result<MultipartMetadata, ApiError> {
    // The upload front end labelled a metadata part that is not JSON as `text/html`.
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|cause| crate::json::parse_error(bytes, &cause).html_typed())?;
    multipart_metadata_of(super::resource_object(&value)?, query_name, expected)
}

/// [`multipart_metadata`] for metadata the caller already holds as members, such as the
/// resumable initiation after its duplicate-key scan.
pub(crate) fn multipart_metadata_of(
    fields: &serde_json::Map<String, serde_json::Value>,
    query_name: Option<String>,
    expected: ExpectedHashes,
) -> Result<MultipartMetadata, ApiError> {
    let ObjectMetadataRequest {
        mut update,
        mut unchanged,
    } = object_metadata_from_map(fields, super::WriteMode::Patch)?;
    update.content_type = match update.content_type {
        Patch::Set(value) if !value.is_empty() => Patch::Set(value),
        Patch::Keep | Patch::Clear | Patch::Set(_) => Patch::Keep,
    };
    let metadata_name = unchanged.remove("name").filter(|value| !value.is_null());
    if metadata_name.as_ref().is_some_and(|name| !name.is_string()) {
        return Err(ApiError::invalid_argument("name must be a string or null"));
    }
    let name = match query_name {
        Some(name) => name,
        None => metadata_name
            .ok_or_else(ApiError::missing_object_name)?
            .as_str()
            .ok_or_else(|| ApiError::invalid_argument("name must be a string"))?
            .to_owned(),
    };
    super::validate_object_name(&name)?;
    if let Some(bucket) = unchanged.remove("bucket")
        && !bucket.is_string()
    {
        return Err(ApiError::invalid_argument("bucket must be a string"));
    }
    // Cloud Storage stored an insert whose `kind` named another resource as `storage#object`
    // (case obj-unsupported-inputs), so the value is output-only and ignored.
    unchanged.remove("kind");
    // Insert accepts a creation class; metadata updates keep comparing it as output-only.
    if let Some(class) = unchanged
        .remove("storageClass")
        .filter(|value| !value.is_null())
    {
        let class = class
            .as_str()
            .ok_or_else(|| ApiError::invalid_argument("storageClass must be a string or null"))?;
        update.storage_class =
            Some(crate::object::validate_object_storage_class(class).map_err(ApiError::from)?);
    }
    let expected = merge_hashes(&mut unchanged, expected)?;
    if let Some((key, _)) = unchanged.into_iter().next() {
        return Err(ApiError::unsupported_object(&key));
    }
    Ok(MultipartMetadata {
        name,
        update,
        expected,
    })
}

fn merge_hashes(
    values: &mut serde_json::Map<String, serde_json::Value>,
    mut expected: ExpectedHashes,
) -> Result<ExpectedHashes, ApiError> {
    // An equal value agrees with the caller's declaration; a different one conflicts.
    checksum::take_resource_hashes(
        values,
        || ApiError::invalid_parameter(checksum::NOT_BASE64),
        |declared| {
            expected
                .agree(declared)
                .map_err(|ChecksumConflict(algorithm)| {
                    ApiError::invalid_parameter(format!(
                        "Conflicting {} checksums",
                        algorithm.name()
                    ))
                })
        },
    )?;
    Ok(expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Full;

    fn wire(metadata: &[u8], media: &[u8]) -> Vec<u8> {
        let mut body =
            b"--boundary\r\nContent-Type: application/json; charset=\"UTF-8\"\r\n\r\n".to_vec();
        body.extend_from_slice(metadata);
        body.extend_from_slice(b"\r\n--boundary\r\nContent-Type: application/octet-stream\r\n\r\n");
        body.extend_from_slice(media);
        body.extend_from_slice(b"\r\n--boundary--\r\n");
        body
    }

    /// Metadata that was serialized again and parsed from its text, as the resumable
    /// initiation once did, reads the same as the members it came from: numbers, escapes and
    /// nested values keep their meaning through the round trip.
    #[test]
    fn metadata_members_read_like_their_serialized_text() {
        for text in [
            r#"{"name":"o","contentType":"text/plain","metadata":{"a":0.1,"b":-0.0,"c":1E7,"d":1.0E-4,"e":18446744073709551615,"f":-9223372036854775808,"g":"é\u0001\"","h":[1],"i":{"x":1},"j":null,"k":true}}"#,
            r#"{"name":"o","md5Hash":"AAAAAAAAAAAAAAAAAAAAAA==","crc32c":"AAAAAA==","storageClass":"COLDLINE"}"#,
            r#"{"name":"o","metadata":{"":"empty key"}}"#,
            r#"{"name":"o","cacheControl":12}"#,
            r#"{"name":7}"#,
            r#"{"unknownMember":[1,2]}"#,
            r#"[]"#,
            r#""o""#,
        ] {
            let value: serde_json::Value = serde_json::from_str(text).unwrap();
            let reparsed = multipart_metadata(
                value.to_string().as_bytes(),
                None,
                ExpectedHashes::default(),
            );
            let members = super::super::resource_object(&value)
                .and_then(|fields| multipart_metadata_of(fields, None, ExpectedHashes::default()));
            assert_eq!(members, reparsed, "{text}");
        }
    }

    /// The exact request shape gcloud storage 591.0.0 (apitools) sends for a small upload.
    const GCLOUD_BOUNDARY: &str = "===============5708193607524502531==";
    const GCLOUD_BODY: &[u8] = b"--===============5708193607524502531==\nContent-Type: application/json\nMIME-Version: 1.0\n\n{\"bucket\": \"example\", \"contentType\": \"text/plain\", \"name\": \"hello.txt\"}\n--===============5708193607524502531==\nContent-Type: text/plain\nMIME-Version: 1.0\nContent-Transfer-Encoding: binary\n\nHello from gcloud!\n\n--===============5708193607524502531==--\n";

    #[tokio::test]
    async fn related_uploads_accept_the_gcloud_lf_framing() {
        let boundary = GCLOUD_BOUNDARY.parse().unwrap();
        let mut upload = RelatedUpload::new(Full::new(Bytes::from_static(GCLOUD_BODY)), &boundary);
        upload.metadata().await.unwrap();
        assert_eq!(
            upload.headers(PartKind::Media).await.unwrap().as_deref(),
            Some("text/plain")
        );
        // The strict framing of XML POST Object forms still rejects bare LFs.
        let mut strict = RelatedUpload::over(Multipart::new(
            Full::new(Bytes::from_static(GCLOUD_BODY)),
            &boundary,
        ));
        assert!(strict.metadata().await.is_err());
    }

    #[tokio::test]
    async fn related_uploads_read_metadata_media_and_checksums() {
        let mut upload = RelatedUpload::new(
            Full::new(Bytes::from(wire(b"{}", b"media"))),
            &"boundary".parse().unwrap(),
        );
        assert_eq!(upload.metadata().await.unwrap(), b"{}");
        upload.headers(PartKind::Media).await.unwrap();
        assert_eq!(upload.next_chunk().await.unwrap().unwrap(), "media");
        assert!(upload.next_chunk().await.unwrap().is_none());
        let expected = upload.checksums(ExpectedHashes::default()).await.unwrap();
        assert_eq!(expected, ExpectedHashes::default());
        upload.finish().await.unwrap();
    }

    #[tokio::test]
    async fn related_uploads_refuse_a_metadata_part_that_is_not_json_or_is_alone() {
        let boundary = "boundary".parse().unwrap();
        for body in [
            b"--boundary\r\nContent-Type: text/plain\r\n\r\n{}\r\n--boundary--".as_slice(),
            b"--boundary\r\nContent-Type: application/json; charset=latin1\r\n\r\n{}\r\n--boundary--",
            b"--boundary\r\n\r\n{}\r\n--boundary--",
            b"--boundary\r\nContent-Type: application/json\r\nContent-Transfer-Encoding: base64\r\n\r\ne30=\r\n--boundary--",
        ] {
            let mut upload = RelatedUpload::new(Full::new(Bytes::copy_from_slice(body)), &boundary);
            let error = upload.metadata().await.unwrap_err();
            assert_eq!(error.reason, "invalidParameter", "{}", String::from_utf8_lossy(body));
        }
        let mut alone = RelatedUpload::new(
            Full::new(Bytes::from_static(
                b"--boundary\r\nContent-Type: application/json\r\n\r\n{}\r\n--boundary--",
            )),
            &boundary,
        );
        let error = alone.metadata().await.unwrap_err();
        assert_eq!(
            (error.status, error.reason),
            (StatusCode::BAD_REQUEST, "invalidPayloadSize")
        );
    }

    #[tokio::test]
    async fn related_uploads_bound_the_metadata_part() {
        let boundary = "boundary".parse().unwrap();
        for length in [JSON_CAP, JSON_CAP + 1] {
            for parser in [
                Multipart::new(
                    Full::new(Bytes::from(wire(&vec![b' '; length], b""))),
                    &boundary,
                ),
                Multipart::related(
                    Full::new(Bytes::from(wire(&vec![b' '; length], b""))),
                    &boundary,
                ),
            ] {
                let result = RelatedUpload::over(parser).metadata().await;
                if length == JSON_CAP {
                    assert_eq!(result.unwrap().len(), length);
                } else {
                    assert_eq!(result.unwrap_err().status, StatusCode::PAYLOAD_TOO_LARGE);
                }
            }
        }
    }
}
