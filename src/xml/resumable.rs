//! XML resumable uploads: `POST Object` with `x-goog-resumable: start`, directly or through a
//! signed URL, and the session URI it returns. Sessions, transfers and completion are the
//! shared resumable engine; this adapter only parses XML requests and renders XML responses.

use super::{Error, ObjectPath, XmlCode, put, signing_parameter};
use crate::server::RequestBody as Incoming;
use crate::{
    auth, json,
    server::{
        ObjectCtx, ResponseBody, anonymous_error, boxed, empty_response,
        resumable::{self, SessionTarget, Transfer},
    },
    storage::{ResumableError, ResumableOptions},
};
use hyper::{
    HeaderMap, Method, Request, Response, StatusCode, Uri,
    header::{HeaderValue, LOCATION},
};

/// The two XML resumable operations on an object path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Route {
    /// `POST /BUCKET/OBJECT` with `x-goog-resumable`, authenticated like any XML request.
    Start,
    /// A request to the session URI, `/BUCKET/OBJECT?upload_id=...`, which is its own
    /// capability and carries no credentials.
    Session,
}

pub(crate) fn route(method: &Method, uri: &Uri, headers: &HeaderMap) -> Option<Route> {
    if uri.query().is_some_and(|query| {
        query
            .split('&')
            .any(|pair| pair.split('=').next() == Some("upload_id"))
    }) {
        Some(Route::Session)
    } else if *method == Method::POST && headers.contains_key("x-goog-resumable") {
        Some(Route::Start)
    } else {
        None
    }
}

/// The bucket and decoded object name of an XML resumable request.
fn target(object: &ObjectPath, ctx: ObjectCtx<'_>) -> Result<String, Error> {
    json::validate_bucket_name_for_profile(&object.bucket, ctx.config.compat_profile)?;
    let name = json::decode(object.name(), json::Decoding::Path)?;
    crate::object::name::validate(&name)
        .map_err(|_| json::JsonError::invalid_parameter("Invalid object name"))?;
    Ok(name)
}

/// Whether the initiation authenticated through a signature, which admits its signing headers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Signing {
    Signed,
    Unsigned,
}

/// Rejects initiation headers outside the supported subset instead of ignoring their effect.
/// Whole-object checksums are not documented for initiation, so they stay unsupported here.
fn reject_unsupported_headers(headers: &HeaderMap, signing: Signing) -> Result<(), Error> {
    for key in headers.keys() {
        let name = key.as_str();
        let goog = name.starts_with("x-goog-")
            && !name.starts_with("x-goog-meta-")
            && !super::is_common_object_header(name)
            && !(signing == Signing::Signed && auth::signature::authentication_header(name))
            && !matches!(name, "x-goog-resumable" | "x-goog-storage-class");
        let amz = name.starts_with("x-amz-")
            && !(signing == Signing::Signed && auth::signature::authentication_header(name));
        if goog
            || amz
            || super::HTTP_PRECONDITION_HEADERS.contains(&name)
            || matches!(
                name,
                "x-goog-hash" | "content-md5" | "content-range" | "range" | "trailer"
            )
        {
            return Err(Error::unsupported(name));
        }
    }
    Ok(())
}

/// Builds the session options from the initiation headers, which carry the object metadata
/// and preconditions the XML API accepts on `POST Object`.
fn options(
    headers: &HeaderMap,
    signing: Signing,
    access: crate::bucket::BucketAccess,
) -> Result<ResumableOptions, Error> {
    reject_unsupported_headers(headers, signing)?;
    if auth::signature::resumable_start(headers) != Ok(true) {
        return Err(Error::invalid("x-goog-resumable must be start."));
    }
    let conditions = put::conditions(headers)?;
    let (content_type, expected) = put::content_type_and_hashes(headers, conditions)?;
    let mut options = ResumableOptions::new(access);
    options.content_type = content_type;
    options.expected = expected;
    options.conditions = conditions;
    options.metadata = put::metadata(headers)?;
    options.origin = put::single(headers, "origin")?.map(str::to_owned);
    Ok(options)
}

pub(crate) async fn start(
    request: Request<Incoming>,
    object: &ObjectPath,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let caller = ctx.caller;
    let name = target(object, ctx)?;
    // Signing parameters authenticate the request; any other query selects another operation.
    let query =
        auth::signature::xml_query(request.uri().query().unwrap_or_default(), request.headers())?;
    if let Some(key) = query.keys().find(|key| !signing_parameter(key)) {
        return Err(Error::unsupported(key));
    }
    let access = ctx
        .authorize(auth::OBJECTS_CREATE, &object.bucket)
        .await
        .map_err(|error| anonymous_error(error, caller))?;
    let signing = if auth::signature::is_signed(request.uri().query())
        || auth::signature::has_header(request.headers())
    {
        Signing::Signed
    } else {
        Signing::Unsigned
    };
    let options = options(request.headers(), signing, access)?;
    json::read_body(request.into_body(), 0)
        .await
        .map_err(|_| Error::invalid("Resumable upload initiation carries no body."))?;
    let id = resumable::create_session(ctx, &object.bucket, &name, options, None)
        .await
        .map_err(|error| Error::from(anonymous_error(error, caller)))?;
    // The raw path keeps the client's object-name encoding; the session URI is path-style on
    // the advertised endpoint, which serves every bucket.
    let location = format!(
        "{}/{}/{}?upload_id={id}",
        ctx.endpoints.api_base_url,
        object.bucket,
        object.name()
    );
    let location =
        HeaderValue::try_from(location).map_err(|_| Error::invalid("Invalid session endpoint."))?;
    let mut response = empty_response(StatusCode::CREATED);
    let headers = response.headers_mut();
    headers.insert(LOCATION, location);
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert("content-length", HeaderValue::from_static("0"));
    Ok(boxed(response))
}

