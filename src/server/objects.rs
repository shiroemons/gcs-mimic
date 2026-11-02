//! JSON API object operations: shared authorization and validation, then one handler per
//! operation. Uploads, reads and derived writes (copy and compose) live in submodules.

mod derived;
mod read;
mod upload;

use super::RequestBody as Incoming;
use super::{
    BUCKET_BODY_LIMIT, JsonCache, ObjectCtx, ResponseBody, acl, boxed, empty_response,
    json_response_selected, resumable, retention_access, validate_bodyless_post,
};
use crate::{
    auth,
    bucket::{BucketAccess, CatalogError},
    json::{self, JsonError, ObjectRequest, UploadRequest},
    object::{ObjectConditions, ObjectError, ObjectTarget, retention::UnlockedOverride},
    storage::{MoveAccess, RestoreAccess, UploadAclSnapshot},
};
use hyper::{HeaderMap, Request, Response, StatusCode, Uri};
use serde_json::Value;
use std::collections::BTreeMap;

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
    // Cloud Storage omits acl and owner from a restore response even for projection=full.
    let acl_full = acl::full_projection(request.method(), request.uri().query())
        && !matches!(operation, ObjectRequest::Restore { .. });
    if query.contains_key("predefinedAcl") || query.contains_key("destinationPredefinedAcl") {
        check_predefined_acl(request.uri(), ctx, bucket, &operation, &access).await?;
    }
    validate_request(request.headers(), &operation)?;
    // Only recorded operations carry a cache header: lists are cacheable-but-revalidated
    // resources and an object insert forbids every cache. Other writes were not recorded.
    let cache = match &operation {
        ObjectRequest::List(_) => JsonCache::Resource,
        ObjectRequest::Insert { .. } | ObjectRequest::Multipart { .. } => JsonCache::NoStore,
        _ => JsonCache::Unrecorded,
    };
    // Cloud Storage sends no ETag header with a move response, though the body has an etag.
    let validator_header = !matches!(operation, ObjectRequest::Move(_));
    let written = match operation {
        ObjectRequest::Restore { target, conditions } => {
            restore(request, ctx, &access, &query, target, conditions)
                .await?
                .into()
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
            target,
            conditions,
            projection,
            mode,
            retention_override,
        } => {
            let target = UpdateTarget {
                object: target,
                conditions,
                projection,
                mode,
                retention_override,
            };
            update(request, ctx, &access, target).await?.into()
        }
        ObjectRequest::Multipart(target) => upload::multipart(request, ctx, access, target).await?,
        ObjectRequest::Insert(target) => upload::insert(request, ctx, access, target).await?,
        ObjectRequest::Get {
            target,
            conditions,
            params,
        } => {
            return read::get(&request, ctx, &access, target, conditions, params, &fields).await;
        }
        ObjectRequest::Delete { target, conditions } => {
            json::validate_object_get_headers(request.headers())?;
            ctx.state
                .catalog
                .delete_object(target, conditions, &access)
                .await?;
            return Ok(boxed(empty_response(StatusCode::NO_CONTENT)));
        }
    };
    let mut value = written.value;
    if acl_full {
        add_acl(ctx, &mut value, written.acl_snapshot).await?;
    }
    let mut response = json_response_selected(&mut value, &fields, cache);
    if !validator_header {
        response.headers_mut().remove("etag");
    }
    Ok(boxed(response))
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
        ObjectRequest::Restore { target, .. } => (auth::OBJECTS_RESTORE, &target.bucket),
        ObjectRequest::Move(params) => (auth::OBJECTS_CREATE, &params.names.bucket),
        ObjectRequest::Copy(params) => (auth::OBJECTS_CREATE, &params.bucket),
        ObjectRequest::List(params) => (auth::OBJECTS_LIST, &params.query.bucket),
        ObjectRequest::Update { target, .. } => (auth::OBJECTS_UPDATE, &target.bucket),
        ObjectRequest::Compose { bucket, .. }
        | ObjectRequest::Insert(UploadRequest { bucket, .. })
        | ObjectRequest::Multipart(UploadRequest { bucket, .. })
        | ObjectRequest::Resumable { bucket, .. } => (auth::OBJECTS_CREATE, bucket),
        ObjectRequest::Get { target, .. } => (auth::OBJECTS_GET, &target.bucket),
        ObjectRequest::Delete { target, .. } => (auth::OBJECTS_DELETE, &target.bucket),
    }
}

