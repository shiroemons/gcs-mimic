use super::{ApiError, ObjectInsertParams, multipart::MultipartMetadata, multipart_metadata_of};
use crate::percent::RequestUri;
use crate::service::object_headers::{single, unsigned, upload_content_type};
use crate::{
    bucket::{BucketAccess, name::BucketKey},
    object::{ExpectedHashes, ObjectError, content_type},
    profile::CompatProfile,
    storage::ResumableOptions,
};
use hyper::{HeaderMap, Method};

/// The resumable session a request continues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UploadSession {
    pub(crate) bucket: BucketKey,
    pub(crate) upload_id: String,
}

pub(crate) fn session_route(
    method: &Method,
    uri: &RequestUri,
    profile: CompatProfile,
) -> Option<Result<UploadSession, ApiError>> {
    let path = uri.as_uri().path().strip_prefix("/upload/storage/v1/b/")?;
    let (bucket, rest) = path.split_once('/')?;
    if rest != "o"
        || !uri.parameters().map_or_else(
            |_| uri.as_uri().query().is_some_and(names_upload_id),
            |query| query.contains_key("upload_id"),
        )
    {
        return None;
    }
    Some(parse_session(method, bucket, uri, profile))
}

/// Whether `query` has an `upload_id` key once its escapes are decoded: Cloud Storage answers an
/// initiation that spells it `upload%5Fid` as a request on that session (recorded 2026-11-25).
/// A key that does not decode names no session; parsing the query then reports it.
fn names_upload_id(query: &str) -> bool {
    query.split('&').any(|pair| {
        let key = pair.split('=').next().unwrap_or_default();
        crate::percent::decode(key, crate::percent::Decoding::Form)
            .is_ok_and(|key| key == "upload_id")
    })
}

/// `bucket` is the raw path segment the route matched.
fn parse_session(
    method: &Method,
    bucket: &str,
    uri: &RequestUri,
    profile: CompatProfile,
) -> Result<UploadSession, ApiError> {
    if !matches!(*method, Method::PUT | Method::POST | Method::DELETE) {
        return Err(ApiError::method_not_allowed(
            "Session requires PUT, POST or DELETE",
            None,
        ));
    }
    let bucket = crate::percent::decode(bucket, crate::percent::Decoding::Path)?;
    let bucket = BucketKey::for_profile(bucket, profile)?;
    let mut query = uri.parameters()?.clone();
    let upload_id = query
        .remove("upload_id")
        .ok_or_else(|| ApiError::invalid_parameter("Missing upload_id"))?;
    if query.remove("uploadType").is_some_and(|v| v != "resumable") {
        return Err(ApiError::invalid_parameter("Invalid uploadType"));
    }
    // The session URI repeats the initiation query (`name`, preconditions), which Cloud
    // Storage accepts and ignores on later requests.
    Ok(UploadSession { bucket, upload_id })
}

/// What a resumable initiation declares in its headers. `X-Goog-Hash` is not among them:
/// Cloud Storage accepted an initiation naming a wrong crc32c or md5 and completed the session
/// with the computed hashes (recorded 2026-11-17, case obj-probe-upload-hashes), as it ignores
/// the header on media and multipart uploads. Only the metadata's `crc32c` and `md5Hash` and
/// the requests on the session URI declare hashes the completion verifies.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UploadHeaders {
    /// `X-Upload-Content-Type`, unless absent or empty.
    pub(crate) mime: Option<String>,
    /// `X-Upload-Content-Length`, which binds the session's total unless it is 0.
    pub(crate) total: Option<u64>,
}

pub(crate) fn headers(headers: &HeaderMap, max: u64) -> Result<UploadHeaders, ApiError> {
    for name in headers.keys() {
        if name.as_str().starts_with("x-goog-upload-") {
            return Err(ApiError::unsupported_object(name.as_str()));
        }
    }
    // Cloud Storage leaves the total open after a declared 0: chunks may omit it and the upload
    // may end at any size, while a declared 5 refuses a longer chunk (recorded 2026-11-15, case
    // obj-resumable-declared-length). The PHP SDK declares the size of its still-empty buffer.
    let total = single(headers, "x-upload-content-length")?
        .map(unsigned)
        .transpose()?
        .filter(|total| *total != 0);
    if total.is_some_and(|n| n > max) {
        return Err(ObjectError::TooLarge.into());
    }
    let declared = single(headers, "x-upload-content-type")?.filter(|value| !value.is_empty());
    let mut clean = headers.clone();
    clean.remove("x-upload-content-type");
    clean.remove("x-upload-content-length");
    clean.remove("origin");
    clean.insert(
        "content-type",
        declared
            .unwrap_or(content_type::DEFAULT)
            .parse()
            .map_err(|_| ApiError::invalid_parameter("Invalid upload content type"))?,
    );
    // Runs even without a declared type: it also validates the initiation's other headers.
    let mime = upload_content_type(&clean)?;
    Ok(UploadHeaders {
        mime: declared.map(|_| mime),
        total,
    })
}

