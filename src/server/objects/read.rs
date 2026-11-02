//! JSON API object reads: media downloads and metadata.

use super::gunzipped_response;
use crate::object::ObjectRecord;
use crate::server::RequestBody as Incoming;
use crate::storage::ObjectReader;
use crate::{
    bucket::BucketAccess,
    json::{self, JsonError},
    object::{ObjectConditions, ObjectError},
    server::{
        ALLOWED_RESOURCES_VARY, JSON_RESOURCE_CACHE_CONTROL, ObjectCtx, QueryPrecondition,
        ResponseBody, acl, boxed, empty_response, gzip, insert_content_range,
        insert_unsatisfied_content_range, not_modified_response, object_body,
        read_metadata_response,
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
    json::validate_read_condition_headers(request.headers(), json::EtagConditions::MediaRead)?;
    json::validate_keyed_object_headers(request.headers(), &[crate::csek::DESTINATION_PREFIX])?;
    let mut headers = json::object_media_headers(&record)?;
    insert_provider_headers(&record, &mut headers)?;
    json::insert_response_headers(&record, &mut headers);
    if reader.publicly_readable {
        // An anonymous-capable read revalidates, varies by the allowed-resources header and
        // sends no `pragma` (observed 2026-10-31, anonymous reads only). Whether the header
        // also depends on the caller being anonymous was not observed; the public
        // classification is the local condition.
        headers.remove("pragma");
        headers.append("vary", HeaderValue::from_static(ALLOWED_RESOURCES_VARY));
        headers.insert(
            "cache-control",
            record
                .cache_control
                .as_deref()
                .unwrap_or("public, max-age=3600, must-revalidate")
                .parse()
                .map_err(|_| JsonError::service_unavailable("Invalid stored Cache-Control"))?,
        );
    }
    let transcode = json::object_media_transcoding(request.headers(), &record)?;
    let etag_outcome = crate::http_conditions::etag_not_modified_first(
        request.headers(),
        &json::object_etag(&record),
    );
    let conditional = match etag_outcome {
        Err(crate::http_conditions::EtagError::PreconditionFailed) => {
            return Ok(boxed(precondition_failed(&headers)));
        }
        Err(crate::http_conditions::EtagError::Invalid) => {
            return Err(JsonError::invalid_parameter(
                "Invalid ETag precondition header",
            ));
        }
        Ok(not_modified) => not_modified,
    };
    if conditional
        || params.conditions.if_generation_not_match == Some(record.generation)
        || params.conditions.if_metageneration_not_match == Some(record.metageneration)
    {
        let mut response = not_modified_response(&headers["etag"], &headers["cache-control"]);
        copy_representation_headers(&headers, response.headers_mut());
        return Ok(boxed(response));
    }
    // Cloud Storage refused a ranged read of several ranges instead of ignoring the header.
    if multiple_ranges(request.headers()) {
        return Ok(boxed(multiple_ranges_response(&headers)));
    }
    // A media HEAD sends the GET headers without reading the object (observed 2026-10-31).
    let head = request.method() == Method::HEAD;
    if transcode {
        return gunzipped(ctx, &record, reader, headers, request.method()).await;
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
    if !head {
        compressed_headers(&record, &mut reader, &mut headers).await?;
    }
    let range = match json::object_media_range(request.headers(), record.size) {
        Ok(range) => range,
        Err(error) => {
            // The JSON HEAD 416 states no length: the omitted HTML body is not measured
            // (observed 2026-10-31).
            let mut response = error.into_media_response().map(|body| {
                if head { Full::new(Bytes::new()) } else { body }
                    .map_err(|never| match never {})
                    .boxed_unsync()
            });
            insert_unsatisfied_content_range(response.headers_mut(), record.size);
            return Ok(response);
        }
    };
    if let Some((offset, length)) = range {
        if !head {
            reader
                .set_range(offset, length)
                .await
                .map_err(|_| JsonError::service_unavailable("Cannot seek object data"))?;
        }
        if headers.contains_key("content-length") {
            headers.insert("content-length", length.into());
        }
        insert_content_range(&mut headers, offset, length, record.size);
        range_hashes(&mut headers, &record, offset, length)?;
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

async fn compressed_headers(
    record: &ObjectRecord,
    reader: &mut ObjectReader,
    headers: &mut HeaderMap,
) -> Result<(), JsonError> {
    if !record
        .content_encoding
        .as_deref()
        .is_some_and(|value| value.eq_ignore_ascii_case("gzip"))
        || record.size < 2
        || reader.publicly_readable
        || gzip::is_plain(reader, record.size).await?
    {
        return Ok(());
    }
    // The HTTP/1.1 recording streams private gzip bytes, including partial ranges.
    headers.remove("content-length");
    if !json::object_media_negotiable(record) {
        headers.insert(
            "cache-control",
            HeaderValue::from_static(
                "no-cache, no-store, max-age=0, must-revalidate, no-transform",
            ),
        );
    }
    Ok(())
}

/// A gunzipped media response: Range is ignored and the whole expanded object is sent with
/// 200, unless the stored bytes are not gzip, which fails before any header is sent.
async fn gunzipped(
    ctx: ObjectCtx<'_>,
    record: &ObjectRecord,
    mut reader: ObjectReader,
    mut headers: HeaderMap,
    method: &Method,
) -> Result<Response<ResponseBody>, JsonError> {
    let head = method == Method::HEAD;
    if !head && gzip::is_plain(&mut reader, record.size).await? {
        return Ok(gzip::plain_body_rejection(headers));
    }
    gzip::insert_transcoded_headers(&mut headers);
    Ok(gunzipped_response(ctx, method, reader, headers, record))
}

/// The headers a conditional media response (304 or 412) keeps: the representation's type,
/// modification date and `x-goog-*` description, but neither its hashes nor its disposition.
fn copy_representation_headers(from: &HeaderMap, to: &mut HeaderMap) {
    for name in [
        "content-type",
        "last-modified",
        "x-goog-generation",
        "x-goog-metageneration",
        "x-goog-storage-class",
        "x-goog-stored-content-encoding",
        "x-goog-stored-content-length",
        "pragma",
    ] {
        if let Some(value) = from.get(name) {
            to.insert(name, value.clone());
        }
    }
}

/// A plain-text media failure that keeps the object's `x-goog-*` description.
fn plain_media_failure(
    status: StatusCode,
    body: &'static str,
    representation: &HeaderMap,
) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    copy_representation_headers(representation, headers);
    headers.insert(
        "content-type",
        HeaderValue::from_static("text/html; charset=UTF-8"),
    );
    if let Some(etag) = representation.get("etag") {
        headers.insert("etag", etag.clone());
    }
    // A failure that depends on a request header must never be stored by a shared cache, even
    // for a public object whose 200 is cacheable (recorded: no-store, no-cache).
    headers.insert(
        "cache-control",
        HeaderValue::from_static(crate::json::NO_STORE_CACHE_CONTROL),
    );
    headers.insert("pragma", HeaderValue::from_static("no-cache"));
    response
}

fn precondition_failed(representation: &HeaderMap) -> Response<Full<Bytes>> {
    plain_media_failure(
        StatusCode::PRECONDITION_FAILED,
        "Precondition Failed",
        representation,
    )
}

fn multiple_ranges_response(representation: &HeaderMap) -> Response<Full<Bytes>> {
    let mut response = plain_media_failure(
        StatusCode::NOT_IMPLEMENTED,
        "Multiple Ranges are not supported",
        representation,
    );
    for name in [
        "last-modified",
        "x-goog-generation",
        "x-goog-metageneration",
        "x-goog-storage-class",
        "x-goog-stored-content-encoding",
        "x-goog-stored-content-length",
        "etag",
        "pragma",
    ] {
        response.headers_mut().remove(name);
    }
    // Recorded: the unsupported-range failure is cacheable by no shared cache, like the other
    // plain media failures that do not depend on a request validator.
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static(crate::json::MEDIA_ERROR_CACHE_CONTROL),
    );
    response
}

/// Whether the request asks for several byte ranges at once.
fn multiple_ranges(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all("range").iter();
    match (values.next(), values.next()) {
        (Some(value), None) => value.to_str().ok().is_some_and(|value| {
            value.trim().split_once('=').is_some_and(|(unit, ranges)| {
                unit.eq_ignore_ascii_case("bytes") && ranges.contains(',')
            })
        }),
        _ => false,
    }
}

/// Fields the JSON API sends on every media response beyond the representation headers
/// (observed 2026-10-31): the modification date, the storage class, `pragma: no-cache` and
/// `content-disposition: attachment`. An object that sets its own disposition keeps it; XML
/// reads send neither of the last two.
fn insert_provider_headers(
    record: &ObjectRecord,
    headers: &mut HeaderMap,
) -> Result<(), JsonError> {
    let invalid = JsonError::invalid_stored_header;
    headers.insert(
        "last-modified",
        HeaderValue::try_from(json::http_date(record.updated)?).map_err(|_| invalid())?,
    );
    headers.insert(
        "x-goog-storage-class",
        HeaderValue::try_from(record.storage_class.as_str()).map_err(|_| invalid())?,
    );
    headers.insert("pragma", HeaderValue::from_static("no-cache"));
    if !headers.contains_key("content-disposition") {
        headers.insert(
            "content-disposition",
            HeaderValue::from_static("attachment"),
        );
    }
    Ok(())
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
            HeaderValue::try_from(crc32c).map_err(|_| JsonError::invalid_stored_header())?,
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
    json::validate_keyed_object_headers(request.headers(), &[crate::csek::DESTINATION_PREFIX])?;
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
        JSON_RESOURCE_CACHE_CONTROL,
    )?))
}