/// Uploads apply `predefinedAcl` outside uniform access; every other operation, and any
/// predefined ACL under uniform access, keeps its explicit 400. Publication rechecks uniform
/// access and public access prevention atomically.
async fn check_predefined_acl(
    uri: &Uri,
    ctx: ObjectCtx<'_>,
    bucket: &str,
    operation: &ObjectRequest,
    access: &BucketAccess,
) -> Result<(), JsonError> {
    let target = if matches!(
        operation,
        ObjectRequest::Insert(_) | ObjectRequest::Multipart(_) | ObjectRequest::Resumable { .. }
    ) {
        // Documented: anonymous uploads cannot specify a predefined ACL. Their owner is the
        // project owners team, so an anonymous caller could otherwise choose its object's ACL.
        if matches!(access, BucketAccess::Policy { identity: None, .. }) {
            return Err(JsonError::invalid_argument(
                "Anonymous callers cannot specify predefinedAcl on upload; authenticate or omit predefinedAcl",
            ));
        }
        json::PredefinedAclTarget::ObjectUpload
    } else {
        json::PredefinedAclTarget::Object
    };
    let record = ctx
        .state
        .catalog
        .get_bucket(bucket, &BucketAccess::Permissive)
        .await?
        .ok_or(CatalogError::NotFound)?;
    json::reject_predefined_acl(
        uri.query(),
        json::UniformAccess::of(record.uniform_bucket_level_access_locked_time, None),
        target,
    )
}

/// Header and body checks that precede every operation's own validation.
fn validate_request(headers: &HeaderMap, operation: &ObjectRequest) -> Result<(), JsonError> {
    if !matches!(operation, ObjectRequest::Get { .. }) {
        let etags = match operation {
            ObjectRequest::Update { .. } => json::EtagConditions::MatchOnly,
            _ => json::EtagConditions::Unsupported,
        };
        json::validate_read_condition_headers(headers, etags)?;
    }
    let accepts_body = matches!(
        operation,
        ObjectRequest::Insert(_)
            | ObjectRequest::Compose { .. }
            | ObjectRequest::Multipart(_)
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
        return acl::add_full_projection(ctx.state, ctx.caller, value, &ctx.endpoints.api_base_url)
            .await;
    };
    if let Some(snapshot) = snapshot.get().ok_or_else(|| {
        JsonError::from(ObjectError::Unavailable(
            "Missing upload replay snapshot".into(),
        ))
    })? {
        let bucket = value["bucket"]
            .as_str()
            .ok_or_else(|| JsonError::from(ObjectError::InvalidMetadata))?;
        if !snapshot.matches_resource(
            bucket,
            value["name"].as_str(),
            value["generation"]
                .as_str()
                .and_then(|generation| generation.parse::<i64>().ok()),
        ) {
            return Err(JsonError::from(ObjectError::InvalidMetadata));
        }
        let access = ctx.authorize(auth::OBJECTS_GET_IAM, bucket).await?;
        if let Some(snapshot) = ctx
            .state
            .catalog
            .object_acl_snapshot(snapshot.clone(), &access)
            .await?
        {
            json::acl::add_object_acl_fields(value, &ctx.endpoints.api_base_url, &snapshot.entries);
            if let Some(entity) = snapshot.owner.as_ref().and_then(|owner| owner.entity()) {
                value["owner"] = serde_json::json!({"entity":entity});
            }
        }
    }
    Ok(())
}

/// The idempotency token that lets a retried write replay its first response.
fn idempotency_token(headers: &HeaderMap) -> Result<Option<String>, JsonError> {
    Ok(json::resumable::single(headers, "x-goog-gcs-idempotency-token")?.map(str::to_owned))
}

async fn restore(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    query: &BTreeMap<String, String>,
    target: ObjectTarget<i64>,
    conditions: ObjectConditions,
) -> Result<Value, JsonError> {
    json::validate_object_get_headers(request.headers())?;
    validate_bodyless_post(request, query).await?;
    let create_access = ctx.authorize(auth::OBJECTS_CREATE, &target.bucket).await?;
    let record = ctx
        .state
        .catalog
        .restore_object(
            target,
            conditions,
            RestoreAccess {
                restore: access,
                create: &create_access,
            },
        )
        .await?;
    json::object_resource(
        &record,
        &ctx.endpoints.api_base_url,
        json::Projection::NoAcl,
    )
}

