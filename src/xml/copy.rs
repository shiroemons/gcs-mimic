//! XML copy parsing; byte transfer, integrity and atomic publication use the shared engine.

use super::{Error, MinValue, ReadRequest, element, number, put};
use crate::{
    auth,
    bucket::BucketAccess,
    json,
    object::{ObjectConditions, ObjectRecord},
    server::{
        ObjectCtx, RequestBody, ResponseBody, anonymous_error, boxed,
        copy::{self, CopyBudget, CopyJob},
    },
    storage::DerivedDestination,
};
use http_body_util::Full;
use hyper::{HeaderMap, Request, Response, StatusCode, body::Bytes, header::HeaderValue};

const SOURCE_FIELDS: &[&str] = &[
    "x-goog-copy-source",
    "x-goog-copy-source-generation",
    "x-goog-copy-source-if-generation-match",
    "x-goog-copy-source-if-metageneration-match",
    "x-goog-copy-source-if-match",
    "x-goog-copy-source-if-none-match",
    "x-goog-metadata-directive",
];

struct Source {
    bucket: String,
    name: String,
    generation: Option<i64>,
    conditions: ObjectConditions,
}

#[derive(Clone, Copy)]
enum MetadataDirective {
    Copy,
    Replace,
}

fn invalid(message: &str) -> Error {
    Error::new(StatusCode::BAD_REQUEST, "InvalidArgument", message)
}

fn source(headers: &HeaderMap, ctx: ObjectCtx<'_>) -> Result<Source, Error> {
    let value = put::single(headers, "x-goog-copy-source")?
        .ok_or_else(|| invalid("x-goog-copy-source is required"))?;
    if value.contains(['?', '#']) {
        return Err(invalid(
            "Percent-encode copy source names; select a generation with x-goog-copy-source-generation",
        ));
    }
    let (bucket, name) = value
        .strip_prefix('/')
        .unwrap_or(value)
        .split_once('/')
        .ok_or_else(|| invalid("x-goog-copy-source must be /BUCKET/OBJECT"))?;
    let bucket = json::decode(bucket, json::Decoding::Path)?;
    let name = json::decode(name, json::Decoding::Path)?;
    json::validate_bucket_name_for_profile(&bucket, ctx.config.compat_profile)?;
    json::validate_object_name(&name)?;
    let generation = source_number(headers, "x-goog-copy-source-generation")?;
    let conditions = ObjectConditions {
        if_generation_match: source_number(headers, "x-goog-copy-source-if-generation-match")?,
        if_metageneration_match: source_number(
            headers,
            "x-goog-copy-source-if-metageneration-match",
        )?,
        ..Default::default()
    };
    if conditions.if_metageneration_match.is_some()
        && generation.is_none()
        && conditions.if_generation_match.is_none()
    {
        return Err(invalid(
            "A source metageneration condition requires a source generation or generation condition",
        ));
    }
    Ok(Source {
        bucket,
        name,
        generation,
        conditions,
    })
}

fn source_number(headers: &HeaderMap, name: &str) -> Result<Option<i64>, Error> {
    put::single(headers, name)?
        .map(|value| number(value, MinValue::One))
        .transpose()
}

fn validate_source_etags(headers: &HeaderMap, record: &ObjectRecord) -> Result<(), Error> {
    let expected = super::object_etag(record);
    for (name, must_match) in [
        ("x-goog-copy-source-if-match", true),
        ("x-goog-copy-source-if-none-match", false),
    ] {
        if let Some(value) = put::single(headers, name)? {
            if value.is_empty() || value.contains(',') || value.starts_with("W/") || value == "*" {
                return Err(invalid(
                    "Copy source ETag conditions require one strong ETag",
                ));
            }
            let matches = value.trim_matches('"') == expected.trim_matches('"');
            if matches != must_match {
                return Err(Error::new(
                    StatusCode::PRECONDITION_FAILED,
                    "PreconditionFailed",
                    "The source ETag condition was not met",
                ));
            }
        }
    }
    Ok(())
}

fn options(headers: &HeaderMap, signing: put::Signing) -> Result<put::Headers, Error> {
    if headers.contains_key("x-goog-gcs-idempotency-token") {
        return Err(Error::unsupported(
            "copy invocation-token replay; use destination generation conditions",
        ));
    }
    let mut selected = headers.clone();
    for name in SOURCE_FIELDS {
        selected.remove(*name);
    }
    for name in headers.keys() {
        if crate::csek::is_key_header(name.as_str(), crate::csek::SOURCE_PREFIX) {
            selected.remove(name);
        }
    }
    let options = put::headers(&selected, 0, signing)?;
    if options.sha256.is_some_and(|hash| {
        hash.as_slice() != ring::digest::digest(&ring::digest::SHA256, b"").as_ref()
    }) {
        return Err(Error::new(
            StatusCode::BAD_REQUEST,
            "BadDigest",
            "Copy requires an empty request body",
        ));
    }
    Ok(options)
}

