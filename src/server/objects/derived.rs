//! JSON API objects derived from stored objects: copy and compose.

use super::{AclSource, Written, idempotency_token};
use crate::server::RequestBody as Incoming;
use crate::{
    auth,
    bucket::BucketAccess,
    csek::{CopyKeys, CustomerKey},
    json,
    json::ApiError,
    object::{ObjectError, ObjectRecord, ObjectTarget, generation::Generation},
    server::{ObjectCtx, RESOURCE_BODY_LIMIT, reject_headers},
    service::copy::{self, CopyBudget, CopyJob},
    storage::{CopyReplay, CopyReplaySource, DerivedDestination, ReplayInvocation},
};
use hyper::Request;

pub(super) async fn copy(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: BucketAccess,
    params: json::ObjectCopyParams,
) -> Result<Written, ApiError> {
    json::validate_keyed_object_headers(
        request.headers(),
        &[crate::csek::DESTINATION_PREFIX, crate::csek::SOURCE_PREFIX],
    )?;
    let keys = json::copy_keys(request.headers())?;
    let token = idempotency_token(request.headers())?;
    reject_headers(
        request.headers(),
        &[
            "content-md5",
            "x-goog-hash",
            "range",
            "if-range",
            "if-modified-since",
            "if-unmodified-since",
        ],
    )?;
    let source_access = ctx
        .authorize(auth::OBJECTS_GET, params.source_bucket.as_str())
        .await?;
    let method = request.method().clone();
    let uri = request.uri().clone();
    let bytes = crate::bounded_body::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    let request_binding = format!(
        "copy-v1:{method}|{uri}|{}|{:?}|{:?}|{:?}",
        ctx.state.api_base_url,
        keys.destination.map(CustomerKey::sha256),
        keys.source.map(CustomerKey::sha256),
        ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref()
    );
    let replay = ctx
        .await_catalog(ctx.state.catalog.prepare_copy_replay(
            params.bucket.as_str(),
            &params.name,
            params.conditions,
            &access,
            CopyReplaySource {
                bucket: params.source_bucket.as_str(),
                name: &params.source_name,
                generation: params.source_generation.map(Generation::get),
                access: &source_access,
            },
            ReplayInvocation {
                token: token.as_deref(),
                request: &request_binding,
            },
        ))
        .await?;
    let acl = AclSource::from(replay.as_ref().map(CopyReplay::acl_snapshot));
    let cached = if let Some(replay) = &replay {
        ctx.await_catalog(ctx.state.catalog.copy_replay_record(replay))
            .await?
    } else {
        None
    };
    let projection = params.projection;
    let record = if let Some(record) = cached {
        record
    } else {
        let request = CopyRequest {
            params,
            access,
            source_access,
            keys,
            replay,
        };
        copy_object(ctx, request, &bytes).await?
    };
    let mut value = json::object_resource_keyed(
        &record,
        ctx.endpoints.api_base_url(),
        projection,
        keys.destination,
    )?;
    json::add_derived_owner(&mut value, &record);
    Ok(Written::object(value, &record, acl))
}

/// A copy that no earlier attempt has completed.
struct CopyRequest {
    params: json::ObjectCopyParams,
    access: BucketAccess,
    source_access: BucketAccess,
    keys: CopyKeys,
    replay: Option<CopyReplay>,
}

async fn copy_object(
    ctx: ObjectCtx<'_>,
    request: CopyRequest,
    body: &[u8],
) -> Result<ObjectRecord, ApiError> {
    let CopyRequest {
        params,
        access,
        source_access,
        keys,
        replay,
    } = request;
    let catalog = &ctx.state.catalog;
    let target = ObjectTarget::from_bucket(
        params.source_bucket.clone(),
        &params.source_name,
        params.source_generation.map(Generation::get),
    );
    let source = catalog
        .resolve_derived_source_keyed(
            target,
            params.source_conditions,
            &source_access,
            keys.source,
        )
        .await
        .map_err(|failure| {
            ApiError::from(failure)
                .naming_object(params.source_bucket.as_str(), &params.source_name)
        })?;
    let json::DerivedMetadata {
        update: mut metadata,
        storage_class,
        expected: declared,
    } = json::derived_metadata(body, &source.object)?;
    let recorded = source.object.hashes().into();
    crate::service::access::retention::authorize_upload(
        &ctx.state.catalog,
        &access,
        params.bucket.as_str(),
        &metadata,
    )
    .await?;
    metadata.customer_key_sha256 = keys.destination.map(CustomerKey::sha256);
    metadata.acl = params.predefined_acl.into();
    let source_location = if source.object.component_count.is_some() {
        Some(
            catalog
                .get_bucket(params.source_bucket.as_str(), &BucketAccess::Permissive)
                .await?
                .ok_or(ObjectError::BucketNotFound)?
                .location
                .as_str()
                .to_owned(),
        )
    } else {
        None
    };
    let job = CopyJob {
        source,
        source_access,
        destination: DerivedDestination {
            bucket: params.bucket.into_inner(),
            name: params.name,
            content_type: String::new(),
            metadata,
            storage_class,
            declared,
            recorded,
            conditions: params.conditions,
            access,
            component_count: None,
        },
        source_location,
        replay,
    };
    let budget = CopyBudget {
        config: ctx.config,
        deadline: ctx.deadline,
        activity: ctx.activity,
    };
    Ok(copy::execute(catalog, job, budget).await?)
}

pub(super) async fn compose(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: BucketAccess,
    params: json::ComposeParams,
) -> Result<Written, ApiError> {
    let json::ComposeParams {
        bucket,
        name,
        conditions,
        predefined_acl,
    } = params;
    json::validate_keyed_object_headers(request.headers(), &[crate::csek::DESTINATION_PREFIX])?;
    let key = crate::csek::request_key(request.headers())?;
    reject_headers(
        request.headers(),
        &[
            "if-range",
            "if-modified-since",
            "if-unmodified-since",
            "range",
            "content-md5",
            "x-goog-hash",
        ],
    )?;
    let bytes = crate::bounded_body::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    let mut body = json::compose::parse_compose(&bytes, bucket.as_str())?;
    crate::service::access::retention::authorize_upload(
        &ctx.state.catalog,
        &access,
        bucket.as_str(),
        &body.destination,
    )
    .await?;
    body.destination.customer_key_sha256 = key.map(CustomerKey::sha256);
    body.destination.acl = predefined_acl.into();
    let _wait = crate::service::activity::begin_catalog_wait(ctx.activity).await;
    let record = ctx
        .state
        .catalog
        .compose_object(
            DerivedDestination {
                bucket: bucket.into_inner(),
                name,
                conditions,
                access,
                metadata: body.destination,
                content_type: String::new(),
                storage_class: body.storage_class,
                // The JSON composite-object guide ignores supplied destination hashes; the
                // catalog records the combined CRC32C itself.
                declared: Default::default(),
                recorded: Default::default(),
                component_count: None,
            },
            body.sources,
            if body.delete_sources {
                crate::storage::SourceDisposal::Delete
            } else {
                crate::storage::SourceDisposal::Keep
            },
            ctx.config.max_object_bytes(),
        )
        .await
        .map_err(|error| {
            let error = ApiError::from(error);
            if error.status() == hyper::StatusCode::PRECONDITION_FAILED {
                error.at_if_match()
            } else {
                error
            }
        })?;
    Written::keyed(
        &record,
        ctx,
        crate::acl::Projection::NoAcl,
        key,
        AclSource::Live,
    )
}
