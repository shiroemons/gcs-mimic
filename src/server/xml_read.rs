//! XML API object GET, HEAD and DELETE.

use super::RequestBody as Incoming;
use super::{
    ObjectCtx, ResponseBody, anonymous_error, boxed, empty_response, insert_content_range,
    insert_unsatisfied_content_range, object_body,
};
use crate::{auth, bucket::BucketAccess, csek::CustomerKey, json, xml};
use hyper::{Method, Request, Response, StatusCode};

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
    if !reveal {
        headers.remove("x-goog-hash");
    }
    headers.extend(operation.response_overrides);
    json::validate_object_media_encoding(request.headers(), &record)?;
    let range = match json::object_media_range(request.headers(), record.size) {
        Ok(range) => range,
        Err(_) if head => {
            // HEAD's Range only adds Content-Range; status and representation length stay unchanged.
            insert_unsatisfied_content_range(&mut headers, record.size);
            None
        }
        Err(error) => {
            let mut response = boxed(
                xml::Error::from(error).response(xml::ErrorBody::for_method(request.method())),
            );
            insert_unsatisfied_content_range(response.headers_mut(), record.size);
            return Ok(response);
        }
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
            headers.insert("content-length", length.into());
        }
        insert_content_range(&mut headers, offset, length, record.size);
    }
    let mut response = if head {
        boxed(empty_response(StatusCode::OK))
    } else {
        Response::new(object_body(reader, ctx.activity))
    };
    if range.is_some() && !head {
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
    }
    *response.headers_mut() = headers;
    Ok(response)
}