async fn move_object(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    params: json::ObjectMoveParams,
) -> Result<Value, JsonError> {
    let json::ObjectMoveParams {
        names,
        conditions,
        projection,
    } = params;
    json::validate_object_get_headers(request.headers())?;
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    // The Node SDK serializes empty options as {}, without move semantics.
    if !bytes.is_empty()
        && serde_json::from_slice::<Value>(&bytes).ok() != Some(serde_json::json!({}))
    {
        return Err(JsonError::invalid_argument(
            "Move requires an empty body or an empty JSON object",
        ));
    }
    let source_read = ctx.authorize(auth::OBJECTS_GET, &names.bucket).await?;
    let source_delete = ctx.authorize(auth::OBJECTS_DELETE, &names.bucket).await?;
    let (bucket, source) = (names.bucket.clone(), names.source.clone());
    let record = ctx
        .state
        .catalog
        .move_object(
            names,
            conditions,
            MoveAccess {
                source_read: &source_read,
                source_delete: &source_delete,
                destination_create: access,
            },
        )
        .await
        .map_err(|error| match error {
            // Cloud Storage cannot tell a missing source from a denied one for a move.
            ObjectError::ObjectNotFound => {
                let host = ctx.endpoints.api_base_url.split("://").last().unwrap_or_default();
                let principal = match ctx.caller {
                    Some(auth::Caller::Principal(identity)) => identity.acl_entity()
                        .and_then(|entity| {
                            let email = entity.strip_prefix("user-")?;
                            auth::validate_local_creator_email(email).is_ok().then(|| email.to_owned())
                        })
                        .or_else(|| auth::validate_local_creator_email(identity.id()).is_ok().then(|| identity.id().to_owned())),
                    Some(auth::Caller::Anonymous) => Some("Anonymous caller".into()),
                    None => ctx.state.auth.local_creator_entity()
                        .and_then(|entity| entity.strip_prefix("user-"))
                        .map(str::to_owned),
                };
                let Some(principal) = principal else { return JsonError::forbidden() };
                JsonError::new(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    format!(
                        "{principal} does not have storage.objects.move access to the Google Cloud Storage object. Permission 'storage.objects.move' denied on resource '//{}/projects/_/buckets/{}/objects/{}' (or it may not exist).",
                        host.trim_end_matches('/'), bucket, source
                    ),
                )
            }
            error => error.into(),
        })?;
    json::object_resource(&record, &ctx.endpoints.api_base_url, projection)
}

async fn list(
    headers: &HeaderMap,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    params: &json::ObjectListParams,
) -> Result<Value, JsonError> {
    json::validate_object_get_headers(headers)?;
    let page = ctx
        .state
        .catalog
        .list_objects(params.query.clone(), params.max_results, access)
        .await?;
    if ctx.config.log_level == crate::diagnostics::LogLevel::Debug
        && params.empty_soft_deleted_message(&page, None).is_some()
    {
        log_empty_soft_deleted_listing(ctx, access, params, &page).await;
    }
    if params.empty_page {
        // The recorded `maxResults=0` listing held only its kind: no items, prefixes or token.
        return Ok(serde_json::json!({"kind": "storage#objects"}));
    }
    json::object_list(&page, params, &ctx.endpoints.api_base_url)
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
        .get_bucket(&params.query.bucket, access)
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
    object: ObjectTarget,
    conditions: ObjectConditions,
    projection: json::Projection,
    mode: json::WriteMode,
    retention_override: UnlockedOverride,
}

async fn update(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    target: UpdateTarget,
) -> Result<Value, JsonError> {
    let UpdateTarget {
        object,
        conditions,
        projection,
        mode,
        retention_override,
    } = target;
    json::validate_keyed_object_headers(request.headers(), &[crate::csek::DESTINATION_PREFIX])?;
    let key = json::request_key(request.headers())?;
    let etags = request.headers().clone();
    // shortcut: SDK invocation tokens do not replay responses; use metageneration conditions for retry safety.
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    let mut update = json::parse_object_metadata_mutation(&bytes, mode)?;
    retention_access::authorize_update(
        ctx.state,
        access,
        &object.bucket,
        &mut update.update,
        retention_override,
    )
    .await?;
    let base = ctx.endpoints.api_base_url.clone();
    let record = ctx
        .state
        .catalog
        .update_object_metadata(
            object,
            conditions,
            update.update,
            move |record| {
                // A wrong key fails before the metadata change commits.
                crate::csek::check_metadata(record.customer_key_sha256, key)
                    .map_err(ObjectError::CustomerEncryption)?;
                crate::http_conditions::require_etag_match(&etags, &json::object_etag(record))
                    .map_err(|error| match error {
                        crate::http_conditions::EtagError::Invalid => {
                            ObjectError::InvalidFixedMetadata(
                                "Invalid If-Match precondition".into(),
                            )
                        }
                        crate::http_conditions::EtagError::PreconditionFailed => {
                            ObjectError::PreconditionFailed
                        }
                    })?;
                json::ObjectMetadataRequest::validate_unchanged(&update.unchanged, record, &base)
            },
            access,
        )
        .await?;
    json::object_resource_keyed(&record, &ctx.endpoints.api_base_url, projection, key)
}
