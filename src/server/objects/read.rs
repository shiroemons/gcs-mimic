//! JSON API object reads: media downloads and metadata.

use crate::object::ObjectRecord;
use crate::server::RequestBody as Incoming;
use crate::{
    bucket::BucketAccess,
    json::{self, JsonError},
    object::{ObjectConditions, ObjectError},
    server::{
        ObjectCtx, QueryPrecondition, ResponseBody, acl, boxed, gzip, insert_content_range,
        insert_unsatisfied_content_range, not_modified_response, object_body,
        read_etag_not_modified, read_metadata_response,
    },
};
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Method, Request, Response, StatusCode, body::Bytes, header::HeaderValue};

pub(super) async fn get(
    request: &Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    bucket: &str,
    name: &str,
    params: &json::ObjectGetParams,
    fields: &json::Fields,
) -> Result<Response<ResponseBody>, JsonError> {
    // Match failures precede both query and HTTP cache validators on one snapshot.
    let mixed = request.headers().contains_key("if-match")
        || request.headers().contains_key("if-none-match");
    let conditions = if mixed {
        ObjectConditions {
            if_generation_not_match: None,
            if_metageneration_not_match: None,
            ..params.conditions
        }
    } else {
        params.conditions
    };
    let target = ReadTarget {
        bucket,
        name,
        params,
        conditions,
    };
    if params.media {
        return media(request, ctx, access, &target).await;
    }
    metadata(request, ctx, access, &target, fields).await
}

/// The object a read selects, with its HTTP validators already reconciled.
struct ReadTarget<'a> {
    bucket: &'a str,
    name: &'a str,
    params: &'a json::ObjectGetParams,
    /// The query conditions the catalog checks on the read snapshot.
    conditions: ObjectConditions,
}

async fn media(
    request: &Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    target: &ReadTarget<'_>,
) -> Result<Response<ResponseBody>, JsonError> {
    let params = target.params;
    let key = json::request_key(request.headers())?;
    let (record, mut reader) = ctx
        .state
        .catalog
        .open_object_keyed(
            target.bucket,
            target.name,
            params.generation,
            target.conditions,
            access,
            key,
        )
        .await?;
    json::validate_read_condition_headers(request.headers(), json::EtagConditions::Supported)?;
    json::validate_keyed_object_headers(
        request.headers(),
        json::ObjectRead::Media,
        &[crate::csek::DESTINATION_PREFIX],
    )?;
    let mut headers = json::object_media_headers(&record)?;
    json::insert_response_headers(&record, &mut headers);
    if reader.publicly_readable {
        headers.insert(
            "cache-control",
            record
                .cache_control
                .as_deref()
                .unwrap_or("public, max-age=3600")
                .parse()
                .map_err(|_| {
                    JsonError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "backendError",
                        "Invalid stored Cache-Control",
                    )
                })?,
        );
    }
    let transcode = json::object_media_transcoding(request.headers(), &record)?;
    if read_etag_not_modified(request.headers(), &json::object_etag(&record))?
        || params.conditions.if_generation_not_match == Some(record.generation)
        || params.conditions.if_metageneration_not_match == Some(record.metageneration)
    {
        return Ok(boxed(not_modified_response(
            &headers["etag"],
            &headers["cache-control"],
        )));
    }
    // A media HEAD sends the GET headers without reading the object (observed 2026-10-31).
    let head = request.method() == Method::HEAD;
    if transcode {
        // Range is ignored while gunzipping: the whole expanded object is sent with 200.
        if !head
            && gzip::is_plain(&mut reader, record.size)
                .await
                .map_err(|_| {
                    JsonError::new(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "backendError",
                        "Cannot read object data",
                    )
                })?
        {
            return Ok(gzip::plain_body_rejection(headers));
        }
        gzip::insert_transcoded_headers(&mut headers);
        let mut response = if head {
            boxed(Response::new(Full::new(Bytes::new())))
        } else {
            Response::new(gzip::body(
                reader,
                record.size,
                ctx.config.max_object_bytes,
                ctx.activity,
            ))
        };
        *response.headers_mut() = headers;
        return Ok(response);
    }
    let negotiable = json::object_media_negotiable(&record);
    if negotiable {
        gzip::insert_negotiated_vary(&mut headers);
        if head {
            // Cloud Storage states no length for a stored-gzip HEAD even when sent compressed,
            // with or without a Range.
            headers.remove("content-length");
        }
    }
    let range = match json::object_media_range(request.headers(), record.size) {
        Ok(range) => range,
        Err(error) => {
            let mut response = error
                .into_response()
                .map(|body| body.map_err(|never| match never {}).boxed_unsync());
            insert_unsatisfied_content_range(response.headers_mut(), record.size);
            return Ok(response);
        }
    };
    if let Some((offset, length)) = range {
        if !head {
            reader.set_range(offset, length).await.map_err(|_| {
                JsonError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "Cannot seek object data",
                )
            })?;
        }
        if !(head && negotiable) {
            headers.insert("content-length", length.into());
        }
        insert_content_range(&mut headers, offset, length, record.size);
        range_hashes(&mut headers, &record, offset, length)?;
    }
    let mut response = if head {
        boxed(Response::new(Full::new(Bytes::new())))
    } else {
        Response::new(object_body(reader, ctx.activity))
    };
    if range.is_some() {
        *response.status_mut() = StatusCode::PARTIAL_CONTENT;
    }
    *response.headers_mut() = headers;
    Ok(response)
}

