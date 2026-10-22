//! Path-style XML object reads. Shared authorization, generation selection and bytes live in storage.

use crate::{
    json::{self, JsonError},
    object::{ObjectConditions, ObjectRecord},
};
use http_body_util::Full;
use hyper::{
    HeaderMap, Method, Response, StatusCode, Uri,
    body::Bytes,
    header::{HeaderName, HeaderValue},
};
use std::time::UNIX_EPOCH;

pub(crate) struct ReadRequest {
    pub bucket: String,
    pub name: String,
    pub generation: Option<i64>,
    pub conditions: ObjectConditions,
}

pub(crate) struct Error {
    pub status: StatusCode,
    code: &'static str,
    message: String,
}

impl Error {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn unsupported() -> Self {
        Self::new(
            StatusCode::NOT_IMPLEMENTED,
            "NotImplemented",
            "The requested functionality is not implemented.",
        )
    }

    pub fn response(self, head: bool) -> Response<Full<Bytes>> {
        let escaped = self
            .message
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{}</Code><Message>{escaped}</Message></Error>",
            self.code
        );
        Response::builder()
            .status(self.status)
            .header("content-type", "application/xml; charset=UTF-8")
            .header("content-length", body.len())
            .body(Full::new(if head {
                Bytes::new()
            } else {
                Bytes::from(body)
            }))
            .unwrap()
    }
}

impl From<JsonError> for Error {
    fn from(error: JsonError) -> Self {
        let code = match error.status {
            StatusCode::UNAUTHORIZED if error.reason == "required" => "AuthenticationRequired",
            StatusCode::UNAUTHORIZED => "InvalidAuthentication",
            StatusCode::FORBIDDEN => "AccessDenied",
            StatusCode::NOT_FOUND if error.message == "Bucket not found" => "NoSuchBucket",
            StatusCode::NOT_FOUND => "NoSuchKey",
            StatusCode::PRECONDITION_FAILED => "PreconditionFailed",
            StatusCode::RANGE_NOT_SATISFIABLE => "InvalidRange",
            StatusCode::NOT_IMPLEMENTED => "NotImplemented",
            StatusCode::SERVICE_UNAVAILABLE => "ServiceUnavailable",
            _ => "InvalidArgument",
        };
        // shortcut: auth wording and error precedence are local choices until cloud differential evidence exists.
        Self::new(error.status, code, error.message)
    }
}

/// Match reserved Google and emulator namespaces before interpreting a bucket path.
pub(crate) fn route(method: &Method, uri: &Uri) -> Option<Result<ReadRequest, Error>> {
    let path = uri.path();
    if [
        "/storage/v1",
        "/upload/storage/v1",
        "/download/storage/v1",
        "/batch",
        "/_internal",
    ]
    .iter()
    .any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|tail| tail.starts_with('/'))
    }) {
        return None;
    }
    let path = path.strip_prefix('/')?;
    let (bucket, name) = path.split_once('/').unwrap_or((path, ""));
    if json::validate_bucket_name(bucket).is_err() {
        return None;
    }
    Some((|| {
        if !matches!(*method, Method::GET | Method::HEAD) || name.is_empty() {
            return Err(Error::unsupported());
        }
        let name = json::decode(name, false)?;
        json::validate_object_name(&name)?;
        let mut query = json::parse_query(uri.query().unwrap_or_default())?;
        let generation = query
            .remove("generation")
            .map(|value| number(&value, false))
            .transpose()?;
        if !query.is_empty() {
            return Err(Error::unsupported());
        }
        Ok(ReadRequest {
            bucket: bucket.into(),
            name,
            generation,
            conditions: ObjectConditions::default(),
        })
    })())
}

fn number(raw: &str, zero: bool) -> Result<i64, Error> {
    raw.parse::<i64>()
        .ok()
        .filter(|n| *n >= i64::from(!zero))
        .filter(|_| !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| {
            Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidArgument",
                "Invalid generation or metageneration.",
            )
        })
}

pub(crate) fn headers(headers: &HeaderMap, request: &mut ReadRequest) -> Result<(), Error> {
    for key in headers.keys() {
        let key = key.as_str();
        if (key.starts_with("x-goog-")
            && !matches!(
                key,
                "x-goog-if-generation-match" | "x-goog-if-metageneration-match"
            ))
            || matches!(
                key,
                "if-match"
                    | "if-none-match"
                    | "if-modified-since"
                    | "if-unmodified-since"
                    | "if-range"
            )
        {
            return Err(Error::unsupported());
        }
    }
    for (key, target, zero) in [
        (
            "x-goog-if-generation-match",
            &mut request.conditions.if_generation_match,
            true,
        ),
        (
            "x-goog-if-metageneration-match",
            &mut request.conditions.if_metageneration_match,
            false,
        ),
    ] {
        let mut values = headers.get_all(key).iter();
        if let Some(value) = values.next() {
            if values.next().is_some() {
                return Err(Error::new(
                    StatusCode::BAD_REQUEST,
                    "ExcessHeaderValues",
                    "Multiple precondition header values.",
                ));
            }
            *target = Some(number(
                value.to_str().map_err(|_| {
                    Error::new(
                        StatusCode::BAD_REQUEST,
                        "InvalidArgument",
                        "Invalid precondition header.",
                    )
                })?,
                zero,
            )?);
        }
    }
    if headers.contains_key("transfer-encoding")
        || headers.get("content-length").is_some_and(|v| v != "0")
    {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "UnexpectedContent",
            "This method requires an empty body.",
        ));
    }
    Ok(())
}

pub(crate) fn media_headers(
    record: &ObjectRecord,
    publicly_readable: bool,
) -> Result<HeaderMap, Error> {
    let mut headers = json::object_media_headers(record)?;
    let cache = record
        .cache_control
        .as_deref()
        .unwrap_or(if publicly_readable {
            "public, max-age=3600"
        } else {
            "private, max-age=0"
        });
    let etag = format!(
        "\"{}\"",
        record
            .md5
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let timestamp = json::timestamp(record.updated)?;
    let days = record
        .updated
        .duration_since(UNIX_EPOCH)
        .map_err(|_| {
            Error::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "ServiceUnavailable",
                "Invalid stored timestamp.",
            )
        })?
        .as_secs()
        / 86400;
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][(days % 7) as usize];
    let month: usize = timestamp[5..7].parse().expect("validated timestamp month");
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][month - 1];
    let date = format!(
        "{weekday}, {} {month} {} {} GMT",
        &timestamp[8..10],
        &timestamp[..4],
        &timestamp[11..19]
    );
    for (key, value) in [
        ("cache-control", cache),
        ("etag", &etag),
        ("last-modified", &date),
        ("x-goog-storage-class", &record.storage_class),
    ] {
        headers.insert(
            key,
            HeaderValue::from_str(value).map_err(|_| {
                Error::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "ServiceUnavailable",
                    "Invalid stored header.",
                )
            })?,
        );
    }
    for (key, value) in &record.metadata {
        let key =
            HeaderName::from_bytes(format!("x-goog-meta-{key}").as_bytes()).map_err(|_| {
                Error::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "NotImplemented",
                    "Custom metadata cannot be represented as XML headers.",
                )
            })?;
        let value = HeaderValue::from_str(value).map_err(|_| {
            Error::new(
                StatusCode::NOT_IMPLEMENTED,
                "NotImplemented",
                "Custom metadata cannot be represented as XML headers.",
            )
        })?;
        if headers.contains_key(&key) {
            return Err(Error::unsupported());
        }
        headers.insert(key, value);
    }
    Ok(headers)
}
