//! JSON API object operations: shared authorization and validation, then one handler per
//! operation. Uploads, reads and derived writes (copy and compose) live in submodules.

mod derived;
mod read;
mod upload;

use super::RequestBody as Incoming;
use super::{
    EmptyJsonBody, ObjectCtx, RESOURCE_BODY_LIMIT, ResponseBody,
    acl::{self, AclSubjects, ObjectVersion, Rendered},
    boxed, empty_response, json_response_selected, require_empty_body, response_fields, resumable,
    retention_access, validate_bodyless_post,
};
use crate::{
    auth,
    bucket::{BucketAccess, CatalogError},
    csek::CustomerKey,
    http_header::{TextError, TextOrder},
    json::ApiError,
    json::{self, ObjectRequest, UploadRequest},
    object::{
        IdempotencyToken, ObjectConditions, ObjectError, ObjectRecord, ObjectTarget, TextTokenError,
    },
    storage::{MoveAccess, RestoreAccess, UploadAclSnapshot},
};
use hyper::{HeaderMap, Request, Response, StatusCode, Uri};
use serde_json::Value;
use std::collections::BTreeMap;

/// Where a written object's `projection=full` ACL fields come from.
enum AclSource {
    /// The live ACL, read after the write.
    Live,
    /// An idempotent upload or copy reports the ACL its first response recorded.
    Replay(UploadAclSnapshot),
}

impl From<Option<UploadAclSnapshot>> for AclSource {
    fn from(snapshot: Option<UploadAclSnapshot>) -> Self {
        snapshot.map_or(Self::Live, Self::Replay)
    }
}

/// A written object's resource and where its ACL fields come from.
struct Written {
    rendered: Rendered,
    acl: AclSource,
}

impl Written {
    /// The resource of `record`, rendered for the customer key it was written with.
    fn keyed(
        record: &ObjectRecord,
        ctx: ObjectCtx<'_>,
        projection: json::Projection,
        key: Option<CustomerKey>,
        acl: AclSource,
    ) -> Result<Self, ApiError> {
        let value =
            json::object_resource_keyed(record, ctx.endpoints.api_base_url(), projection, key)?;
        Ok(Self::object(value, record, acl))
    }

    /// `value` renders `record`.
    fn object(value: Value, record: &ObjectRecord, acl: AclSource) -> Self {
        Self {
            rendered: Rendered {
                value,
                subjects: AclSubjects::Object(record.into()),
            },
            acl,
        }
    }

    /// A response whose ACL fields come from the live ACL of `subjects`.
    fn live(value: Value, subjects: AclSubjects) -> Self {
        Self {
            rendered: Rendered { value, subjects },
            acl: AclSource::Live,
        }
    }
}

pub(super) async fn respond(
    request: Request<Incoming>,
    operation: ObjectRequest,
    ctx: ObjectCtx<'_>,
) -> Result<Response<ResponseBody>, ApiError> {
    let fields = response_fields(request.uri().query(), operation.fields())?;
    let (permission, bucket) = operation.required_permission();
    // Authorization precedes header/body validation, conditions and existence checks.
    // Replacing a live object additionally needs storage.objects.delete. The access carries
    // that decision, and publication enforces it inside the catalog transaction if the name is
    // live, so a concurrent create cannot be overwritten.
    let access = ctx.authorize(permission, bucket).await?;
    let query = crate::percent::parse_query(request.uri().query().unwrap_or_default())?;
    let acl_full = operation.projection() == json::Projection::Full;
    if query.contains_key("predefinedAcl") || query.contains_key("destinationPredefinedAcl") {
        check_predefined_acl(request.uri(), ctx, bucket, &operation, &access).await?;
    }
    validate_request(request.headers(), &operation)?;
    let cache = operation.response_cache();
    let validator_header = operation.sends_etag_header();
    let written = match operation {
        ObjectRequest::Restore { target, conditions } => {
            restore(request, ctx, &access, &query, target, conditions).await?
        }
        ObjectRequest::Move(params) => move_object(request, ctx, &access, params).await?,
        ObjectRequest::Copy(params) => derived::copy(request, ctx, access, params).await?,
        ObjectRequest::Compose(params) => derived::compose(request, ctx, access, params).await?,
        ObjectRequest::Resumable(UploadRequest {
            bucket,
            name,
            params,
            content_encoding,
        }) => {
            return resumable::initiate(
                request,
                &bucket,
                name,
                params,
                content_encoding,
                access,
                ctx,
            )
            .await
            .map_err(ApiError::for_upload);
        }
        ObjectRequest::List(params) => list(request.headers(), ctx, &access, &params).await?,
        ObjectRequest::Update(params) => update(request, ctx, &access, params).await?,
        ObjectRequest::Multipart(target) => upload::multipart(request, ctx, access, target)
            .await
            .map_err(ApiError::for_upload)?,
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
            ctx.await_catalog(ctx.state.catalog.delete_object(target, conditions, &access))
                .await?;
            return Ok(boxed(empty_response(StatusCode::NO_CONTENT)));
        }
    };
    let Written {
        mut rendered,
        acl: source,
    } = written;
    if acl_full {
        add_acl(ctx, &mut rendered, source).await?;
    }
    let mut response = json_response_selected(&mut rendered.value, &fields, cache);
    if !validator_header {
        response.headers_mut().remove("etag");
    }
    Ok(boxed(response))
}