/// Cloud Storage's JSON API omits `x-goog-hash` from a partial range, and sends only the
/// CRC32C when the range spans the whole object (observed 2026-10-31). Clients such as the
/// .NET SDK validate any hash they receive against the bytes, so whole-object hashes on a
/// partial range fail their download. The XML API keeps both hashes on a range.
fn range_hashes(
    headers: &mut HeaderMap,
    record: &ObjectRecord,
    offset: u64,
    length: u64,
) -> Result<(), JsonError> {
    headers.remove("x-goog-hash");
    if offset == 0 && length == record.size {
        let crc32c = format!("crc32c={}", json::base64(&record.crc32c.to_be_bytes()));
        headers.insert(
            "x-goog-hash",
            HeaderValue::try_from(crc32c).map_err(|_| {
                JsonError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "Invalid stored object response header",
                )
            })?,
        );
    }
    Ok(())
}

async fn metadata(
    request: &Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    target: &ReadTarget<'_>,
    fields: &json::Fields,
) -> Result<Response<ResponseBody>, JsonError> {
    let params = target.params;
    let key = json::request_key(request.headers())?;
    let catalog = &ctx.state.catalog;
    let record = if params.soft_deleted {
        let generation = params
            .generation
            .ok_or_else(|| JsonError::invalid_argument("softDeleted=true requires generation"))?;
        catalog
            .get_soft_deleted_object_authorized(
                target.bucket,
                target.name,
                generation,
                target.conditions,
                access,
            )
            .await?
    } else {
        catalog
            .get_object_authorized(
                target.bucket,
                target.name,
                params.generation,
                target.conditions,
                access,
            )
            .await?
    };
    json::validate_read_condition_headers(request.headers(), json::EtagConditions::Supported)?;
    json::validate_keyed_object_headers(
        request.headers(),
        json::ObjectRead::Metadata,
        &[crate::csek::DESTINATION_PREFIX],
    )?;
    // Metadata needs no key; a supplied key must match and then reveals hashes.
    crate::csek::check_metadata(record.customer_key_sha256, key)
        .map_err(ObjectError::CustomerEncryption)?;
    let mut resource =
        json::object_resource_keyed(&record, &ctx.endpoints.api_base_url, params.projection, key)?;
    if params.projection == json::Projection::Full {
        acl::add_full_projection(
            ctx.state,
            ctx.caller,
            &mut resource,
            &ctx.endpoints.api_base_url,
        )
        .await?;
    }
    let query = if params.conditions.if_generation_not_match == Some(record.generation)
        || params.conditions.if_metageneration_not_match == Some(record.metageneration)
    {
        QueryPrecondition::NotModified
    } else {
        QueryPrecondition::Unmatched
    };
    Ok(boxed(read_metadata_response(
        request.headers(),
        &json::object_etag(&record),
        resource,
        fields,
        query,
    )?))
}