/// Serves the session URI. Google answers a cancelled XML session with 204, both to the
/// cancellation and to every later request.
pub(crate) async fn session(
    request: Request<Incoming>,
    object: &ObjectPath,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let name = target(object, ctx)?;
    let mut query = json::parse_query(request.uri().query().unwrap_or_default())?;
    let id = query
        .remove("upload_id")
        .ok_or_else(|| Error::invalid("Missing upload_id."))?;
    if let Some(key) = query.keys().next() {
        return Err(Error::unsupported(key));
    }
    let target = SessionTarget {
        bucket: &object.bucket,
        id: &id,
        name: Some(&name),
    };
    let result = match *request.method() {
        Method::PUT => resumable::transfer(request, target, ctx).await.map(Some),
        Method::DELETE => cancel(request, target, ctx).await.map(|()| None),
        _ => {
            return Err(Error::unsupported(
                "resumable session method; use PUT or DELETE on the session URI",
            ));
        }
    };
    match result {
        Ok(Some(Transfer::Incomplete(response))) => Ok(response),
        Ok(Some(Transfer::Complete(value))) => completed(&value),
        Ok(None) => Ok(no_content()),
        Err(error) if error.status == StatusCode::GONE => Ok(no_content()),
        Err(error) if error.kind == json::ErrorKind::CancelledUpload => Ok(no_content()),
        Err(error) if error.status == StatusCode::NOT_FOUND => Err(Error::new(
            XmlCode::NoSuchUpload,
            "The upload session does not exist or has expired.",
        )),
        Err(error) => Err(error.into()),
    }
}

async fn cancel(
    request: Request<Incoming>,
    target: SessionTarget<'_>,
    ctx: ObjectCtx<'_>,
) -> Result<(), json::JsonError> {
    let catalog = &ctx.state.catalog;
    json::read_body(request.into_body(), 0)
        .await
        .map_err(|_| json::JsonError::invalid_parameter("Cancellation requires an empty body"))?;
    let session = ctx
        .await_catalog(catalog.resumable_session(target.id, target.bucket))
        .await?;
    if target.name.is_some_and(|name| name != session.name) {
        return Err(ResumableError::NotFound.into());
    }
    ctx.await_catalog(catalog.cancel_resumable_session(target.id, target.bucket))
        .await?;
    Ok(())
}

fn no_content() -> Response<ResponseBody> {
    let mut response = empty_response(StatusCode::NO_CONTENT);
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    boxed(response)
}

/// Renders the final XML response from the stored JSON object resource, which every protocol
/// replays from: XML reports the stored object through headers and returns no body.
fn completed(value: &serde_json::Value) -> Result<Response<ResponseBody>, Error> {
    let text = |field: &str| value.get(field).and_then(serde_json::Value::as_str);
    let unavailable = || {
        Error::new(
            XmlCode::ServiceUnavailable,
            "The completed upload has no stored object response.",
        )
    };
    let (generation, metageneration, size, crc32c, etag) = (
        text("generation").ok_or_else(unavailable)?,
        text("metageneration").ok_or_else(unavailable)?,
        text("size").ok_or_else(unavailable)?,
        text("crc32c").ok_or_else(unavailable)?,
        text("etag").ok_or_else(unavailable)?,
    );
    let md5 = text("md5Hash");
    // XML reports a plain object's MD5 as its ETag; other objects quote the JSON validator,
    // matching `xml::object_etag`.
    let plain = value.get("customerEncryption").is_none() && value.get("componentCount").is_none();
    let etag = match md5 {
        Some(md5) if plain => format!(
            "\"{}\"",
            crate::hashing::encode_lower(&json::decode_hash::<16>(md5)?)
        ),
        _ => format!("\"{etag}\""),
    };
    let hash = md5.map_or_else(
        || format!("crc32c={crc32c}"),
        |md5| format!("crc32c={crc32c},md5={md5}"),
    );
    let mut response = empty_response(StatusCode::OK);
    let headers = response.headers_mut();
    for (name, value) in [
        ("etag", etag.as_str()),
        ("x-goog-generation", generation),
        ("x-goog-metageneration", metageneration),
        ("x-goog-hash", hash.as_str()),
        ("x-goog-stored-content-length", size),
        (
            "x-goog-stored-content-encoding",
            text("contentEncoding").unwrap_or("identity"),
        ),
        ("cache-control", "no-store"),
    ] {
        headers.insert(
            name,
            HeaderValue::from_str(value).map_err(|_| unavailable())?,
        );
    }
    Ok(boxed(response))
}
