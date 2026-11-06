use super::RequestBody as Incoming;
use super::*;
use crate::{
    json::rewrite::{RewriteRequest, RewriteSide},
    object::ObjectConditions,
    storage::{
        DerivedDestination,
        rewrite::{RewriteBinding, RewriteError, RewriteStart},
    },
};
use serde_json::Value;

fn error(error: RewriteError) -> JsonError {
    match error {
        RewriteError::Invalid => JsonError::invalid("Invalid argument."),
        RewriteError::Gone => JsonError::new(
            StatusCode::GONE,
            "gone",
            "Rewrite token is unavailable or expired",
        ),
        RewriteError::Object(error) => error.into(),
    }
}

async fn authorize_source(
    catalog: &Catalog,
    operation: &RewriteRequest,
    access: &BucketAccess,
) -> Result<(), JsonError> {
    let source = &operation.names.source;
    if catalog
        .has_bucket_permission(&source.bucket, access)
        .await?
    {
        return Ok(());
    }
    let requested = operation.number("sourceGeneration");
    let generation = if let Some(token) = &operation.token {
        match catalog.rewrite_session(token).await {
            Ok(session)
                if session.binding.source_bucket == source.bucket
                    && session.binding.source_object == source.name =>
            {
                Some(session.binding.source_generation)
            }
            Ok(_) | Err(RewriteError::Invalid | RewriteError::Gone) => requested,
            Err(failure) => return Err(error(failure)),
        }
    } else {
        requested
    };
    catalog
        .get_object(
            operation.names.source_at(generation),
            ObjectConditions::default(),
            access,
        )
        .await?;
    Ok(())
}

