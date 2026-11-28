//! XML copy parsing; byte transfer, integrity and atomic publication use the shared engine.

use super::{Error, Namespace, ReadRequest, XmlCode, XmlWriter, number, put};
use crate::{
    auth,
    bucket::{BucketAccess, name::BucketKey},
    object::{
        ObjectConditions, ObjectMetadataUpdate, ObjectRecord, ObjectTarget, Patch, WriteMode,
        content_type,
        generation::{Generation, Metageneration},
    },
    service::{
        body::{RequestBody, ResponseBody, boxed},
        context::ObjectCtx,
        copy::{self, CopyBudget, CopyJob},
    },
    storage::DerivedDestination,
};
use hyper::{HeaderMap, Request, Response};

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
    bucket: BucketKey,
    name: String,
    generation: Option<Generation>,
    conditions: ObjectConditions,
}

#[derive(Clone, Copy)]
enum MetadataDirective {
    Copy,
    Replace,
}

fn source(headers: &HeaderMap, ctx: ObjectCtx<'_>) -> Result<Source, Error> {
    let value = put::single(headers, "x-goog-copy-source")?
        .ok_or_else(|| Error::invalid("x-goog-copy-source is required"))?;
    if value.contains(['?', '#']) {
        return Err(Error::invalid(
            "Percent-encode copy source names; select a generation with x-goog-copy-source-generation",
        ));
    }
    let (bucket, name) = value
        .trim_prefix('/')
        .split_once('/')
        .ok_or_else(|| Error::invalid("x-goog-copy-source must be /BUCKET/OBJECT"))?;
    let bucket = crate::percent::decode(bucket, crate::percent::Decoding::Path)?;
    let name = crate::percent::decode(name, crate::percent::Decoding::Path)?;
    let bucket = BucketKey::for_profile(bucket, ctx.config.compat_profile())?;
    crate::object::name::validate(&name)?;
    let generation = source_number(headers, "x-goog-copy-source-generation")?;
    let conditions = ObjectConditions {
        if_generation_match: source_number(headers, "x-goog-copy-source-if-generation-match")?
            .map(Generation::get),
        if_metageneration_match: source_number(
            headers,
            "x-goog-copy-source-if-metageneration-match",
        )?
        .map(Metageneration::get),
        ..Default::default()
    };
    if conditions.if_metageneration_match.is_some()
        && generation.is_none()
        && conditions.if_generation_match.is_none()
    {
        return Err(Error::invalid(
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

fn source_number<T: TryFrom<i64>>(headers: &HeaderMap, name: &str) -> Result<Option<T>, Error> {
    put::single(headers, name)?.map(number).transpose()
}

fn validate_source_etags(headers: &HeaderMap, record: &ObjectRecord) -> Result<(), Error> {
    let expected = super::object_etag(record);
    for (name, must_match) in [
        ("x-goog-copy-source-if-match", true),
        ("x-goog-copy-source-if-none-match", false),
    ] {
        if let Some(value) = put::single(headers, name)? {
            if value.is_empty() || value.contains(',') || value.starts_with("W/") || value == "*" {
                return Err(Error::invalid(
                    "Copy source ETag conditions require one strong ETag",
                ));
            }
            let matches = value.trim_matches('"') == expected.trim_matches('"');
            if matches != must_match {
                // Cloud Storage answered `x-goog-copy-source-if-match: "bogus"`, an ETag that is not
                // shaped like an MD5, with 400 InvalidArgument (recorded). A well-formed ETag that
                // merely differs, and `if-none-match`, were not recorded; they are assumed to fail
                // the same way.
                return Err(Error::invalid("Invalid argument."));
            }
        }
    }
    Ok(())
}

fn options(headers: &HeaderMap, signing: put::Signing) -> Result<put::Headers, Error> {
    if headers.contains_key(crate::object::IdempotencyToken::HEADER) {
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
    // Cloud Storage ignored a range the copied object exceeded (recorded 2026-11-04 in the
    // `xml-content-length-range` differential case): it limits a request body, which a copy lacks.
    selected.remove(put::length_range::HEADER);
    let options = put::headers(&selected, 0, signing, put::Framing::Required)?;
    if super::sha256_mismatch(options.sha256, b"") {
        return Err(Error::new(
            XmlCode::BadDigest,
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
        .authorize(auth::OBJECTS_CREATE, operation.target.bucket.as_str())
        .await
        .map_err(|error| Error::from(error).for_caller(ctx.caller))?;
    let source = source(request.headers(), ctx)?;
    let (source_bucket, source_name) = (source.bucket.clone(), source.name.clone());
    let source_access = ctx
        .authorize(auth::OBJECTS_GET, source.bucket.as_str())
        .await
        .map_err(|error| Error::from(error).for_caller(ctx.caller))?;
    let directive = match put::single(request.headers(), "x-goog-metadata-directive")? {
        None | Some("COPY") => MetadataDirective::Copy,
        Some("REPLACE") => MetadataDirective::Replace,
        Some(_) => {
            return Err(Error::invalid(
                "x-goog-metadata-directive must be COPY or REPLACE",
            ));
        }
    };
    if source.bucket == operation.target.bucket && source.name == operation.target.name {
        return Err(Error::unsupported(
            "self-copy metadata updates; use the JSON object metadata update API",
        ));
    }
    let options = options(request.headers(), put::Signing::of(&request))?;
    super::canned_acl::check(
        options.metadata.acl.predefined(),
        operation.target.bucket.as_str(),
        &access,
        ctx,
    )
    .await?;
    let key = crate::csek::source_key(request.headers())?;
    let target = ObjectTarget::from_bucket(source.bucket, source.name, source.generation);
    let source = ctx
        .await_catalog(ctx.state.catalog.resolve_derived_source_keyed(
            target.into_signed_target(),
            source.conditions,
            &source_access,
            key,
        ))
        .await
        .map_err(|error| {
            put::storage_error(error, ctx.caller)
                .naming_object(source_bucket.as_str(), &source_name)
        })?;
    validate_source_etags(request.headers(), &source.object)?;
    crate::bounded_body::read_body(request.into_body(), 0).await?;
    let modified = source.object.updated;
    let destination = destination(operation, &source.object, options, directive, access)?;
    let source_location = if source.object.component_count.is_some() {
        Some(
            ctx.state
                .catalog
                .get_bucket(&source.object.bucket, &BucketAccess::Permissive)
                .await?
                .ok_or_else(|| {
                    put::storage_error(crate::object::ObjectError::BucketNotFound, ctx.caller)
                })?
                .location
                .as_str()
                .to_owned(),
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
    let acl = std::mem::take(&mut options.metadata.acl);
    let storage_class = options
        .metadata
        .storage_class
        .take()
        .or(Some(source.storage_class));
    let mut metadata = if matches!(directive, MetadataDirective::Replace) {
        options.metadata.write = WriteMode::Replace;
        options.metadata.content_type = Patch::Set(content_type::or_default(options.content_type));
        options.metadata
    } else {
        // Publishing checks the encoded size as well, but only after the whole body is copied.
        let mut inherited = ObjectMetadataUpdate::inheriting(source);
        inherited.check_encoded_size()?;
        inherited.custom_time = Patch::nullable(source.custom_time.clone());
        inherited
    };
    metadata.customer_key_sha256 = destination_key;
    // The directive selects metadata; the ACL applies to the destination under either value.
    metadata.acl = acl;
    Ok(DerivedDestination {
        bucket: operation.target.bucket.into_inner(),
        name: operation.target.name,
        content_type: String::new(),
        metadata,
        storage_class,
        declared: options.expected,
        recorded: source.hashes().into(),
        conditions: options.conditions,
        access,
        component_count: None,
    })
}

pub(super) fn response(
    record: &ObjectRecord,
    modified: std::time::SystemTime,
) -> Result<Response<ResponseBody>, Error> {
    let mut document = XmlWriter::new("CopyObjectResult", Namespace::None);
    document.element("LastModified", &crate::time_format::timestamp(modified)?);
    document.element("ETag", &super::object_etag(record));
    let mut response = super::document_response(document.finish());
    let mut headers = super::media_headers(record, super::CacheScope::Private)?;
    super::move_headers(
        &mut headers,
        response.headers_mut(),
        &[
            "etag",
            "x-goog-generation",
            "x-goog-metageneration",
            "x-goog-hash",
            "x-goog-stored-content-length",
            "x-goog-stored-content-encoding",
            "x-goog-component-count",
        ],
    );
    crate::csek::insert_response_headers(record, response.headers_mut());
    Ok(boxed(response))
}