/// What a resumable upload's initiation request declares outside its metadata body: the URL
/// name and parameters, the upload headers and the caller's access.
pub(crate) struct Initiation {
    pub(crate) name: Option<String>,
    /// `X-Upload-Content-Type`, unless absent or empty.
    pub(crate) mime: Option<String>,
    pub(crate) params: ObjectInsertParams,
    pub(crate) access: BucketAccess,
    pub(crate) origin: Option<String>,
    pub(crate) encoding: Option<String>,
}

pub(crate) fn options(
    bytes: &[u8],
    initiation: Initiation,
) -> Result<(String, ResumableOptions), ApiError> {
    let Initiation {
        name,
        mime,
        params,
        access,
        origin,
        encoding,
    } = initiation;
    let bytes = if bytes.is_empty() {
        b"{}".as_slice()
    } else {
        bytes
    };
    validate_duplicate_fields(bytes)?;
    let mut value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|cause| crate::json::parse_error(bytes, &cause))?;
    if let Some(object) = value.as_object_mut()
        && object.get("bucket").is_some_and(serde_json::Value::is_null)
    {
        object.remove("bucket");
    }
    let MultipartMetadata {
        name,
        update: mut metadata,
        expected,
    } = multipart_metadata_of(
        super::resource_object(&value)?,
        name,
        ExpectedHashes::default(),
    )?;
    super::declare_content_encoding(&mut metadata, encoding)?;
    // Cloud Storage refused an initiation whose metadata contentType disagrees with
    // X-Upload-Content-Type, with the multipart message (case obj-gaps-uploads).
    if let (Some(declared), Some(upload)) = (metadata.content_type.value(), &mime) {
        super::check_declared_content_type(declared, upload)?;
    }
    let content_type = metadata
        .content_type
        .value()
        .cloned()
        .or(mime)
        .unwrap_or_else(|| content_type::DEFAULT.to_owned());
    // Persisted with the session so completion applies the ACL the initiation requested.
    metadata.acl = params.predefined_acl.into();
    Ok((
        name,
        ResumableOptions {
            content_type,
            metadata,
            expected,
            conditions: params.conditions,
            access,
            origin,
            projection: params.projection,
            response_fields: None,
        },
    ))
}

#[cfg(test)]
#[test]
fn an_escaped_upload_id_key_names_the_session() {
    let route = |query: &str| {
        let uri: RequestUri = format!("/upload/storage/v1/b/bucket/o?{query}")
            .parse()
            .unwrap();
        session_route(&Method::PUT, &uri, CompatProfile::Native)
            .map(|session| session.unwrap().upload_id)
    };
    for query in [
        "upload_id=abc",
        "uploadType=resumable&upload%5Fid=abc",
        "upload%5fid=abc",
        "upload%5F%69d=abc",
    ] {
        assert_eq!(route(query).as_deref(), Some("abc"), "{query}");
    }
    for query in [
        "uploadType=resumable&name=upload_id",
        "upload_identity=abc",
        "upload%ZZid=abc",
    ] {
        assert_eq!(route(query), None, "{query}");
    }
}

#[cfg(test)]
#[test]
fn duplicate_field_scan_handles_escaped_keys_nested_objects_and_truncated_strings() {
    for input in [
        r#"{"metadata":{"猫":"a\\\"b"},"items":[{"key":1},{"key":2}]}"#,
        r#"{"metadata":{"a\"b":"one","a\\b":"two"}}"#,
    ] {
        assert!(
            validate_duplicate_fields(input.as_bytes()).is_ok(),
            "{input}"
        );
    }
    for input in [
        r#"{"metadata":{"key":1,"\u006bey":2}}"#,
        r#"{"key":"unterminated"#,
        r#"{"key":"escaped\""#,
    ] {
        let error = validate_duplicate_fields(input.as_bytes()).unwrap_err();
        assert_eq!(error.status(), hyper::StatusCode::BAD_REQUEST, "{input}");
    }
}

fn validate_duplicate_fields(bytes: &[u8]) -> Result<(), ApiError> {
    // JSON parsing validates syntax; this bounded scan preserves duplicate-key information.
    let _: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|cause| crate::json::parse_error(bytes, &cause))?;
    let mut stack: Vec<Option<std::collections::BTreeSet<String>>> = Vec::new();
    let mut i = 0;
    while let Some(byte) = bytes.get(i) {
        match byte {
            b'{' => stack.push(Some(Default::default())),
            b'[' => stack.push(None),
            b'}' | b']' => {
                stack.pop();
            }
            b'"' => {
                let start = i;
                i += 1;
                while let Some(byte) = bytes.get(i).filter(|byte| **byte != b'"') {
                    if *byte == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                let end = i + 1;
                let mut next = end;
                while bytes.get(next).is_some_and(u8::is_ascii_whitespace) {
                    next += 1;
                }
                if bytes.get(next) == Some(&b':') {
                    let key: String = serde_json::from_slice(
                        bytes
                            .get(start..end)
                            .ok_or_else(|| ApiError::invalid_parameter("Invalid JSON key"))?,
                    )
                    .map_err(|_| ApiError::invalid_parameter("Invalid JSON key"))?;
                    if let Some(Some(keys)) = stack.last_mut()
                        && !keys.insert(key)
                    {
                        return Err(ApiError::invalid_parameter("Duplicate metadata field"));
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    Ok(())
}
