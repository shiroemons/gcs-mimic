use super::{Error, ReadRequest, number};
use crate::{
    auth::{self, Caller},
    json,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate, UploadChunk},
    server::{ObjectCtx, ResponseBody, anonymous_error, boxed, stream_upload},
};
use http_body_util::Full;
use hyper::{
    HeaderMap, Request, Response, StatusCode,
    body::{Bytes, Incoming},
};
use std::{collections::BTreeMap, io};
use tokio::sync::mpsc;

struct Headers {
    content_type: String,
    expected: ExpectedHashes,
    conditions: ObjectConditions,
    metadata: ObjectMetadataUpdate,
    sha256: Option<[u8; 32]>,
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

fn headers(headers: &HeaderMap, max_bytes: u64, signed: bool) -> Result<Headers, Error> {
    for key in headers.keys() {
        let name = key.as_str();
        if (name.starts_with("x-goog-")
            && !name.starts_with("x-goog-meta-")
            && !crate::csek::is_key_header(name, crate::csek::DESTINATION_PREFIX)
            && !(signed && name == "x-goog-content-sha256")
            && !matches!(
                name,
                "x-goog-hash"
                    | "x-goog-if-generation-match"
                    | "x-goog-if-metageneration-match"
                    | "x-goog-api-client"
                    | "x-goog-gcs-idempotency-token"
                    | "x-goog-storage-class"
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
            return Err(Error::unsupported(name));
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
            return Err(Error::unsupported(
                "x-goog-gcs-idempotency-token without x-goog-if-generation-match; add x-goog-if-generation-match: 0",
            ));
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
        return Err(Error::unsupported(
            "transfer-encoding; use Transfer-Encoding: chunked",
        ));
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
    // Checksum failures omit the algorithm; CRC32C codes await provider parity (SPEC-XML-PUT-EDGES).
    let (_, mut expected) =
        json::parse_object_upload_headers(&selected, conditions).map_err(|error| {
            if error.reason == "badRequest" {
                return Error::from(error);
            }
            Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidDigest",
                "Invalid x-goog-hash digest.",
            )
        })?;
    if let Some(md5) = single(headers, "content-md5")? {
        let content_md5 = json::decode_hash::<16>(md5).map_err(|_| {
            Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidDigest",
                "Invalid Content-MD5.",
            )
        })?;
        if expected.md5.is_some_and(|value| value != content_md5) {
            return Err(Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidDigest",
                "Conflicting MD5 declarations.",
            ));
        }
        expected.md5 = Some(content_md5);
    }
    let mut metadata = ObjectMetadataUpdate {
        customer_key_sha256: json::request_key(headers)?.map(crate::csek::CustomerKey::sha256),
        ..Default::default()
    };
    for (name, field) in [
        ("cache-control", &mut metadata.cache_control),
        ("content-disposition", &mut metadata.content_disposition),
        ("content-encoding", &mut metadata.content_encoding),
        ("content-language", &mut metadata.content_language),
    ] {
        *field = single(headers, name)?.map(|value| Some(value.to_owned()));
    }
    if let Some(class) = single(headers, "x-goog-storage-class")? {
        crate::object::validate_object_storage_class(class).map_err(|error| match error {
            ObjectError::UnsupportedMetadata(reason) => Error::unsupported(&reason),
            _ => Error::new(
                StatusCode::BAD_REQUEST,
                "InvalidArgument",
                "Invalid x-goog-storage-class; use STANDARD, NEARLINE, COLDLINE or ARCHIVE.",
            ),
        })?;
        metadata.storage_class = Some(class.to_owned());
    }
    let mut custom = BTreeMap::new();
    let mut custom_bytes = 0usize;
    for name in headers.keys() {
        if let Some(key) = name.as_str().strip_prefix("x-goog-meta-") {
            if key.is_empty() {
                return Err(Error::new(
                    StatusCode::BAD_REQUEST,
                    "InvalidArgument",
                    "Custom metadata requires a key.",
                ));
            }
            // `name` comes from this map, so the header is present.
            if let Some(value) = single(headers, name.as_str())? {
                custom_bytes += key.len() + value.len();
                custom.insert(key.to_owned(), Some(value.to_owned()));
            }
        }
    }
    if custom_bytes > 8192 {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "MetadataTooLarge",
            "Custom metadata exceeds 8 KiB.",
        ));
    }
    metadata.metadata = Some(Some(custom));
    let sha256 = auth::signature::payload_hash(headers).map_err(|_| {
        Error::new(
            StatusCode::BAD_REQUEST,
            "InvalidDigest",
            "Invalid SHA256 declaration.",
        )
    })?;
    Ok(Headers {
        content_type,
        expected,
        conditions,
        metadata,
        sha256,
    })
}

pub(super) fn storage_error(error: ObjectError, caller: Option<Caller<'_>>) -> Error {
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

pub(crate) async fn respond(
    request: Request<Incoming>,
    operation: ReadRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let ObjectCtx {
        state,
        config,
        activity,
        caller,
        ..
    } = ctx;
    let access = ctx
        .authorize(auth::OBJECTS_CREATE, &operation.bucket)
        .await
        .map_err(|error| anonymous_error(error, caller))?;
    let Headers {
        content_type,
        expected,
        conditions,
        metadata,
        sha256,
    } = headers(
        request.headers(),
        config.max_object_bytes,
        auth::signature::is_signed(request.uri().query()),
    )?;
    let permit = ctx
        .await_catalog(state.catalog.prepare_object_upload(
            &operation.bucket,
            &operation.name,
            conditions,
            &access,
        ))
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
        result = stream_upload(request.into_body(), &sender, config.max_object_bytes, activity, sha256) => {
            if let Err(error) = result {
                if sender.send(Err(io::Error::other(error.to_string()))).await.is_err() {
                    return Err(storage_error(upload.await.err().unwrap_or(error), caller));
                }
                return Err(storage_error(error, caller));
            }
            ctx.await_catalog(async {
                let _ = sender.send(Ok(UploadChunk::End)).await;
                drop(sender);
                upload.await
            }).await
        }
    }
    .map_err(|error| storage_error(error, caller))?;
    let mut headers = super::media_headers(&record, super::CacheScope::Private)?;
    let mut response = boxed(Response::new(Full::new(Bytes::new())));
    json::insert_response_headers(&record, response.headers_mut());
    for name in [
        "etag",
        "x-goog-generation",
        "x-goog-metageneration",
        "x-goog-hash",
        "x-goog-stored-content-length",
        "x-goog-stored-content-encoding",
    ] {
        if let Some(value) = headers.remove(name) {
            response.headers_mut().insert(name, value);
        }
    }
    Ok(response)
}
