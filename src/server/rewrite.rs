use super::RequestBody as Incoming;
use super::*;
use crate::{
    csek::CopyKeys,
    json::rewrite::RewriteRequest,
    object::{ObjectConditions, content_type, generation::Generation},
    service::activity::begin_catalog_wait,
    storage::{
        DerivedDestination,
        rewrite::{
            RewriteBinding, RewriteError, RewriteProgress, RewriteSession, RewriteStart,
            RewriteStep,
        },
    },
};
use serde_json::Value;

/// The parameters a new rewrite session binds: the request's, then the response defaults it
/// left out, so a continuation that spells a default explicitly still matches.
fn bound_parameters(parameters: &serde_json::Map<String, Value>) -> serde_json::Map<String, Value> {
    let mut parameters = parameters.clone();
    for (key, value) in [
        ("projection", "noAcl"),
        ("alt", "json"),
        ("prettyPrint", "false"),
    ] {
        parameters
            .entry(key)
            .or_insert_with(|| Value::String(value.into()));
    }
    parameters
}

#[cfg(test)]
mod tests;

fn error(error: RewriteError) -> ApiError {
    match error {
        RewriteError::Invalid => ApiError::invalid("Invalid argument."),
        RewriteError::Gone => ApiError::new(
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
) -> Result<Option<(RewriteStep, RewriteSession)>, ApiError> {
    let source = &operation.names.source;
    if catalog
        .has_bucket_permission(source.bucket.as_str(), access)
        .await?
    {
        return Ok(None);
    }
    let session = if let Some(token) = &operation.token {
        match catalog.rewrite_step(token).await {
            Ok(found) => Some(found),
            Err(RewriteError::Invalid | RewriteError::Gone) => None,
            Err(failure) => return Err(error(failure)),
        }
    } else {
        None
    };
    let generation = session
        .as_ref()
        .filter(|(_, session)| {
            session.binding.source.bucket == source.bucket
                && session.binding.source.name == source.name
        })
        .map(|(_, session)| session.binding.source.generation)
        .or_else(|| operation.source_generation.map(Generation::get));
    catalog
        .get_object(
            operation.names.source_at(generation),
            ObjectConditions::default(),
            access,
        )
        .await?;
    Ok(session)
}

pub(super) async fn respond(
    request: Request<Incoming>,
    operation: RewriteRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, ApiError> {
    let ObjectCtx {
        state,
        config,
        activity,
        ..
    } = ctx;
    let source_access = ctx
        .authorize(auth::OBJECTS_GET, operation.names.source.bucket.as_str())
        .await?;
    let session = authorize_source(&state.catalog, &operation, &source_access).await?;
    let destination_access = ctx
        .authorize(
            auth::OBJECTS_CREATE,
            operation.names.destination.bucket.as_str(),
        )
        .await?;
    if operation.predefined_acl.is_some() {
        let record = state
            .catalog
            .get_bucket(
                operation.names.destination.bucket.as_str(),
                &BucketAccess::Permissive,
            )
            .await?
            .ok_or(CatalogError::NotFound)?;
        json::reject_predefined_acl(
            ctx.uri.parameters()?,
            record.uniform_access(None),
            json::PredefinedAclTarget::DerivedObject,
            &destination_access,
        )?;
    }
    json::validate_read_condition_headers(request.headers(), json::EtagConditions::Unsupported)?;
    json::validate_keyed_object_headers(
        request.headers(),
        &[crate::csek::DESTINATION_PREFIX, crate::csek::SOURCE_PREFIX],
    )?;
    let keys = json::copy_keys(request.headers())?;
    let projection = operation.projection;
    reject_headers(
        request.headers(),
        &[
            "x-goog-hash",
            "content-md5",
            "x-goog-upload-command",
            "x-upload-content-type",
            "x-upload-content-length",
        ],
    )?;
    let bytes = crate::bounded_body::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    let body = json::derived_resource(&bytes)?;
    let wait = begin_catalog_wait(activity).await;
    // Once mutation starts, await its durable result instead of timing out a detached commit.
    let job = RewriteJob {
        session,
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
            config.max_object_bytes(),
            ctx.endpoints.api_base_url().to_owned(),
        ),
    )
    .await;
    drop(wait);
    let (mut progress, owner) =
        result.map_err(|failure| ApiError::from(ObjectError::backend(failure)))??;
    // The step is settled: a call repeating its token answers from what it stored, so the
    // transfer outlives a failure to render the response below.
    if let Some(mut owner) = owner {
        owner.disarm();
    }
    if let Some(resource) = &mut progress.resource {
        // A repeated call on a finished rewrite answers from its stored resource.
        super::context::rebase_stored_links(ctx.endpoints, resource);
        // Cloud Storage answered a full projection rewrite with the destination's `acl`
        // (recorded 2026-11-17, case obj-full-projection-writes).
        if projection == crate::acl::Projection::Full {
            super::acl::add_full_projection_to_stored(
                state,
                super::acl::AclReader::Caller(ctx.caller),
                resource,
                ctx.endpoints.api_base_url(),
            )
            .await?;
        }
    }
    let value = json::rewrite::response(progress)?;
    // A rewrite response has no `etag` of its own, so it answers without an ETag header.
    Ok(boxed(json_response(&value)))
}

/// What one detached rewrite step owns: the request, its metadata body, keys and access. A
/// rewrite to a new destination key on the same object rotates its key.
struct RewriteJob {
    session: Option<(RewriteStep, RewriteSession)>,
    operation: RewriteRequest,
    body: Option<Value>,
    keys: CopyKeys,
    source_access: BucketAccess,
    destination_access: BucketAccess,
}

/// Resolves the rewrite source; a missing one is named in the error as Cloud Storage does.
async fn resolve_source(
    catalog: &Catalog,
    operation: &RewriteRequest,
    access: &BucketAccess,
    keys: CopyKeys,
) -> Result<crate::storage::DerivedSource, ApiError> {
    let target = operation
        .names
        .source_at(operation.source_generation.map(Generation::get));
    let conditions = operation.source_conditions;
    catalog
        .resolve_derived_source_keyed(target, conditions, access, keys.source)
        .await
        .map_err(|failure| {
            ApiError::from(failure).naming_object(
                operation.names.source.bucket.as_str(),
                &operation.names.source.name,
            )
        })
}

async fn execute(
    job: RewriteJob,
    catalog: Catalog,
    max_bytes: u64,
    base: String,
) -> Result<(RewriteProgress, Option<crate::storage::DerivedRequestGuard>), ApiError> {
    let RewriteJob {
        session,
        operation,
        body,
        keys,
        source_access,
        destination_access,
    } = job;
    let names = &operation.names;
    let mut owner = None;
    let step = if let Some(token) = &operation.token {
        let (step, session) = match session {
            Some(checked) => checked,
            None => catalog.rewrite_step(token).await.map_err(error)?,
        };
        if !session
            .binding
            .accepts(&operation.names, &operation.parameters, body.as_ref())
        {
            return Err(error(RewriteError::Invalid));
        }
        session.check_customer_keys(keys)?;
        step
    } else {
        let source = resolve_source(&catalog, &operation, &source_access, keys).await?;
        let mut destination = destination(
            &source.object,
            &operation,
            body.as_ref(),
            destination_access.clone(),
        )?;
        super::retention_access::authorize_upload(
            &catalog,
            &destination_access,
            names.destination.bucket.as_str(),
            &destination.metadata,
        )
        .await?;
        destination.metadata.customer_key_sha256 =
            keys.destination.map(crate::csek::CustomerKey::sha256);
        destination.metadata.acl = operation.predefined_acl.into();
        let buckets = catalog
            .rewrite_buckets(
                names.source.bucket.as_str(),
                &names.destination,
                operation.conditions,
                &destination_access,
            )
            .await?
            .ok_or(ObjectError::BucketNotFound)?;
        let budget = buckets.budget(
            source.object.storage_class,
            destination.storage_class,
            operation.max_bytes_per_call,
        );
        let binding = RewriteBinding {
            source: crate::object::ObjectTarget::new(
                names.source.bucket.clone(),
                names.source.name.clone(),
                source.object.generation.get(),
            ),
            destination: crate::object::ObjectTarget::new(
                names.destination.bucket.clone(),
                names.destination.name.clone(),
                (),
            ),
            parameters: bound_parameters(&operation.parameters),
            metadata: body.clone(),
        };
        let start = RewriteStart {
            binding,
            destination,
            source_conditions: operation.source_conditions,
            source_metageneration: source.object.metageneration.get(),
            source_access: source_access.clone(),
            source_key_sha256: keys.source.map(crate::csek::CustomerKey::sha256),
            budget,
        };
        let (step, guard) = catalog
            .begin_checked_rewrite(vec![source.whole()], max_bytes, start, buckets)
            .await
            .map_err(error)?;
        owner = Some(guard);
        step
    };
    let progress = catalog
        .advance_rewrite(step, source_access, destination_access, move |record| {
            let mut value = json::object_resource_keyed(
                record,
                &base,
                crate::acl::Projection::NoAcl,
                keys.destination,
            )
            .map_err(ObjectError::backend)?;
            json::add_derived_owner(&mut value, record);
            Ok(value)
        })
        .await
        .map_err(error)?;
    Ok((progress, owner))
}

fn destination(
    source: &crate::object::ObjectRecord,
    request: &RewriteRequest,
    body: Option<&Value>,
    access: BucketAccess,
) -> Result<DerivedDestination, ApiError> {
    let json::DerivedMetadata {
        update: metadata,
        storage_class: class,
        expected: supplied,
    } = json::derived_metadata_of(body, source)?;
    // Rewrite ignores a supplied md5Hash: Cloud Storage completed one with a wrong md5Hash using
    // the source's hash (recorded 2026-11-13). A crc32c mismatch still fails, before any byte
    // is copied; its wording follows the recorded upload mismatch.
    supplied
        .verify_crc32c(source.crc32c)
        .map_err(ObjectError::from)?;
    let content_type = metadata
        .content_type
        .value()
        .cloned()
        .unwrap_or_else(|| content_type::DEFAULT.to_owned());
    Ok(DerivedDestination {
        component_count: source.component_count,
        bucket: request.names.destination.bucket.as_str().to_owned(),
        name: request.names.destination.name.clone(),
        content_type,
        metadata,
        storage_class: class,
        declared: Default::default(),
        recorded: source.hashes().into(),
        conditions: request.conditions,
        access,
    })
}
