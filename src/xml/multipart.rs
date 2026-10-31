//! XML multipart protocol adapter; storage owns session transitions and durable bytes.

use super::{Error, ReadRequest, element, put, signing_parameter};
use crate::{
    auth, json,
    object::{ObjectError, UploadChunk},
    server::{ObjectCtx, RequestBody, ResponseBody, anonymous_error, boxed, stream_upload},
    storage::xml_multipart as storage,
};
use http_body_util::Full;
use hyper::{Method, Request, Response, StatusCode, Uri, body::Bytes, header::HeaderValue};
use std::{collections::BTreeMap, io};
use tokio::sync::mpsc;

mod manifest;
mod render;

const MANIFEST_LIMIT: usize = 2 * 1024 * 1024;

pub(crate) fn matches(uri: &Uri) -> bool {
    uri.query().is_some_and(|query| {
        query.split('&').any(|field| {
            json::decode(
                field.split('=').next().unwrap_or_default(),
                json::Decoding::Form,
            )
            .is_ok_and(|name| matches!(name.as_str(), "uploads" | "uploadId" | "partNumber"))
        })
    })
}

fn invalid(message: &str) -> Error {
    Error::new(StatusCode::BAD_REQUEST, "InvalidArgument", message)
}

fn xml_text(value: &str) -> Result<(), Error> {
    if value.chars().all(|ch| {
        matches!(ch, '\t' | '\n' | '\r')
            || (ch >= '\u{20}' && !matches!(ch, '\u{fffe}' | '\u{ffff}'))
    }) {
        Ok(())
    } else {
        Err(invalid(
            "This multipart value cannot be represented in XML 1.0",
        ))
    }
}

#[derive(Clone, Copy)]
enum Operation {
    Begin,
    Put(u16),
    Parts,
    Uploads,
    Complete,
    Abort,
}

fn operation(
    method: &Method,
    name: &str,
    query: &mut BTreeMap<String, String>,
) -> Result<(Operation, String), Error> {
    let uploads = query.remove("uploads");
    let id = query.remove("uploadId");
    let number = query.remove("partNumber");
    if uploads.as_ref().is_some_and(|value| !value.is_empty())
        || (uploads.is_some() && id.is_some())
    {
        return Err(invalid(
            "Use uploads or uploadId, with an empty uploads value",
        ));
    }
    let operation = match (
        method,
        name.is_empty(),
        uploads.is_some(),
        id.is_some(),
        number.as_deref(),
    ) {
        (&Method::POST, false, true, false, None) => Operation::Begin,
        (&Method::GET, true, true, false, None) => Operation::Uploads,
        (&Method::GET, false, false, true, None) => Operation::Parts,
        (&Method::POST, false, false, true, None) => Operation::Complete,
        (&Method::DELETE, false, false, true, None) => Operation::Abort,
        (&Method::PUT, false, false, true, Some(number)) => {
            Operation::Put(integer(number, 1, 10000)? as u16)
        }
        _ => return Err(invalid("Invalid multipart method or query combination")),
    };
    if id
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.len() > 1024)
    {
        return Err(invalid("Invalid uploadId"));
    }
    Ok((operation, id.unwrap_or_default()))
}

fn integer(raw: &str, min: u32, max: u32) -> Result<u32, Error> {
    raw.parse::<u32>()
        .ok()
        .filter(|value| {
            *value >= min
                && *value <= max
                && !raw.is_empty()
                && raw.bytes().all(|byte| byte.is_ascii_digit())
        })
        .ok_or_else(|| invalid("Multipart number is out of range"))
}

fn numeric(
    query: &mut BTreeMap<String, String>,
    name: &str,
    default: u32,
    max: u32,
) -> Result<u32, Error> {
    query
        .remove(name)
        .map(|value| integer(&value, 0, max))
        .transpose()
        .map(|value| value.unwrap_or(default))
}

fn remaining(query: &BTreeMap<String, String>) -> Result<(), Error> {
    if let Some(name) = query.keys().find(|key| !signing_parameter(key)) {
        return Err(Error::unsupported(name));
    }
    Ok(())
}