pub(super) async fn respond(
    request: Request<Incoming>,
    operation: RewriteRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, JsonError> {
    let ObjectCtx {
        state,
        config,
        activity,
        ..
    } = ctx;
    let source_access = ctx
        .authorize(auth::OBJECTS_GET, &operation.names.source.bucket)
        .await?;
    authorize_source(&state.catalog, &operation, &source_access).await?;
    let destination_access = ctx
        .authorize(auth::OBJECTS_CREATE, &operation.names.destination.bucket)
        .await?;
    if operation
        .parameters
        .contains_key("destinationPredefinedAcl")
    {
        let record = state
            .catalog
            .get_bucket(
                &operation.names.destination.bucket,
                &BucketAccess::Permissive,
            )
            .await?
            .ok_or(CatalogError::NotFound)?;
        json::reject_predefined_acl(
            request.uri().query(),
            json::UniformAccess::of(record.uniform_bucket_level_access_locked_time, None),
            json::PredefinedAclTarget::Object,
        )?;
    }
    json::validate_read_condition_headers(request.headers(), json::EtagConditions::Unsupported)?;
    json::validate_keyed_object_headers(
        request.headers(),
        &[crate::csek::DESTINATION_PREFIX, crate::csek::SOURCE_PREFIX],
    )?;
    let keys = Keys {
        destination: json::request_key(request.headers())?,
        source: json::source_key(request.headers())?,
    };
    for name in [
        "x-goog-hash",
        "content-md5",
        "x-goog-upload-command",
        "x-upload-content-type",
        "x-upload-content-length",
    ] {
        if request.headers().contains_key(name) {
            return Err(JsonError::unsupported_object(name));
        }
    }
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    let body = if bytes.is_empty() {
        None
    } else {
        Some(
            serde_json::from_slice::<Value>(&bytes)
                .map_err(|cause| JsonError::parse_error(&bytes, &cause))?,
        )
    };
    let body = body.filter(|value| !value.is_null());
    begin_catalog_wait(activity).await;
    // Once mutation starts, await its durable result instead of timing out a detached commit.
    let job = RewriteJob {
        operation,
        body,
        keys,
        source_access,
        destination_access,
    };
    let result = crate::task::spawn_supervised(
        "rewrite",
        execute(
            job,
            state.catalog.clone(),
            config.max_object_bytes,
            ctx.endpoints.api_base_url().to_owned(),
        ),
    )
    .await;
    activity.send_modify(|progress| progress.awaiting_catalog = false);
    let (value, mut owner) =
        result.map_err(|failure| JsonError::from(ObjectError::backend(failure)))??;
    let mut response = Response::new(Full::new(Bytes::from(json::render_body(&value))));
    response.headers_mut().insert(
        "content-type",
        hyper::header::HeaderValue::from_static("application/json; charset=UTF-8"),
    );
    let response = boxed(response);
    if let Some(owner) = &mut owner {
        owner.disarm();
    }
    Ok(response)
}

/// Customer-supplied keys: `source` decrypts the source, `destination` encrypts the result.
/// A rewrite to a new destination key on the same object rotates its key.
#[derive(Clone, Copy)]
struct Keys {
    destination: Option<crate::csek::CustomerKey>,
    source: Option<crate::csek::CustomerKey>,
}

/// What one detached rewrite step owns: the request, its metadata body, keys and access.
struct RewriteJob {
    operation: RewriteRequest,
    body: Option<Value>,
    keys: Keys,
    source_access: BucketAccess,
    destination_access: BucketAccess,
}

/// Resolves the rewrite source; a missing one is named in the error as Cloud Storage does.
async fn resolve_source(
    catalog: &Catalog,
    operation: &RewriteRequest,
    access: &BucketAccess,
    keys: Keys,
) -> Result<crate::storage::DerivedSource, JsonError> {
    let target = operation
        .names
        .source_at(operation.number("sourceGeneration"));
    let conditions = operation.conditions(RewriteSide::Source);
    catalog
        .resolve_derived_source_keyed(target, conditions, access, keys.source)
        .await
        .map_err(|failure| {
            JsonError::from(failure)
                .naming_object(&operation.names.source.bucket, &operation.names.source.name)
        })
}

async fn execute(
    job: RewriteJob,
    catalog: Catalog,
    max_bytes: u64,
    base: String,
) -> Result<(Value, Option<crate::storage::DerivedRequestGuard>), JsonError> {
    let RewriteJob {
        operation,
        body,
        keys,
        source_access,
        destination_access,
    } = job;
    let names = &operation.names;
    let mut owner = None;
    let token = if let Some(token) = &operation.token {
        let session = catalog.rewrite_session(token).await.map_err(error)?;
        if !session
            .binding
            .accepts(&operation.names, &operation.parameters, body.as_ref())
        {
            return Err(error(RewriteError::Invalid));
        }
        // Each call must carry the keys the session started with.
        crate::csek::check_data(session.source_key_sha256, keys.source)?;
        crate::csek::check_data(
            session.destination.metadata.customer_key_sha256,
            keys.destination,
        )?;
        token.clone()
    } else {
        let source = resolve_source(&catalog, &operation, &source_access, keys).await?;
        let mut destination = destination(&source.object, &operation, &body)?;
        destination.metadata.customer_key_sha256 =
            keys.destination.map(crate::csek::CustomerKey::sha256);
        catalog
            .check_upload(
                &names.destination.bucket,
                &names.destination.name,
                operation.conditions(RewriteSide::Destination),
                &destination_access,
            )
            .await?;
        let source_bucket = catalog
            .get_bucket(&names.source.bucket, &BucketAccess::Permissive)
            .await?
            .ok_or(ObjectError::BucketNotFound)?;
        let destination_bucket = catalog
            .get_bucket(&names.destination.bucket, &BucketAccess::Permissive)
            .await?
            .ok_or(ObjectError::BucketNotFound)?;
        let multiple = source_bucket.location != destination_bucket.location
            || destination
                .storage_class
                .unwrap_or(destination_bucket.storage_class)
                != source.object.storage_class;
        let budget = if multiple {
            operation
                .number("maxBytesRewrittenPerCall")
                .map(u64::try_from)
                .transpose()
                .map_err(crate::storage::failure::object_unavailable)?
                .unwrap_or(8 * 1024 * 1024)
        } else {
            u64::MAX
        };
        let mut parameters = operation.parameters.clone();
        for (key, value) in [
            ("projection", "noAcl"),
            ("alt", "json"),
            ("prettyPrint", "false"),
        ] {
            parameters
                .entry(key)
                .or_insert_with(|| Value::String(value.into()));
        }
        let binding = RewriteBinding {
            source_bucket: names.source.bucket.clone(),
            source_object: names.source.name.clone(),
            source_generation: source.object.generation,
            destination_bucket: names.destination.bucket.clone(),
            destination_object: names.destination.name.clone(),
            parameters,
            metadata: body.clone(),
        };
        let start = RewriteStart {
            binding,
            destination,
            source_conditions: operation.conditions(RewriteSide::Source),
            source_metageneration: source.object.metageneration,
            source_access: source_access.clone(),
            source_key_sha256: keys.source.map(crate::csek::CustomerKey::sha256),
            budget,
        };
        let (token, guard) = catalog
            .begin_rewrite_session(vec![source.whole()], max_bytes, start)
            .await
            .map_err(error)?;
        owner = Some(guard);
        token
    };
    let progress = catalog
        .continue_rewrite(&token, source_access, destination_access, move |record| {
            let mut value = json::object_resource_keyed(
                record,
                &base,
                json::Projection::NoAcl,
                keys.destination,
            )
            .map_err(ObjectError::backend)?;
            json::add_derived_owner(&mut value, record);
            Ok(value)
        })
        .await
        .map_err(error)?;
    let value = json::rewrite::response(progress)?;
    Ok((value, owner))
}

fn destination(
    source: &crate::object::ObjectRecord,
    request: &RewriteRequest,
    body: &Option<Value>,
) -> Result<DerivedDestination, JsonError> {
    let bytes = body.as_ref().map(Value::to_string).unwrap_or_default();
    let (metadata, class, supplied) = json::derived_metadata(bytes.as_bytes(), source)?;
    if supplied.md5.is_some_and(|hash| hash != source.md5)
        || supplied.crc32c.is_some_and(|hash| hash != source.crc32c)
    {
        return Err(ObjectError::ChecksumMismatch.into());
    }
    let content_type = metadata
        .content_type
        .as_ref()
        .and_then(|value| value.as_ref())
        .cloned()
        .unwrap_or_else(|| "application/octet-stream".into());
    Ok(DerivedDestination {
        component_count: source.component_count,
        bucket: request.names.destination.bucket.clone(),
        name: request.names.destination.name.clone(),
        content_type,
        metadata,
        storage_class: class,
        expected: crate::object::ExpectedHashes {
            md5: Some(source.md5),
            crc32c: Some(source.crc32c),
        },
        conditions: request.conditions(RewriteSide::Destination),
        access: BucketAccess::Permissive,
    })
}
