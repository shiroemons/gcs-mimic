use super::{
    JsonError, ObjectInsertParams, Projection, multipart_metadata, parse_object_upload_headers,
};
use crate::{
    bucket::BucketAccess,
    object::{ExpectedHashes, ObjectError},
    storage::ResumableOptions,
};
use hyper::{HeaderMap, Method, StatusCode, Uri};

pub(crate) fn session_route(
    method: &Method,
    uri: &Uri,
) -> Option<Result<(String, String), JsonError>> {
    let path = uri.path().strip_prefix("/upload/storage/v1/b/")?;
    let (bucket, rest) = path.split_once('/')?;
    if rest != "o"
        || !uri.query().is_some_and(|q| {
            q.split('&')
                .any(|p| p.split('=').next() == Some("upload_id"))
        })
    {
        return None;
    }
    Some((|| {
        if !matches!(*method, Method::PUT | Method::POST | Method::DELETE) {
            return Err(JsonError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "methodNotAllowed",
                "Session requires PUT, POST or DELETE",
            ));
        }
        let bucket = super::decode(bucket, false)?;
        super::validate_bucket_name(&bucket)?;
        let mut query = super::parse_query(uri.query().unwrap_or_default())?;
        let id = query
            .remove("upload_id")
            .ok_or_else(|| JsonError::invalid_parameter("Missing upload_id"))?;
        if query.remove("uploadType").is_some_and(|v| v != "resumable") {
            return Err(JsonError::invalid_parameter("Invalid uploadType"));
        }
        super::query::finish(query)?;
        Ok((bucket, id))
    })())
}

pub(crate) fn single<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, JsonError> {
    let mut values = headers.get_all(name).iter();
    let value = values
        .next()
        .map(|v| {
            v.to_str()
                .map_err(|_| JsonError::invalid_parameter(format!("Invalid {name}")))
        })
        .transpose()?;
    if values.next().is_some() {
        return Err(JsonError::invalid_parameter(format!("Duplicate {name}")));
    }
    Ok(value)
}

pub(crate) fn unsigned(value: &str) -> Result<u64, JsonError> {
    if value.is_empty() || !value.bytes().all(|v| v.is_ascii_digit()) {
        return Err(JsonError::invalid_parameter(
            "Expected an unsigned decimal integer",
        ));
    }
    value
        .parse()
        .map_err(|_| JsonError::invalid_parameter("Upload integer overflow"))
}

pub(crate) fn headers(
    headers: &HeaderMap,
    params: ObjectInsertParams,
    max: u64,
) -> Result<(String, ExpectedHashes, Option<u64>), JsonError> {
    for name in headers.keys() {
        if name.as_str().starts_with("x-goog-upload-") {
            return Err(JsonError::unsupported_object(name.as_str()));
        }
    }
    let total = single(headers, "x-upload-content-length")?
        .map(unsigned)
        .transpose()?;
    if total.is_some_and(|n| n > max) {
        return Err(ObjectError::TooLarge.into());
    }
    let mime = single(headers, "x-upload-content-type")?.unwrap_or("application/octet-stream");
    let mut clean = headers.clone();
    clean.remove("x-upload-content-type");
    clean.remove("x-upload-content-length");
    clean.remove("origin");
    clean.insert(
        "content-type",
        mime.parse()
            .map_err(|_| JsonError::invalid_parameter("Invalid upload content type"))?,
    );
    let (mime, hashes) = parse_object_upload_headers(&clean, params.conditions)?;
    Ok((mime, hashes, total))
}

#[allow(
    clippy::too_many_arguments,
    reason = "Initiation combines existing metadata and upload contracts"
)]
pub(crate) fn options(
    bytes: &[u8],
    name: Option<String>,
    mime: String,
    expected: ExpectedHashes,
    params: ObjectInsertParams,
    access: BucketAccess,
    origin: Option<String>,
    encoding: Option<String>,
) -> Result<(String, ResumableOptions), JsonError> {
    let bytes = if bytes.is_empty() {
        b"{}".as_slice()
    } else {
        bytes
    };
    validate_duplicate_fields(bytes)?;
    let mut value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| {
        JsonError::new(
            StatusCode::BAD_REQUEST,
            "parseError",
            "Invalid metadata JSON",
        )
    })?;
    if value.get("bucket").is_some_and(|v| v.is_null()) {
        value
            .as_object_mut()
            .expect("object field")
            .remove("bucket");
    }
    let bytes = value.to_string().into_bytes();
    let (name, mut metadata, expected) = multipart_metadata(&bytes, name, expected)?;
    if let Some(encoding) = encoding {
        metadata.content_encoding = super::parse_object_metadata(
            serde_json::json!({"contentEncoding":encoding})
                .to_string()
                .as_bytes(),
            false,
        )?
        .update
        .content_encoding;
    }
    let content_type = metadata
        .content_type
        .as_ref()
        .and_then(|v| v.as_ref())
        .cloned()
        .unwrap_or(mime);
    Ok((
        name,
        ResumableOptions {
            content_type,
            metadata,
            expected,
            conditions: params.conditions,
            access,
            origin,
            projection_full: params.projection == Projection::Full,
        },
    ))
}

fn validate_duplicate_fields(bytes: &[u8]) -> Result<(), JsonError> {
    // JSON parsing validates syntax; this bounded scan preserves duplicate-key information.
    let _: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| {
        JsonError::new(
            StatusCode::BAD_REQUEST,
            "parseError",
            "Invalid metadata JSON",
        )
    })?;
    let mut stack: Vec<Option<std::collections::BTreeSet<String>>> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => stack.push(Some(Default::default())),
            b'[' => stack.push(None),
            b'}' | b']' => {
                stack.pop();
            }
            b'"' => {
                let start = i;
                i += 1;
                while bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                let end = i + 1;
                let mut next = end;
                while next < bytes.len() && bytes[next].is_ascii_whitespace() {
                    next += 1;
                }
                if bytes.get(next) == Some(&b':') {
                    let key: String = serde_json::from_slice(&bytes[start..end])
                        .map_err(|_| JsonError::invalid_parameter("Invalid JSON key"))?;
                    if let Some(Some(keys)) = stack.last_mut()
                        && !keys.insert(key)
                    {
                        return Err(JsonError::invalid_parameter("Duplicate metadata field"));
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Range {
    Status(Option<u64>),
    Data {
        start: u64,
        end: u64,
        total: Option<u64>,
    },
}

pub(crate) fn range(headers: &HeaderMap) -> Result<Option<Range>, JsonError> {
    single(headers, "content-range")?
        .map(|value| {
            let value = value
                .strip_prefix("bytes ")
                .ok_or_else(|| JsonError::invalid_parameter("Invalid Content-Range"))?;
            let (range, total) = value
                .split_once('/')
                .ok_or_else(|| JsonError::invalid_parameter("Invalid Content-Range"))?;
            let total = if total == "*" {
                None
            } else {
                Some(unsigned(total)?)
            };
            if range == "*" {
                return Ok(Range::Status(total));
            }
            let (start, end) = range
                .split_once('-')
                .ok_or_else(|| JsonError::invalid_parameter("Invalid Content-Range"))?;
            let (start, end) = (unsigned(start)?, unsigned(end)?);
            if end < start || end == u64::MAX || total.is_some_and(|n| end >= n) {
                return Err(JsonError::invalid_parameter(
                    "Invalid Content-Range offsets",
                ));
            }
            Ok(Range::Data { start, end, total })
        })
        .transpose()
}