fn request_headers(
    request: &Request<RequestBody>,
    max: u64,
    operation: Operation,
) -> Result<put::Headers, Error> {
    for name in request.headers().keys() {
        let name = name.as_str();
        if name.starts_with("x-goog-if-") || name == "x-goog-gcs-idempotency-token" {
            return Err(Error::unsupported(
                "multipart preconditions and invocation-token replay",
            ));
        }
        if !matches!(operation, Operation::Begin)
            && (name.starts_with("x-goog-meta-")
                || matches!(
                    name,
                    "x-goog-storage-class"
                        | "cache-control"
                        | "content-disposition"
                        | "content-language"
                        | "content-encoding"
                ))
        {
            return Err(Error::unsupported("multipart metadata outside initiation"));
        }
    }
    let signing = if auth::signature::is_signed(request.uri().query())
        || auth::signature::has_header(request.headers())
    {
        put::Signing::SignedUrl
    } else {
        put::Signing::Header
    };
    put::headers(request.headers(), max, signing)
}

fn storage_error(error: storage::Error, ctx: ObjectCtx<'_>) -> Error {
    let code = match &error {
        storage::Error::NoSuchUpload => "NoSuchUpload",
        storage::Error::InvalidPart => "InvalidPart",
        storage::Error::InvalidPartOrder => "InvalidPartOrder",
        storage::Error::PartTooSmall => "InvalidArgument",
        storage::Error::Busy => "ServiceUnavailable",
        storage::Error::Object(_) => "",
    };
    if let storage::Error::Object(error) = error {
        return put::storage_error(error, ctx.caller);
    }
    let status = match error {
        storage::Error::NoSuchUpload => StatusCode::NOT_FOUND,
        storage::Error::Busy => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::BAD_REQUEST,
    };
    Error::new(status, code, error.to_string())
}

fn response(body: String) -> Response<ResponseBody> {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/xml; charset=UTF-8"),
    );
    boxed(response)
}

pub(crate) async fn respond(
    request: Request<RequestBody>,
    path: ReadRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let mut query = crate::auth::signature::xml_query(
        request.uri().query().unwrap_or_default(),
        request.headers(),
    )?;
    let (operation, id) = operation(request.method(), &path.name, &mut query)?;
    xml_text(&path.name)?;
    let permission = match operation {
        Operation::Begin | Operation::Put(_) | Operation::Complete => {
            "storage.multipartUploads.create"
        }
        Operation::Parts => "storage.multipartUploads.listParts",
        Operation::Uploads => "storage.multipartUploads.list",
        Operation::Abort => "storage.multipartUploads.abort",
    };
    let access = ctx
        .authorize(permission, &path.bucket)
        .await
        .map_err(|error| anonymous_error(error, ctx.caller))?;
    if matches!(operation, Operation::Uploads | Operation::Parts) {
        return render::list(request, path, operation, id, query, access, ctx).await;
    }
    remaining(&query)?;
    let max = match operation {
        Operation::Put(_) => ctx.config.max_object_bytes.min(storage::MAX_PART_BYTES),
        Operation::Complete => MANIFEST_LIMIT as u64,
        _ => 0,
    };
    let headers = request_headers(&request, max, operation)?;
    if matches!(operation, Operation::Begin) {
        if headers.expected != Default::default() {
            return Err(Error::unsupported(
                "whole-object checksums during multipart initiation",
            ));
        }
        empty_body(request, headers.sha256).await?;
        let options = storage::Options {
            content_type: headers.content_type,
            metadata: headers.metadata,
        };
        let upload = ctx
            .await_catalog(ctx.state.catalog.begin_xml_multipart(
                &path.bucket,
                &path.name,
                options,
                access,
            ))
            .await
            .map_err(|error| storage_error(error, ctx))?;
        let mut body = render::start("InitiateMultipartUploadResult");
        render::target(&mut body, &upload.target);
        body.push_str("</InitiateMultipartUploadResult>");
        return Ok(response(body));
    }
    let target = storage::Target {
        bucket: path.bucket,
        name: path.name,
        id,
    };
    let lease = ctx
        .await_catalog(ctx.state.catalog.lease_xml_multipart(target, access))
        .await
        .map_err(|error| storage_error(error, ctx))?;
    if matches!(operation, Operation::Put(_) | Operation::Complete) {
        let key = json::request_key(request.headers())?;
        crate::csek::check_data(lease.upload.options.metadata.customer_key_sha256, key).map_err(
            |error| put::storage_error(ObjectError::CustomerEncryption(error), ctx.caller),
        )?;
    }
    match operation {
        Operation::Put(number) => upload_part(request, lease, number, headers, ctx).await,
        Operation::Complete => complete(request, lease, headers, ctx).await,
        Operation::Abort => {
            empty_body(request, headers.sha256).await?;
            ctx.await_catalog(ctx.state.catalog.abort_xml_multipart(lease))
                .await
                .map_err(|error| storage_error(error, ctx))?;
            let mut response = response(String::new());
            *response.status_mut() = StatusCode::NO_CONTENT;
            Ok(response)
        }
        _ => Err(invalid("Invalid multipart operation")),
    }
}

