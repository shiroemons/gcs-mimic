//! JSON API object operations: shared authorization and validation, then one handler per
//! operation. Uploads, reads and derived writes (copy and compose) live in submodules.

mod derived;
mod read;
mod upload;

use super::{
    BUCKET_BODY_LIMIT, ObjectCtx, ResponseBody, acl, boxed, empty_response, json_response,
    resumable, retention_access, validate_bodyless_post,
};
use crate::{
    auth,
    bucket::{BucketAccess, CatalogError},
    json::{self, JsonError, ObjectRequest},
    object::ObjectError,
};
use hyper::{HeaderMap, Request, Response, StatusCode, Uri, body::Incoming};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

/// The ACL entries an upload replay recorded with its first response; the catalog sets them
/// once the first attempt publishes.
type UploadAclSnapshot = Arc<OnceLock<Option<Vec<crate::acl::AclEntry>>>>;

/// A written object's resource and, for a replayed upload, the ACL it recorded.
struct Written {
    value: Value,
    /// An idempotent replay reports the ACL from its first response instead of the live one.
    acl_snapshot: Option<UploadAclSnapshot>,
}

impl From<Value> for Written {
    fn from(value: Value) -> Self {
        Self {
            value,
            acl_snapshot: None,
        }
    }
}

pub(super) async fn respond(
    request: Request<Incoming>,
    operation: ObjectRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, JsonError> {
    let fields = response_fields(request.uri(), &operation)?;
    let (permission, bucket) = required_permission(&operation);
    // Authorization precedes header/body validation, conditions and existence checks.
    // Replacing a live object additionally needs storage.objects.delete. The access carries
    // that decision, and publication enforces it inside the catalog transaction if the name is
    // live, so a concurrent create cannot be overwritten.
    let access = ctx.authorize(permission, bucket).await?;
    let query = json::parse_query(request.uri().query().unwrap_or_default())?;
    let acl_full = acl::full_projection(request.method(), request.uri().query());
    if query.contains_key("predefinedAcl") || query.contains_key("destinationPredefinedAcl") {
        reject_predefined_acl(request.uri(), ctx, bucket).await?;
    }
    validate_request(request.headers(), &operation)?;
    let written = match operation {
        ObjectRequest::Restore {
            bucket,
            name,
            generation,
            conditions,
            projection,
        } => {
            let target = RestoreTarget {
                bucket,
                name,
                generation,
                conditions,
                projection,
            };
            restore(request, ctx, &access, &query, target).await?.into()
        }
        ObjectRequest::Move(params) => move_object(request, ctx, &access, params).await?.into(),
        ObjectRequest::Copy(params) => derived::copy(request, ctx, access, params).await?,
        ObjectRequest::Compose {
            bucket,
            name,
            conditions,
        } => derived::compose(request, ctx, access, bucket, name, conditions)
            .await?
            .into(),
        ObjectRequest::Resumable {
            bucket,
            name,
            params,
            content_encoding,
        } => {
            return resumable::initiate(
                request,
                &bucket,
                name,
                params,
                content_encoding,
                access,
                ctx,
            )
            .await;
        }
        ObjectRequest::List(params) => list(request.headers(), ctx, &access, &params).await?.into(),
        ObjectRequest::Update {
            bucket,
            name,
            params,
            replace,
            override_unlocked_retention,
        } => {
            let target = UpdateTarget {
                bucket,
                name,
                params,
                replace,
                override_unlocked_retention,
            };
            update(request, ctx, &access, target).await?.into()
        }
        ObjectRequest::Multipart {
            bucket,
            name,
            params,
        } => upload::multipart(request, ctx, access, bucket, name, params).await?,
        ObjectRequest::Insert {
            bucket,
            name,
            params,
        } => upload::insert(request, ctx, access, bucket, name, params).await?,
        ObjectRequest::Get {
            bucket,
            name,
            params,
        } => return read::get(&request, ctx, &access, &bucket, &name, &params, &fields).await,
        ObjectRequest::Delete {
            bucket,
            name,
            generation,
            conditions,
        } => {
            json::validate_object_get_headers(request.headers(), json::ObjectRead::Metadata)?;
            ctx.state
                .catalog
                .delete_object_authorized(&bucket, &name, generation, conditions, &access)
                .await?;
            return Ok(boxed(empty_response(StatusCode::NO_CONTENT)));
        }
    };
    let mut value = written.value;
    if acl_full {
        add_acl(ctx, &mut value, written.acl_snapshot).await?;
    }
    fields.apply(&mut value);
    Ok(boxed(json_response(&value)))
}

fn response_fields(uri: &Uri, operation: &ObjectRequest) -> Result<json::Fields, JsonError> {
    match operation {
        ObjectRequest::Delete { .. } => json::Fields::from_query(None, "Object"),
        ObjectRequest::List(_) => json::Fields::from_query(uri.query(), "Objects"),
        _ => json::Fields::from_query(uri.query(), "Object"),
    }
}

/// The permission an operation needs on the bucket its request names.
fn required_permission(operation: &ObjectRequest) -> (&'static str, &str) {
    match operation {
        ObjectRequest::Restore { bucket, .. } => (auth::OBJECTS_RESTORE, bucket),
        ObjectRequest::Move(params) => (auth::OBJECTS_CREATE, &params.bucket),
        ObjectRequest::Copy(params) => (auth::OBJECTS_CREATE, &params.bucket),
        ObjectRequest::List(params) => (auth::OBJECTS_LIST, &params.query.bucket),
        ObjectRequest::Update { bucket, .. } => (auth::OBJECTS_UPDATE, bucket),
        ObjectRequest::Compose { bucket, .. }
        | ObjectRequest::Insert { bucket, .. }
        | ObjectRequest::Multipart { bucket, .. }
        | ObjectRequest::Resumable { bucket, .. } => (auth::OBJECTS_CREATE, bucket),
        ObjectRequest::Get { bucket, .. } => (auth::OBJECTS_GET, bucket),
        ObjectRequest::Delete { bucket, .. } => (auth::OBJECTS_DELETE, bucket),
    }
}

async fn reject_predefined_acl(
    uri: &Uri,
    ctx: ObjectCtx<'_>,
    bucket: &str,
) -> Result<(), JsonError> {
    let record = ctx
        .state
        .catalog
        .get_bucket(bucket)
        .await?
        .ok_or(CatalogError::NotFound)?;
    json::reject_predefined_acl(
        uri.query(),
        record.uniform_bucket_level_access_locked_time.is_some(),
        true,
    )
}

/// Header and body checks that precede every operation's own validation.
fn validate_request(headers: &HeaderMap, operation: &ObjectRequest) -> Result<(), JsonError> {
    json::validate_read_condition_headers(headers, matches!(operation, ObjectRequest::Get { .. }))?;
    let accepts_body = matches!(
        operation,
        ObjectRequest::Insert { .. }
            | ObjectRequest::Compose { .. }
            | ObjectRequest::Multipart { .. }
            | ObjectRequest::Resumable { .. }
            | ObjectRequest::Update { .. }
            | ObjectRequest::Copy(_)
            | ObjectRequest::Restore { .. }
            | ObjectRequest::Move(_)
    );
    if !accepts_body
        && (headers.contains_key("transfer-encoding")
            || headers.get("content-length").is_some_and(|v| v != "0"))
    {
        return Err(JsonError::invalid_argument(
            "This method requires an empty body",
        ));
    }
    Ok(())
}

/// Adds the full projection's ACL fields to a written object's resource.
async fn add_acl(
    ctx: ObjectCtx<'_>,
    value: &mut Value,
    snapshot: Option<UploadAclSnapshot>,
) -> Result<(), JsonError> {
    let Some(snapshot) = snapshot else {
        return acl::add_full_projection(ctx.state, ctx.caller, value).await;
    };
    if let Some(entries) = snapshot.get().ok_or_else(|| {
        JsonError::from(ObjectError::Unavailable(
            "Missing upload replay snapshot".into(),
        ))
    })? {
        let bucket = value["bucket"]
            .as_str()
            .ok_or_else(|| JsonError::from(ObjectError::InvalidMetadata))?;
        if acl::may_read(ctx.state, ctx.caller, crate::acl::AclKind::Object, bucket).await? {
            json::acl::add_object_acl_fields(value, &ctx.state.api_base_url, entries);
        }
    }
    Ok(())
}

/// The idempotency token that lets a retried write replay its first response.
fn idempotency_token(headers: &HeaderMap) -> Result<Option<String>, JsonError> {
    Ok(json::resumable::single(headers, "x-goog-gcs-idempotency-token")?.map(str::to_owned))
}

struct RestoreTarget {
    bucket: String,
    name: String,
    generation: i64,
    conditions: crate::object::ObjectConditions,
    projection: json::Projection,
}

async fn restore(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    query: &BTreeMap<String, String>,
    target: RestoreTarget,
) -> Result<Value, JsonError> {
    json::validate_object_get_headers(request.headers(), json::ObjectRead::Metadata)?;
    validate_bodyless_post(request, query).await?;
    let create_access = ctx.authorize(auth::OBJECTS_CREATE, &target.bucket).await?;
    let record = ctx
        .state
        .catalog
        .restore_object_authorized(
            &target.bucket,
            &target.name,
            target.generation,
            target.conditions,
            access,
            &create_access,
        )
        .await?;
    json::object_resource(&record, &ctx.state.api_base_url, target.projection)
}

async fn move_object(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    params: json::ObjectMoveParams,
) -> Result<Value, JsonError> {
    json::validate_object_get_headers(request.headers(), json::ObjectRead::Metadata)?;
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    // The Node SDK serializes empty options as {}, without move semantics.
    if !bytes.is_empty()
        && serde_json::from_slice::<Value>(&bytes).ok() != Some(serde_json::json!({}))
    {
        return Err(JsonError::invalid_argument(
            "Move requires an empty body or an empty JSON object",
        ));
    }
    let source_read = ctx.authorize(auth::OBJECTS_GET, &params.bucket).await?;
    let source_delete = ctx.authorize(auth::OBJECTS_DELETE, &params.bucket).await?;
    let record = ctx
        .state
        .catalog
        .move_object_authorized(
            &params.bucket,
            &params.source,
            &params.destination,
            params.source_conditions,
            params.conditions,
            &source_read,
            &source_delete,
            access,
        )
        .await?;
    json::object_resource(&record, &ctx.state.api_base_url, params.projection)
}

async fn list(
    headers: &HeaderMap,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    params: &json::ObjectListParams,
) -> Result<Value, JsonError> {
    json::validate_object_get_headers(headers, json::ObjectRead::Metadata)?;
    let page = ctx
        .state
        .catalog
        .list_objects_authorized(params.query.clone(), params.max_results, access)
        .await?;
    if ctx.config.log_level == crate::diagnostics::LogLevel::Debug
        && params.empty_soft_deleted_message(&page, None).is_some()
    {
        log_empty_soft_deleted_listing(ctx, access, params, &page).await;
    }
    json::object_list(&page, params, &ctx.state.api_base_url)
}

/// Explains an empty soft-deleted listing in debug logs. Diagnostic reads must not change the
/// successful listing response, so their failures are only logged.
async fn log_empty_soft_deleted_listing(
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    params: &json::ObjectListParams,
    page: &crate::storage::ObjectListPage,
) {
    match ctx
        .state
        .catalog
        .get_bucket_authorized(&params.query.bucket, access)
        .await
    {
        Ok(Some(bucket)) => {
            if let Some(message) =
                params.empty_soft_deleted_message(page, bucket.soft_delete_policy.as_ref())
            {
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Debug,
                    "soft_deleted_listing_empty",
                    serde_json::json!({"message":message}),
                );
            }
        }
        Ok(None) => {}
        Err(error) => crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Debug,
            "soft_deleted_listing_diagnostic_failed",
            serde_json::json!({"message":"Cannot inspect softDeletePolicy after listing; retry the bucket metadata request", "cause":error.to_string()}),
        ),
    }
}

