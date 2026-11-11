//! XML API object GET, HEAD and DELETE.

use super::RequestBody as Incoming;
use super::{
    ObjectCtx, ResponseBody, anonymous_error, body_read_ahead, boxed, empty_response, gzip,
    insert_content_range, object_body,
};
use crate::{
    auth,
    bucket::BucketAccess,
    csek::CustomerKey,
    json,
    object::{ObjectConditions, ObjectRecord},
    storage::ObjectReader,
    xml,
};
use hyper::{HeaderMap, Method, Request, Response, StatusCode, header::HeaderValue};

#[cfg(test)]
mod tests;

pub(super) async fn respond(
    mut request: Request<Incoming>,
    operation: xml::ReadRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, xml::Error> {
    if operation.target.name.is_empty() {
        return xml::list::respond(request, operation, ctx).await;
    }
    let delete = request.method() == Method::DELETE;
    let permission = if delete {
        auth::OBJECTS_DELETE
    } else {
        auth::OBJECTS_GET
    };
    let access = ctx
        .authorize(permission, &operation.target.bucket)
        .await
        .map_err(|error| anonymous_error(error, ctx.caller))?;
    let conditions = xml::headers(
        request.headers(),
        if delete {
            xml::HeaderPolicy::Strict
        } else {
            xml::HeaderPolicy::Read
        },
    )?;
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
        ctx.await_catalog(
            ctx.state
                .catalog
                .delete_object(operation.target, conditions, &access),
        )
        .await
        .map_err(|error| anonymous_error(error.into(), ctx.caller))?;
        return Ok(boxed(empty_response(StatusCode::NO_CONTENT)));
    }
    read(&request, operation, conditions, ctx, &access, key).await
}

fn conditional_read(
    request: &HeaderMap,
    record: &ObjectRecord,
    headers: &mut HeaderMap,
) -> Result<Option<Response<ResponseBody>>, xml::Error> {
    use crate::http_conditions::{ReadCondition, Validators, evaluate_read};
    let validators = Validators {
        etag: &xml::object_etag(record),
        last_modified: record.updated,
    };
    match evaluate_read(request, &validators) {
        Ok(ReadCondition::Proceed) => Ok(None),
        Ok(ReadCondition::NotModified) => {
            if json::object_media_negotiable(record) {
                gzip::insert_negotiated_vary(headers);
            }
            Ok(Some(boxed(xml::not_modified(headers))))
        }
        Ok(ReadCondition::PreconditionFailed) => Err(xml::Error::precondition_failed()),
        Err(_) => Err(xml::Error::invalid(
            "Invalid entity tag in a conditional header.",
        )),
    }
}

async fn read(
    request: &Request<Incoming>,
    operation: xml::ReadRequest,
    conditions: ObjectConditions,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    key: Option<CustomerKey>,
) -> Result<Response<ResponseBody>, xml::Error> {
    let xml::ReadRequest {
        target,
        response_overrides,
    } = operation;
    let catalog = &ctx.state.catalog;
    let head = request.method() == Method::HEAD;
    // HEAD returns only metadata, which an encrypted object exposes without its key.
    let (record, mut reader, reveal) = if head {
        catalog
            .open_object_metadata_keyed(target, conditions, access, key)
            .await
    } else {
        catalog
            .open_object_keyed(target, conditions, access, key, body_read_ahead(request))
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
    xml::extension_headers::spell_read_metadata(
        &mut headers,
        auth::signature::ExtensionDialect::of(request.uri().query(), request.headers()),
    )?;
    // A private object is sent with `Expires` set to the response time, even when the object
    // carries its own `Cache-Control` (recorded with `no-cache`).
    if scope == xml::CacheScope::Private
        && let Some(value) = xml::envelope::expires(ctx.state.clock.now())
    {
        headers.insert("expires", value);
    }
    json::insert_response_headers(&record, &mut headers);
    if !reveal {
        headers.remove("x-goog-hash");
    }
    if let Some(response) = conditional_read(request.headers(), &record, &mut headers)? {
        return Ok(response);
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
        return gunzipped(
            ctx,
            &record,
            reader,
            headers,
            response_overrides,
            request.method(),
        )
        .await;
    }
    headers.extend(response_overrides);
    let requested = request
        .headers()
        .get("range")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    // Cloud Storage rejects a multi-range request with 400 instead of serving the first range.
    let requested_range = json::range_request(request.headers());
    if let json::RangeRequest::Multiple(requested) = requested_range {
        return Ok(range_error(
            request,
            headers,
            xml::Error::multiple_ranges(requested),
            ctx.state.clock.now(),
        ));
    }
    let range = match requested_range.resolve(record.size) {
        Ok(range) => range,
        Err(error) if error.status == StatusCode::RANGE_NOT_SATISFIABLE => {
            return Ok(range_error(
                request,
                headers,
                xml::Error::unsatisfiable_range(requested),
                ctx.state.clock.now(),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if let Some((offset, length)) = range {
        if !head {
            reader.set_range(offset, length).await.map_err(|_| {
                xml::Error::from(json::JsonError::service_unavailable(
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
        // The rejection is private whatever the object's own Cache-Control says.
        let mut response = gzip::plain_body_rejection(headers);
        let rejection = response.headers_mut();
        rejection.insert(
            "cache-control",
            HeaderValue::from_static(xml::PRIVATE_CACHE_CONTROL),
        );
        if let Some(value) = xml::envelope::expires(ctx.state.clock.now()) {
            rejection.insert("expires", value);
        }
        return Ok(response);
    }
    gzip::insert_transcoded_headers(&mut headers);
    headers.extend(overrides);
    Ok(gzip::response(ctx, record, reader, headers, method))
}

/// A refused Range keeps the object's validators, generation, hashes and `accept-ranges`, with
/// the error document in place of the representation headers, and no `content-range`
/// (observed 2026-10-31 for an unsatisfiable and for a multiple range).
fn range_error(
    request: &Request<Incoming>,
    mut headers: HeaderMap,
    error: xml::Error,
    now: std::time::SystemTime,
) -> Response<ResponseBody> {
    let error = error.response(xml::ErrorBody::for_method(request.method()));
    for name in [
        "content-range",
        "content-encoding",
        "content-language",
        "content-disposition",
    ] {
        headers.remove(name);
    }
    let (parts, body) = error.into_parts();
    // The error is private whatever the object's own Cache-Control says: a public object's
    // refusal must not stay in a shared cache.
    for name in ["content-type", "content-length", "cache-control"] {
        if let Some(value) = parts.headers.get(name) {
            headers.insert(name, value.clone());
        }
    }
    headers.remove("expires");
    if let Some(value) = xml::envelope::expires(now) {
        headers.insert("expires", value);
    }
    let mut response = boxed(Response::new(body));
    *response.status_mut() = parts.status;
    *response.headers_mut() = headers;
    response
}