pub(super) async fn respond(
    request: Request<RequestBody>,
    operation: ReadRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, Error> {
    let access = ctx
        .authorize(auth::OBJECTS_CREATE, &operation.bucket)
        .await
        .map_err(|error| anonymous_error(error, ctx.caller))?;
    let source = source(request.headers(), ctx)?;
    let source_access = ctx
        .authorize(auth::OBJECTS_GET, &source.bucket)
        .await
        .map_err(|error| anonymous_error(error, ctx.caller))?;
    let directive = match put::single(request.headers(), "x-goog-metadata-directive")? {
        None | Some("COPY") => MetadataDirective::Copy,
        Some("REPLACE") => MetadataDirective::Replace,
        Some(_) => return Err(invalid("x-goog-metadata-directive must be COPY or REPLACE")),
    };
    if source.bucket == operation.bucket && source.name == operation.name {
        return Err(Error::unsupported(
            "self-copy metadata updates; use the JSON object metadata update API",
        ));
    }
    let signing = if auth::signature::is_signed(request.uri().query())
        || auth::signature::has_header(request.headers())
    {
        put::Signing::SignedUrl
    } else {
        put::Signing::Header
    };
    let options = options(request.headers(), signing)?;
    let key = json::source_key(request.headers())?;
    let source = ctx
        .await_catalog(ctx.state.catalog.resolve_derived_source_keyed(
            &source.bucket,
            &source.name,
            source.generation,
            source.conditions,
            &source_access,
            key,
        ))
        .await
        .map_err(|error| put::storage_error(error, ctx.caller))?;
    validate_source_etags(request.headers(), &source.object)?;
    json::read_body(request.into_body(), 0).await?;
    let modified = source.object.updated;
    let destination = destination(operation, &source.object, options, directive, access)?;
    let source_location = if source.object.component_count.is_some() {
        Some(
            ctx.state
                .catalog
                .get_bucket(&source.object.bucket)
                .await
                .map_err(json::JsonError::from)?
                .ok_or_else(|| {
                    put::storage_error(crate::object::ObjectError::BucketNotFound, ctx.caller)
                })?
                .location,
        )
    } else {
        None
    };
    let record = copy::execute(
        &ctx.state.catalog,
        CopyJob {
            source,
            source_access,
            destination,
            source_location,
            replay: None,
        },
        CopyBudget {
            config: ctx.config,
            deadline: ctx.deadline,
            activity: ctx.activity,
        },
        #[cfg(test)]
        None,
    )
    .await
    .map_err(|error| put::storage_error(error, ctx.caller))?;
    response(&record, modified)
}

fn destination(
    operation: ReadRequest,
    source: &ObjectRecord,
    mut options: put::Headers,
    directive: MetadataDirective,
    access: BucketAccess,
) -> Result<DerivedDestination, Error> {
    let destination_key = options.metadata.customer_key_sha256;
    let storage_class = options
        .metadata
        .storage_class
        .take()
        .or_else(|| Some(source.storage_class.clone()));
    let mut metadata = if matches!(directive, MetadataDirective::Replace) {
        options.metadata.replace = true;
        options.metadata.content_type = Some(Some(if options.content_type.is_empty() {
            "application/octet-stream".into()
        } else {
            options.content_type
        }));
        options.metadata
    } else {
        let mut inherited = json::derived_metadata(b"", source)?.0;
        inherited.custom_time = Some(source.custom_time.clone());
        inherited
    };
    metadata.customer_key_sha256 = destination_key;
    let expected = crate::object::ExpectedHashes {
        md5: options.expected.md5.or(Some(source.md5)),
        crc32c: options.expected.crc32c.or(Some(source.crc32c)),
    };
    Ok(DerivedDestination {
        bucket: operation.bucket,
        name: operation.name,
        content_type: String::new(),
        metadata,
        storage_class,
        expected,
        conditions: options.conditions,
        access,
        component_count: None,
    })
}

fn response(
    record: &ObjectRecord,
    modified: std::time::SystemTime,
) -> Result<Response<ResponseBody>, Error> {
    let mut body = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?><CopyObjectResult>");
    element(&mut body, "LastModified", &json::timestamp(modified)?);
    element(&mut body, "ETag", &super::object_etag(record));
    body.push_str("</CopyObjectResult>");
    let mut response = Response::new(Full::new(Bytes::from(body)));
    let mut headers = super::media_headers(record, super::CacheScope::Private)?;
    for name in [
        "etag",
        "x-goog-generation",
        "x-goog-metageneration",
        "x-goog-hash",
        "x-goog-component-count",
    ] {
        if let Some(value) = headers.remove(name) {
            response.headers_mut().insert(name, value);
        }
    }
    json::insert_response_headers(record, response.headers_mut());
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("application/xml; charset=UTF-8"),
    );
    Ok(boxed(response))
}