struct UpdateTarget {
    bucket: String,
    name: String,
    params: json::ObjectGetParams,
    replace: bool,
    override_unlocked_retention: bool,
}

async fn update(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    target: UpdateTarget,
) -> Result<Value, JsonError> {
    let UpdateTarget {
        bucket,
        name,
        params,
        replace,
        override_unlocked_retention,
    } = target;
    json::validate_keyed_object_headers(
        request.headers(),
        json::ObjectRead::Metadata,
        &[crate::csek::DESTINATION_PREFIX],
    )?;
    let key = json::request_key(request.headers())?;
    // shortcut: SDK invocation tokens do not replay responses; use metageneration conditions for retry safety.
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    let mut update = json::parse_object_metadata_mutation(&bytes, replace)?;
    retention_access::authorize_update(
        ctx.state,
        access,
        &bucket,
        &mut update.update,
        override_unlocked_retention,
    )
    .await?;
    let base = ctx.state.api_base_url.clone();
    let record = ctx
        .state
        .catalog
        .update_object_metadata_authorized(
            &bucket,
            &name,
            params.generation,
            params.conditions,
            update.update,
            move |record| {
                // A wrong key fails before the metadata change commits.
                crate::csek::check_metadata(record.customer_key_sha256, key)
                    .map_err(ObjectError::CustomerEncryption)?;
                json::ObjectMetadataRequest::validate_unchanged(&update.unchanged, record, &base)
            },
            access,
        )
        .await?;
    json::object_resource_keyed(&record, &ctx.state.api_base_url, params.projection, key)
}