/// Uploads apply `predefinedAcl` and copy and compose `destinationPredefinedAcl` outside
/// uniform access and for an authenticated caller; every other parameter, and any predefined ACL
/// under uniform access or from an anonymous caller, keeps its explicit 400. Publication
/// rechecks uniform access and public access prevention atomically.
async fn check_predefined_acl(
    uri: &Uri,
    ctx: ObjectCtx<'_>,
    bucket: &str,
    operation: &ObjectRequest,
    access: &BucketAccess,
) -> Result<(), ApiError> {
    let record = ctx
        .state
        .catalog
        .get_bucket(bucket, &BucketAccess::Permissive)
        .await?
        .ok_or(CatalogError::NotFound)?;
    json::reject_predefined_acl(
        uri.query(),
        record.uniform_access(None),
        operation.predefined_acl_target(),
        access,
    )
}

/// Header and body checks that precede every operation's own validation.
fn validate_request(headers: &HeaderMap, operation: &ObjectRequest) -> Result<(), ApiError> {
    if let Some(etags) = operation.etag_conditions() {
        json::validate_read_condition_headers(headers, etags)?;
    }
    if !operation.accepts_body() {
        require_empty_body(headers)?;
    }
    Ok(())
}

/// Adds the full projection's ACL fields to a written object's resource.
async fn add_acl(
    ctx: ObjectCtx<'_>,
    rendered: &mut Rendered,
    source: AclSource,
) -> Result<(), ApiError> {
    let snapshot = match source {
        AclSource::Live => {
            return acl::add_full_projection(
                ctx.state,
                ctx.caller,
                rendered,
                ctx.endpoints.api_base_url(),
            )
            .await;
        }
        AclSource::Replay(snapshot) => snapshot,
    };
    let Some(snapshot) = snapshot.recorded()? else {
        return Ok(());
    };
    // A replay is recorded only by an upload or a copy, which render one object.
    let mismatch = || {
        ApiError::from(ObjectError::Unavailable(
            "Upload replay snapshot does not match the replayed object".into(),
        ))
    };
    let AclSubjects::Object(object) = &rendered.subjects else {
        return Err(mismatch());
    };
    if !snapshot.matches_resource(&object.bucket, &object.name, object.generation) {
        return Err(mismatch());
    }
    let access = ctx.authorize(auth::OBJECTS_GET_IAM, &object.bucket).await?;
    if let Some(snapshot) = ctx
        .state
        .catalog
        .object_acl_snapshot(snapshot.clone(), &access)
        .await?
    {
        acl::add_object_acl(
            &mut rendered.value,
            ctx.endpoints.api_base_url(),
            object,
            &snapshot.entries,
            snapshot.owner.as_ref(),
        )?;
    }
    Ok(())
}

/// The token as replay text, after the JSON header checks have validated its bytes.
fn idempotency_token(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let token =
        IdempotencyToken::from_text(headers, TextOrder::ValueFirst).map_err(
            |error| match error {
                TextTokenError::Header(TextError::Repeated(_)) => {
                    ApiError::invalid_parameter("Duplicate x-goog-gcs-idempotency-token")
                }
                TextTokenError::Header(TextError::Invalid(_)) | TextTokenError::Empty => {
                    ApiError::invalid_parameter("Invalid x-goog-gcs-idempotency-token")
                }
            },
        )?;
    Ok(token.map(|token| token.as_str().to_owned()))
}

async fn restore(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    query: &BTreeMap<String, String>,
    target: ObjectTarget<i64>,
    conditions: ObjectConditions,
) -> Result<Written, ApiError> {
    json::validate_object_get_headers(request.headers())?;
    validate_bodyless_post(request, query, EmptyJsonBody::Accepted).await?;
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
    let value = json::object_resource(
        &record,
        ctx.endpoints.api_base_url(),
        json::Projection::NoAcl,
    )?;
    Ok(Written::object(value, &record, AclSource::Live))
}

