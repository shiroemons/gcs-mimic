//! JSON API objects derived from stored objects: copy and compose.

use super::{Written, idempotency_token};
use crate::server::RequestBody as Incoming;
use crate::{
    auth,
    bucket::BucketAccess,
    csek::CustomerKey,
    json::{self, JsonError},
    object::{ObjectConditions, ObjectError, ObjectRecord},
    server::{
        BUCKET_BODY_LIMIT, ObjectCtx,
        copy::{self, CopyBudget, CopyJob},
    },
    storage::{CopyReplay, CopyReplaySource, DerivedDestination, ReplayInvocation},
};
use hyper::{HeaderMap, Request};

/// Rejects headers that do not apply to a derived write.
fn reject_headers(headers: &HeaderMap, names: &[&str]) -> Result<(), JsonError> {
    match names.iter().find(|name| headers.contains_key(**name)) {
        Some(name) => Err(JsonError::unsupported_object(name)),
        None => Ok(()),
    }
}

pub(super) async fn copy(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: BucketAccess,
    params: json::ObjectCopyParams,
) -> Result<Written, JsonError> {
    json::validate_keyed_object_headers(
        request.headers(),
        json::ObjectRead::Metadata,
        &[crate::csek::DESTINATION_PREFIX, crate::csek::SOURCE_PREFIX],
    )?;
    let destination_key = json::request_key(request.headers())?;
    let source_key = json::source_key(request.headers())?;
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
        .authorize(auth::OBJECTS_GET, &params.source_bucket)
        .await?;
    let method = request.method().clone();
    let uri = request.uri().clone();
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    let request_binding = format!(
        "copy-v1:{method}|{uri}|{}|{:?}|{:?}|{:?}",
        ctx.state.api_base_url,
        destination_key.map(CustomerKey::sha256),
        source_key.map(CustomerKey::sha256),
        ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref()
    );
    let replay = ctx
        .await_catalog(ctx.state.catalog.prepare_copy_replay(
            &params.bucket,
            &params.name,
            params.conditions,
            &access,
            CopyReplaySource {
                bucket: &params.source_bucket,
                name: &params.source_name,
                generation: params.source_generation,
                access: &source_access,
            },
            ReplayInvocation {
                token: token.as_deref(),
                request: &request_binding,
            },
        ))
        .await?;
    let acl_snapshot = replay.as_ref().map(CopyReplay::acl_snapshot);
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
            source_key,
            destination_key,
            replay,
        };
        copy_object(ctx, request, &bytes).await?
    };
    Ok(Written {
        value: json::object_resource_keyed(
            &record,
            &ctx.endpoints.api_base_url,
            projection,
            destination_key,
        )?,
        acl_snapshot,
    })
}

/// A copy that no earlier attempt has completed.
struct CopyRequest {
    params: json::ObjectCopyParams,
    access: BucketAccess,
    source_access: BucketAccess,
    source_key: Option<CustomerKey>,
    destination_key: Option<CustomerKey>,
    replay: Option<CopyReplay>,
}

async fn copy_object(
    ctx: ObjectCtx<'_>,
    request: CopyRequest,
    body: &[u8],
) -> Result<ObjectRecord, JsonError> {
    let CopyRequest {
        params,
        access,
        source_access,
        source_key,
        destination_key,
        replay,
    } = request;
    let catalog = &ctx.state.catalog;
    let source = catalog
        .resolve_derived_source_keyed(
            &params.source_bucket,
            &params.source_name,
            params.source_generation,
            params.source_conditions,
            &source_access,
            source_key,
        )
        .await?;
    let (mut metadata, storage_class, expected) = json::derived_metadata(body, &source.object)?;
    metadata.customer_key_sha256 = destination_key.map(CustomerKey::sha256);
    let source_location = if source.object.component_count.is_some() {
        Some(
            catalog
                .get_bucket(&params.source_bucket)
                .await?
                .ok_or(ObjectError::BucketNotFound)?
                .location,
        )
    } else {
        None
    };
    let job = CopyJob {
        source,
        source_access,
        destination: DerivedDestination {
            bucket: params.bucket,
            name: params.name,
            content_type: String::new(),
            metadata,
            storage_class,
            expected,
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
    Ok(copy::execute(
        catalog,
        job,
        budget,
        #[cfg(test)]
        None,
    )
    .await?)
}

pub(super) async fn compose(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: BucketAccess,
    bucket: String,
    name: String,
    conditions: ObjectConditions,
) -> Result<serde_json::Value, JsonError> {
    json::validate_keyed_object_headers(
        request.headers(),
        json::ObjectRead::Metadata,
        &[crate::csek::DESTINATION_PREFIX],
    )?;
    let key = json::request_key(request.headers())?;
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
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    let mut body = json::compose::parse_compose(&bytes, &bucket, &name)?;
    body.destination.customer_key_sha256 = key.map(CustomerKey::sha256);
    ctx.activity
        .send_modify(|progress| progress.awaiting_catalog = true);
    let record = ctx
        .state
        .catalog
        .compose_object(
            DerivedDestination {
                bucket,
                name,
                conditions,
                access,
                metadata: body.destination,
                content_type: String::new(),
                storage_class: None,
                expected: Default::default(),
                component_count: None,
            },
            body.sources,
            if body.delete_sources {
                crate::storage::SourceDisposal::Delete
            } else {
                crate::storage::SourceDisposal::Keep
            },
            ctx.config.max_object_bytes,
        )
        .await?;
    json::object_resource_keyed(
        &record,
        &ctx.endpoints.api_base_url,
        json::Projection::NoAcl,
        key,
    )
}