async fn empty_body(
    request: Request<RequestBody>,
    expected: Option<[u8; 32]>,
) -> Result<(), Error> {
    super::empty_body(request.into_body()).await?;
    validate_sha256(expected, b"")
}

fn validate_sha256(expected: Option<[u8; 32]>, bytes: &[u8]) -> Result<(), Error> {
    if expected.is_some_and(|expected| {
        expected.as_slice() != ring::digest::digest(&ring::digest::SHA256, bytes).as_ref()
    }) {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "BadDigest",
            "The signed body digest does not match",
        ));
    }
    Ok(())
}

async fn upload_part(
    request: Request<RequestBody>,
    lease: storage::Lease,
    number: u16,
    headers: put::Headers,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let (sender, body) = mpsc::channel(2);
    let upload = ctx.state.catalog.put_xml_part(
        lease,
        storage::PartUpload {
            number,
            expected: headers.expected,
            max_bytes: ctx.config.max_object_bytes,
        },
        body,
    );
    tokio::pin!(upload);
    let part=tokio::select! {
        result=&mut upload=>result,
        result=stream_upload(request.into_body(),&sender,ctx.config.max_object_bytes.min(storage::MAX_PART_BYTES),ctx.activity,headers.sha256)=> {
            if let Err(error)=result {
                let _=sender.send(Err(io::Error::other(error.to_string()))).await;drop(sender);
                let _=ctx.await_catalog(upload).await;
                return Err(put::storage_error(error,ctx.caller));
            }
            ctx.await_catalog(async {let _=sender.send(Ok(UploadChunk::End)).await;drop(sender);upload.await}).await
        }
    }.map_err(|error|storage_error(error,ctx))?;
    let mut response = response(String::new());
    response.headers_mut().insert(
        "etag",
        HeaderValue::from_str(&part.etag).map_err(|_| invalid("Invalid stored part ETag"))?,
    );
    Ok(response)
}

async fn complete(
    request: Request<RequestBody>,
    lease: storage::Lease,
    headers: put::Headers,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let bytes = json::read_body(request.into_body(), MANIFEST_LIMIT).await?;
    validate_sha256(headers.sha256, &bytes)?;
    if headers.expected != Default::default() {
        return Err(Error::unsupported(
            "checksums on a multipart completion document",
        ));
    }
    let parts = manifest::parse(&bytes)?;
    let record = ctx
        .await_catalog(ctx.state.catalog.complete_xml_multipart(
            lease,
            parts,
            ctx.config.max_object_bytes,
        ))
        .await
        .map_err(|error| storage_error(error, ctx))?;
    let mut body = render::start("CompleteMultipartUploadResult");
    element(
        &mut body,
        "Location",
        &format!(
            "{}/{}/{}",
            ctx.endpoints.api_base_url.trim_end_matches('/'),
            record.bucket,
            crate::url::encode_object_name(&record.name)
        ),
    );
    element(&mut body, "Bucket", &record.bucket);
    element(&mut body, "Key", &record.name);
    element(&mut body, "ETag", &super::object_etag(&record));
    body.push_str("</CompleteMultipartUploadResult>");
    let mut response = response(body);
    json::insert_response_headers(&record, response.headers_mut());
    let mut headers = super::media_headers(&record, super::CacheScope::Private)?;
    for name in [
        "etag",
        "x-goog-hash",
        "x-goog-generation",
        "x-goog-metageneration",
    ] {
        if let Some(value) = headers.remove(name) {
            response.headers_mut().insert(name, value);
        }
    }
    Ok(response)
}
