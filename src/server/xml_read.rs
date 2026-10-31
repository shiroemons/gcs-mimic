//! XML API object GET, HEAD and DELETE.

use super::RequestBody as Incoming;
use super::{
    ObjectCtx, ResponseBody, anonymous_error, boxed, empty_response, gzip, insert_content_range,
    object_body,
};
use crate::{
    auth, bucket::BucketAccess, csek::CustomerKey, json, object::ObjectRecord,
    storage::ObjectReader, xml,
};
use hyper::{HeaderMap, Method, Request, Response, StatusCode, header::HeaderValue};

pub(super) async fn respond(
    mut request: Request<Incoming>,
    mut operation: xml::ReadRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, xml::Error> {
    if operation.name.is_empty() {
        return xml::list::respond(request, operation, ctx).await;
    }
    let delete = request.method() == Method::DELETE;
    let permission = if delete {
        auth::OBJECTS_DELETE
    } else {
        auth::OBJECTS_GET
    };
    let access = ctx
        .authorize(permission, &operation.bucket)
        .await
        .map_err(|error| anonymous_error(error, ctx.caller))?;
    xml::headers(request.headers(), &mut operation)?;
    if delete
        && let Some(name) =
            crate::csek::has_headers(request.headers(), crate::csek::DESTINATION_PREFIX)
    {
        return Err(xml::Error::unsupported(&name));
    }
    let key = json::request_key(request.headers())?;
    // Reads and DELETE have no payload; consume framing before any mutation or response.
    xml::empty_body(request.body_mut()).await?;
    if delete {
        ctx.state
            .catalog
            .delete_object_authorized(
                &operation.bucket,
                &operation.name,
                operation.generation,
                operation.conditions,
                &access,
            )
            .await
            .map_err(|error| anonymous_error(error.into(), ctx.caller))?;
        return Ok(boxed(empty_response(StatusCode::NO_CONTENT)));
    }
    read(&request, operation, ctx, &access, key).await
}

async fn read(
    request: &Request<Incoming>,
    operation: xml::ReadRequest,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    key: Option<CustomerKey>,
) -> Result<Response<ResponseBody>, xml::Error> {
    let catalog = &ctx.state.catalog;
    let head = request.method() == Method::HEAD;
    // HEAD returns only metadata, which an encrypted object exposes without its key.
    let (record, mut reader, reveal) = if head {
        catalog
            .open_object_metadata_keyed(
                &operation.bucket,
                &operation.name,
                operation.generation,
                operation.conditions,
                access,
                key,
            )
            .await
    } else {
        catalog
            .open_object_keyed(
                &operation.bucket,
                &operation.name,
                operation.generation,
                operation.conditions,
                access,
                key,
            )
            .await
            .map(|(record, reader)| (record, reader, true))
    }
    .map_err(|error| anonymous_error(error.into(), ctx.caller))?;
    let scope = if reader.publicly_readable {
        xml::CacheScope::Public
    } else {
        xml::CacheScope::Private
    };
    let mut headers = xml::media_headers(&record, scope)?;
    json::insert_response_headers(&record, &mut headers);
    headers.remove("x-goog-hash");
    if reveal {
        insert_hashes(&mut headers, &record);
    }
    if json::object_media_negotiable(&record) {
        // The XML API varies every representation of a stored-gzip object, gunzipped or not,
        // and a compressed HEAD states no length (observed 2026-10-31).
        gzip::insert_negotiated_vary(&mut headers);
        if head {
            headers.remove("content-length");
        }
    }
    if json::object_media_transcoding(request.headers(), &record)? {
        let overrides = operation.response_overrides;
        return gunzipped(ctx, &record, reader, headers, overrides, request.method()).await;
    }
    headers.extend(operation.response_overrides);
    let range = match json::object_media_range(request.headers(), record.size) {
        Ok(range) => range,
        Err(error) if error.status == StatusCode::RANGE_NOT_SATISFIABLE => {
            return Ok(unsatisfiable_range(request, headers));
        }
        Err(error) => return Err(error.into()),
    };
    if let Some((offset, length)) = range {
        if !head {
            reader.set_range(offset, length).await.map_err(|_| {
                xml::Error::from(json::JsonError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "Cannot seek object data",
                ))
            })?;
        }
        // A compressed stored-gzip HEAD states no length, with or without a Range.
        if !(head && json::object_media_negotiable(&record)) {
            headers.insert("content-length", length.into());
        }
        insert_content_range(&mut headers, offset, length, record.size);
    }
    let mut response = if head {
        boxed(empty_response(StatusCode::OK))
    } else {
        Response::new(object_body(reader, ctx.activity))
    };
    if range.is_some() {
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
    }
    *response.headers_mut() = headers;
    Ok(response)
}

/// A gunzipped read ignores Range and sends the whole expanded object with 200, unless the
/// stored bytes are not gzip, which fails before any header is sent. The response-header
/// overrides apply only to the successful representation.
async fn gunzipped(
    ctx: ObjectCtx<'_>,
    record: &ObjectRecord,
    mut reader: ObjectReader,
    mut headers: HeaderMap,
    overrides: HeaderMap,
    method: &Method,
) -> Result<Response<ResponseBody>, xml::Error> {
    let head = method == Method::HEAD;
    if !head
        && gzip::is_plain(&mut reader, record.size)
            .await
            .map_err(json::JsonError::from)?
    {
        return Ok(gzip::plain_body_rejection(headers));
    }
    gzip::insert_transcoded_headers(&mut headers);
    headers.extend(overrides);
    let mut response = if head {
        boxed(empty_response(StatusCode::OK))
    } else {
        Response::new(gzip::body(
            reader,
            record.size,
            ctx.config.max_object_bytes,
            ctx.activity,
        ))
    };
    *response.headers_mut() = headers;
    Ok(response)
}

/// An unsatisfiable Range keeps the object's validators, generation, hashes and
/// `accept-ranges`, with an `InvalidRange` document in place of the representation
/// headers, and no `content-range` (observed 2026-10-31).
fn unsatisfiable_range(
    request: &Request<Incoming>,
    mut headers: HeaderMap,
) -> Response<ResponseBody> {
    let requested = request
        .headers()
        .get("range")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let error = xml::Error::unsatisfiable_range(requested)
        .response(xml::ErrorBody::for_method(request.method()));
    for name in [
        "content-range",
        "content-encoding",
        "content-language",
        "content-disposition",
    ] {
        headers.remove(name);
    }
    let (parts, body) = error.into_parts();
    for name in ["content-type", "content-length"] {
        if let Some(value) = parts.headers.get(name) {
            headers.insert(name, value.clone());
        }
    }
    let mut response = boxed(Response::new(body));
    *response.status_mut() = parts.status;
    *response.headers_mut() = headers;
    response
}

/// Cloud Storage's XML API sends one `x-goog-hash` field per algorithm, always for the
/// whole object, including on a range (observed 2026-10-31). The JSON API instead combines
/// them in one field.
fn insert_hashes(headers: &mut HeaderMap, record: &ObjectRecord) {
    let crc32c = format!("crc32c={}", json::base64(&record.crc32c.to_be_bytes()));
    let md5 = record
        .has_md5()
        .then(|| format!("md5={}", json::base64(&record.md5)));
    // Base64 always forms a valid header value; omitting a hash is safer than panicking
    // if that ever changes.
    for value in std::iter::once(crc32c).chain(md5) {
        if let Ok(value) = HeaderValue::try_from(value) {
            headers.append("x-goog-hash", value);
        }
    }
}