async fn move_object(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    params: json::ObjectMoveParams,
) -> Result<Written, ApiError> {
    let json::ObjectMoveParams {
        names,
        conditions,
        projection,
    } = params;
    json::validate_object_get_headers(request.headers())?;
    let bytes = crate::bounded_body::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    // The Node SDK serializes empty options as {}, without move semantics.
    if !bytes.is_empty()
        && serde_json::from_slice::<Value>(&bytes).ok() != Some(serde_json::json!({}))
    {
        return Err(ApiError::invalid_argument(
            "Move requires an empty body or an empty JSON object",
        ));
    }
    let source_read = ctx.authorize(auth::OBJECTS_GET, &names.bucket).await?;
    let source_delete = ctx.authorize(auth::OBJECTS_DELETE, &names.bucket).await?;
    let (bucket, source) = (names.bucket.clone(), names.source.clone());
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "every other object error keeps the shared JSON mapping"
    )]
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
                let principal = match ctx.caller {
                    // An alias and a natural entity name at most 254 bytes of email, as a
                    // creator address does.
                    Some(auth::Caller::Principal(identity)) => identity.acl_entity()
                        .and_then(|entity| entity.email().map(str::to_owned))
                        .or_else(|| auth::validate_local_creator_email(identity.id()).is_ok().then(|| identity.id().to_owned())),
                    Some(auth::Caller::Anonymous) => Some("Anonymous caller".into()),
                    None => ctx.state.auth.local_creator_entity()
                        .and_then(crate::acl::Entity::user_id)
                        .map(str::to_owned),
                };
                let Some(principal) = principal else { return ApiError::forbidden() };
                ApiError::new(
                    StatusCode::FORBIDDEN,
                    "forbidden",
                    format!(
                        "{principal} does not have storage.objects.move access to the Google Cloud Storage object. Permission 'storage.objects.move' denied on resource '//{}/projects/_/buckets/{}/objects/{}' (or it may not exist).",
                        ctx.endpoints.api_authority(), bucket, source
                    ),
                )
            }
            error => error.into(),
        })?;
    let value = json::object_resource(&record, ctx.endpoints.api_base_url(), projection)?;
    Ok(Written::object(value, &record, AclSource::Live))
}

async fn list(
    headers: &HeaderMap,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    params: &json::ObjectListParams,
) -> Result<Written, ApiError> {
    json::validate_object_get_headers(headers)?;
    let page = ctx
        .state
        .catalog
        .list_objects(params.query.clone(), params.max_results, access)
        .await?;
    if params.empty_page {
        // The recorded `maxResults=0` listing held only its kind: no items, prefixes or token.
        return Ok(Written::live(
            serde_json::json!({"kind": "storage#objects"}),
            AclSubjects::None,
        ));
    }
    let value = json::object_list(&page, params, ctx.endpoints.api_base_url())?;
    let subjects = match params.projection {
        json::Projection::Full => AclSubjects::Objects {
            bucket: params.query.bucket.clone(),
            items: page.items.iter().map(ObjectVersion::from).collect(),
        },
        json::Projection::NoAcl => AclSubjects::None,
    };
    Ok(Written::live(value, subjects))
}

async fn update(
    request: Request<Incoming>,
    ctx: ObjectCtx<'_>,
    access: &BucketAccess,
    params: json::ObjectUpdateParams,
) -> Result<Written, ApiError> {
    let json::ObjectUpdateParams {
        target,
        conditions,
        projection,
        mode,
        retention_override,
        predefined_acl,
    } = params;
    json::validate_keyed_object_headers(request.headers(), &[crate::csek::DESTINATION_PREFIX])?;
    let key = json::request_key(request.headers())?;
    // If-Match is accepted and not evaluated: Cloud Storage applied metadata PATCH and PUT
    // requests naming a superseded or bogus entity tag (case obj-gaps-json). Only the query
    // conditions guard a concurrent update.
    // shortcut: SDK invocation tokens do not replay responses; use metageneration conditions for retry safety.
    let bytes = crate::bounded_body::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    let mut update = json::parse_object_metadata_mutation(&bytes, mode)?;
    update.update.acl = std::mem::take(&mut update.update.acl).with_predefined(predefined_acl)?;
    let target = update.select_target(target)?;
    retention_access::authorize_update(
        &ctx.state.catalog,
        access,
        &target.bucket,
        &mut update.update,
        retention_override,
    )
    .await?;
    let base = ctx.endpoints.api_base_url().to_owned();
    let base_source = ctx.endpoints.api_base_source();
    let record = ctx
        .state
        .catalog
        .update_object_metadata(
            target,
            conditions,
            update.update,
            move |record| {
                // A wrong key fails before the metadata change commits.
                crate::csek::check_metadata(record.customer_key_sha256, key)
                    .map_err(ObjectError::CustomerEncryption)?;
                json::ObjectMetadataRequest::validate_unchanged(
                    &update.unchanged,
                    record,
                    &base,
                    base_source,
                )
            },
            access,
        )
        .await?;
    Written::keyed(&record, ctx, projection, key, AclSource::Live)
}
