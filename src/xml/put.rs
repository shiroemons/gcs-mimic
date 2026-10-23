use super::{Error, ReadRequest, number};
use crate::{
    auth::{self, Caller},
    config::Config,
    json,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate, UploadChunk},
    server::{
        ObjectActivity, ResponseBody, State, anonymous_error, authorize_bucket,
        await_upload_catalog, boxed, stream_upload,
    },
};
use http_body_util::Full;
use hyper::{
    HeaderMap, Request, Response, StatusCode,
    body::{Bytes, Incoming},
};
use std::{collections::BTreeMap, io, time::Duration};
use tokio::sync::{mpsc, watch};

struct Headers {
    content_type: String,
    expected: ExpectedHashes,
    conditions: ObjectConditions,
    metadata: ObjectMetadataUpdate,
}

fn single<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, Error> {
    let mut values = headers.get_all(name).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "ExcessHeaderValues",
            "Expected one header value.",
        ));
    }
    value.to_str().map(Some).map_err(|_| {
        Error::new(
            StatusCode::BAD_REQUEST,
            "InvalidArgument",
            "Header must be representable as ASCII.",
        )
    })
}

fn headers(headers: &HeaderMap, max_bytes: u64) -> Result<Headers, Error> {
    for key in headers.keys() {
        let name = key.as_str();
        if (name.starts_with("x-goog-")
            && !name.starts_with("x-goog-meta-")
            && !matches!(
                name,
                "x-goog-hash"
                    | "x-goog-if-generation-match"
                    | "x-goog-if-metageneration-match"
                    | "x-goog-api-client"
                    | "x-goog-gcs-idempotency-token"
            ))
            || name.starts_with("x-amz-")
            || matches!(
                name,
                "content-range"
                    | "range"
                    | "if-match"
                    | "if-none-match"
                    | "if-modified-since"
                    | "if-unmodified-since"
                    | "if-range"
                    | "trailer"
            )
        {
            return Err(Error::unsupported());
        }
    }
    let mut conditions = ObjectConditions::default();
    for (key, target, zero) in [
        (
            "x-goog-if-generation-match",
            &mut conditions.if_generation_match,
            true,
        ),
        (
            "x-goog-if-metageneration-match",
            &mut conditions.if_metageneration_match,
            false,
        ),
    ] {
        *target = single(headers, key)?
            .map(|raw| number(raw, zero))
            .transpose()?;
    }
    if let Some(token) = single(headers, "x-goog-gcs-idempotency-token")? {
        if token.trim().is_empty() {
            return Err(Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidArgument",
                "Expected a non-empty retry token.",
            ));
        }
        if conditions.if_generation_match.is_none() {
            return Err(Error::unsupported());
        }
    }
    let length = single(headers, "content-length")?
        .map(|raw| {
            raw.parse::<u64>()
                .ok()
                .filter(|_| !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()))
                .ok_or_else(|| {
                    Error::new(
                        StatusCode::BAD_REQUEST,
                        "InvalidArgument",
                        "Invalid Content-Length.",
                    )
                })
        })
        .transpose()?;
    let chunked = single(headers, "transfer-encoding")?;
    if chunked.is_some_and(|value| !value.eq_ignore_ascii_case("chunked")) {
        return Err(Error::unsupported());
    }
    if chunked.is_some() && length.is_some() {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "InvalidArgument",
            "Ambiguous body framing.",
        ));
    }
    if length.is_none() && chunked.is_none() {
        return Err(Error::new(
            StatusCode::LENGTH_REQUIRED,
            "MissingContentLength",
            "Content-Length or chunked transfer encoding is required.",
        ));
    }
    if length.is_some_and(|length| length > max_bytes) {
        return Err(storage_error(ObjectError::TooLarge, None));
    }
    let mut selected = HeaderMap::new();
    if let Some(content_type) = headers.get("content-type") {
        single(headers, "content-type")?;
        selected.insert("content-type", content_type.clone());
    }
    let (content_type, _) = json::parse_object_upload_headers(&selected, conditions)?;
    selected.clear();
    for value in headers.get_all("x-goog-hash") {
        selected.append("x-goog-hash", value.clone());
    }
    let (_, mut expected) =
        json::parse_object_upload_headers(&selected, conditions).map_err(|error| {
            if error.status == StatusCode::NOT_IMPLEMENTED {
                return Error::from(error);
            }
            Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidDigest",
                "Invalid x-goog-hash digest.",
            )
        })?;
    if let Some(md5) = single(headers, "content-md5")? {
        selected.clear();
        selected.insert(
            "x-goog-hash",
            format!("md5={md5}").parse().map_err(|_| {
                Error::new(
                    StatusCode::BAD_REQUEST,
                    "InvalidDigest",
                    "Invalid Content-MD5.",
                )
            })?,
        );
        let (_, content_md5) =
            json::parse_object_upload_headers(&selected, conditions).map_err(|_| {
                Error::new(
                    StatusCode::BAD_REQUEST,
                    "InvalidDigest",
                    "Invalid Content-MD5.",
                )
            })?;
        if expected
            .md5
            .zip(content_md5.md5)
            .is_some_and(|(a, b)| a != b)
        {
            return Err(Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidDigest",
                "Conflicting MD5 declarations.",
            ));
        }
        expected.md5 = expected.md5.or(content_md5.md5);
    }
    let mut metadata = ObjectMetadataUpdate::default();
    for (name, field) in [
        ("cache-control", &mut metadata.cache_control),
        ("content-disposition", &mut metadata.content_disposition),
        ("content-encoding", &mut metadata.content_encoding),
        ("content-language", &mut metadata.content_language),
    ] {
        *field = single(headers, name)?.map(|value| Some(value.to_owned()));
    }
    let mut custom = BTreeMap::new();
    for name in headers.keys() {
        if let Some(key) = name.as_str().strip_prefix("x-goog-meta-") {
            if key.is_empty() {
                return Err(Error::new(
                    StatusCode::BAD_REQUEST,
                    "InvalidArgument",
                    "Custom metadata requires a key.",
                ));
            }
            custom.insert(
                key.to_owned(),
                Some(single(headers, name.as_str())?.unwrap().to_owned()),
            );
        }
    }
    if custom
        .iter()
        .map(|(key, value)| key.len() + value.as_ref().unwrap().len())
        .sum::<usize>()
        > 8192
    {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "MetadataTooLarge",
            "Custom metadata exceeds 8 KiB.",
        ));
    }
    metadata.metadata = Some(Some(custom));
    Ok(Headers {
        content_type,
        expected,
        conditions,
        metadata,
    })
}

