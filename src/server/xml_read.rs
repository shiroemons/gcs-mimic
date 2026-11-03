//! XML API object GET, HEAD and DELETE.

use super::RequestBody as Incoming;
use super::{
    ObjectCtx, ResponseBody, anonymous_error, boxed, empty_response, gzip, insert_content_range,
    object_body,
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
mod tests {
    use super::*;
    use crate::{
        auth::Auth,
        config::Config,
        server::{ObjectActivity, RequestCtx, State},
        storage::Catalog,
    };
    use http_body_util::{BodyExt, Full};

    #[tokio::test]
    async fn xml_read_validators_wrap_routed_public_and_virtual_host_responses() {
        let dir = crate::test_support::TestDir::new();
        let state = State::for_test(Catalog::open(&dir.0).await.unwrap(), Auth::permissive());
        let config = Config {
            xml_virtual_host_domains: vec!["storage.invalid".into()],
            ..Config::default()
        };
        let endpoints = config
            .runtime_endpoints
            .snapshot(&config, "http://localhost")
            .unwrap();
        let (activity, _) = tokio::sync::watch::channel(ObjectActivity::default());
        let ctx = RequestCtx {
            state: &state,
            config: &config,
            endpoints: &endpoints,
            activity: &activity,
        };
        for (method, uri, data) in [
            (
                "POST",
                "/storage/v1/b?project=local",
                br#"{"name":"bucket"}"#.as_slice(),
            ),
            (
                "POST",
                "/upload/storage/v1/b/bucket/o?uploadType=media&name=object",
                b"body",
            ),
            (
                "PUT",
                "/storage/v1/b/bucket/iam",
                br#"{"bindings":[{"role":"roles/storage.objectViewer","members":["allUsers"]}]}"#,
            ),
        ] {
            let request = Request::builder()
                .method(method)
                .uri(uri)
                .body(Incoming::Buffered(Full::new(
                    bytes::Bytes::copy_from_slice(data),
                )))
                .unwrap();
            let response = crate::server::routing::respond(request, ctx).await;
            assert_eq!(response.status(), StatusCode::OK);
        }
        let request = Request::builder()
            .uri("/bucket/object")
            .body(Incoming::Buffered(Full::new(bytes::Bytes::new())))
            .unwrap();
        let full = crate::server::routing::respond(request, ctx).await;
        assert_eq!(full.status(), StatusCode::OK);
        let etag = full.headers()["etag"].to_str().unwrap().to_owned();
        let modified = full.headers()["last-modified"].clone();
        for method in ["GET", "HEAD"] {
            for (host, path) in [
                ("localhost", "/bucket/object"),
                ("bucket.storage.invalid", "/object"),
            ] {
                let request = Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", host)
                    .header("if-none-match", format!("W/{etag}"))
                    .header("range", "bytes=999-")
                    .body(Incoming::Buffered(Full::new(bytes::Bytes::new())))
                    .unwrap();
                let response = crate::server::routing::respond(request, ctx).await;
                assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
                assert_eq!(response.headers()["etag"], etag);
                assert_eq!(response.headers()["cache-control"], "public, max-age=3600");
                assert_eq!(response.headers()["last-modified"], modified);
                assert!(
                    response
                        .into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes()
                        .is_empty()
                );
            }
        }
        let request = Request::builder()
            .uri("/bucket/object")
            .header("if-match", "\"other\"")
            .header("if-none-match", etag)
            .body(Incoming::Buffered(Full::new(bytes::Bytes::new())))
            .unwrap();
        assert_eq!(
            crate::server::routing::respond(request, ctx).await.status(),
            StatusCode::PRECONDITION_FAILED
        );
        let request = Request::builder()
            .uri("/bucket/object")
            .header("range", "bytes=0-0,2-2")
            .header("range", "bytes=1-1")
            .body(Incoming::Buffered(Full::new(bytes::Bytes::new())))
            .unwrap();
        let response = crate::server::routing::respond(request, ctx).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            b"body"[..]
        );
        state.catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn encrypted_head_validators_do_not_bypass_data_key_checks() {
        let dir = crate::test_support::TestDir::new();
        let state = State::for_test(Catalog::open(&dir.0).await.unwrap(), Auth::permissive());
        let config = Config::default();
        let endpoints = config
            .runtime_endpoints
            .snapshot(&config, "http://localhost")
            .unwrap();
        let (activity, _) = tokio::sync::watch::channel(ObjectActivity::default());
        let ctx = RequestCtx {
            state: &state,
            config: &config,
            endpoints: &endpoints,
            activity: &activity,
        };
        let request = Request::builder()
            .method("POST")
            .uri("/storage/v1/b?project=local")
            .body(Incoming::Buffered(Full::new(bytes::Bytes::from_static(
                br#"{"name":"bucket"}"#,
            ))))
            .unwrap();
        assert_eq!(
            crate::server::routing::respond(request, ctx).await.status(),
            StatusCode::OK
        );
        let raw_key = [7; 32];
        let key = crate::base64::encode(&raw_key);
        let hash =
            crate::base64::encode(ring::digest::digest(&ring::digest::SHA256, &raw_key).as_ref());
        let request = Request::builder()
            .method("POST")
            .uri("/upload/storage/v1/b/bucket/o?uploadType=media&name=encrypted")
            .header("x-goog-encryption-algorithm", "AES256")
            .header("x-goog-encryption-key", key)
            .header("x-goog-encryption-key-sha256", hash)
            .body(Incoming::Buffered(Full::new(bytes::Bytes::from_static(
                b"body",
            ))))
            .unwrap();
        assert_eq!(
            crate::server::routing::respond(request, ctx).await.status(),
            StatusCode::OK
        );
        let request = Request::builder()
            .method("HEAD")
            .uri("/bucket/encrypted")
            .body(Incoming::Buffered(Full::new(bytes::Bytes::new())))
            .unwrap();
        let head = crate::server::routing::respond(request, ctx).await;
        assert_eq!(head.status(), StatusCode::OK);
        assert!(!head.headers().contains_key("x-goog-hash"));
        let etag = head.headers()["etag"].clone();
        for (method, status) in [
            ("HEAD", StatusCode::NOT_MODIFIED),
            ("GET", StatusCode::BAD_REQUEST),
        ] {
            let request = Request::builder()
                .method(method)
                .uri("/bucket/encrypted")
                .header("if-none-match", etag.clone())
                .body(Incoming::Buffered(Full::new(bytes::Bytes::new())))
                .unwrap();
            let response = crate::server::routing::respond(request, ctx).await;
            assert_eq!(response.status(), status);
            assert!(!response.headers().contains_key("x-goog-hash"));
            if status == StatusCode::NOT_MODIFIED {
                assert_eq!(response.headers()["etag"], etag);
                assert!(response.headers().contains_key("expires"));
                assert!(
                    response
                        .into_body()
                        .collect()
                        .await
                        .unwrap()
                        .to_bytes()
                        .is_empty()
                );
            }
        }
        state.catalog.close().await.unwrap();
    }
}

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
        ctx.state
            .catalog
            .delete_object(operation.target, conditions, &access)
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
            .open_object_keyed(target, conditions, access, key)
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