fn storage_error(error: ObjectError, caller: Option<Caller<'_>>) -> Error {
    match error {
        ObjectError::ChecksumMismatch => Error::new(
            StatusCode::BAD_REQUEST,
            "BadDigest",
            "The supplied checksum does not match the uploaded object.",
        ),
        ObjectError::TooLarge => Error::new(
            StatusCode::BAD_REQUEST,
            "EntityTooLarge",
            "The upload exceeds the configured object size limit.",
        ),
        ObjectError::BodyAborted(_) => Error::new(
            StatusCode::BAD_REQUEST,
            "IncompleteBody",
            "The upload body did not complete.",
        ),
        error => anonymous_error(error.into(), caller).into(),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "The XML adapter reuses the shared request and streaming deadline context"
)]
pub(crate) async fn respond(
    request: Request<Incoming>,
    operation: ReadRequest,
    caller: Option<Caller<'_>>,
    state: &State,
    config: &Config,
    activity: &watch::Sender<ObjectActivity>,
    deadline: tokio::time::Instant,
) -> Result<Response<ResponseBody>, Error> {
    let access = authorize_bucket(state, caller, auth::OBJECTS_CREATE, &operation.bucket)
        .await
        .map_err(|error| anonymous_error(error, caller))?;
    let Headers {
        content_type,
        expected,
        conditions,
        metadata,
    } = headers(request.headers(), config.max_object_bytes)?;
    let timeout = Duration::from_millis(config.catalog_wait_timeout_ms);
    let permit = await_upload_catalog(
        state.catalog.prepare_object_upload(
            &operation.bucket,
            &operation.name,
            conditions,
            &access,
        ),
        timeout,
        deadline,
        activity,
    )
    .await
    .map_err(|error| storage_error(error, caller))?;
    let (sender, body) = mpsc::channel(2);
    let upload = state.catalog.put_object_prechecked(
        &operation.bucket,
        &operation.name,
        content_type,
        expected,
        conditions,
        config.max_object_bytes,
        body,
        permit,
        metadata,
        access,
    );
    tokio::pin!(upload);
    let record = tokio::select! {
        result = &mut upload => result,
        result = stream_upload(request.into_body(), &sender, config.max_object_bytes, activity) => {
            if let Err(error) = result {
                if sender.send(Err(io::Error::other(error.to_string()))).await.is_err() {
                    return Err(storage_error(upload.await.err().unwrap_or(error), caller));
                }
                return Err(storage_error(error, caller));
            }
            await_upload_catalog(async {
                let _ = sender.send(Ok(UploadChunk::End)).await;
                drop(sender);
                upload.await
            }, timeout, deadline, activity).await
        }
    }
    .map_err(|error| storage_error(error, caller))?;
    let mut headers = super::media_headers(&record, false)?;
    let mut response = boxed(Response::new(Full::new(Bytes::new())));
    for name in [
        "etag",
        "x-goog-generation",
        "x-goog-metageneration",
        "x-goog-hash",
        "x-goog-stored-content-length",
        "x-goog-stored-content-encoding",
    ] {
        response
            .headers_mut()
            .insert(name, headers.remove(name).unwrap());
    }
    Ok(response)
}
