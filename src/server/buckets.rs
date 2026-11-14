//! JSON API bucket operations: one handler per operation after shared authorization and
//! request validation.

use super::RequestBody as Incoming;
use super::{
    JSON_RESOURCE_CACHE_CONTROL, QueryPrecondition, RESOURCE_BODY_LIMIT, State,
    acl::{self, AclSubjects, BucketVersion, Rendered},
    authorize_bucket, authorize_project, bucket_access, empty_response, json_response_selected,
    read_metadata_response, require_empty_body, response_fields, validate_bodyless_post,
};
use crate::{
    auth::{self, Caller},
    bucket::{BucketAccess, BucketConditions, BucketRecord, CatalogError},
    config::CompatProfile,
    http_conditions::EtagError,
    json::{self, BucketRequest, JsonError, WriteMode},
    storage::failure::unavailable,
};
use http_body_util::Full;
use hyper::{
    HeaderMap, Request, Response, StatusCode,
    body::Bytes,
    header::{HeaderValue, IF_MATCH},
};
use serde_json::Value;

/// What every bucket operation needs besides its request.
#[derive(Clone, Copy)]
struct BucketCtx<'a> {
    state: &'a State,
    base_url: &'a str,
    caller: Option<Caller<'a>>,
    profile: CompatProfile,
}

pub(super) async fn respond<'a>(
    request: Request<Incoming>,
    operation: BucketRequest,
    caller: Option<Caller<'a>>,
    state: &'a State,
    profile: CompatProfile,
    base_url: &'a str,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let ctx = BucketCtx {
        state,
        base_url,
        caller,
        profile,
    };
    let access = authorize(ctx, &operation).await?;
    validate_request(&request, &operation)?;
    let fields = response_fields(request.uri().query(), operation.fields())?;
    let acl_full = operation.projection() == json::Projection::Full && operation.renders_acl();
    // A write that turns uniform bucket-level access off answers without ACL fields, as one that
    // turns it on does (recorded 2026-10-31), so the state before the write matters too.
    let acl_full = acl_full
        && !(profile == CompatProfile::Native
            && leaves_uniform_access(state, &access, &operation).await?);
    let cache = operation.response_cache();
    let mut rendered = match operation {
        BucketRequest::LockRetentionPolicy {
            bucket,
            metageneration,
        } => lock_retention_policy(request, ctx, &access, &bucket, metageneration).await?,
        BucketRequest::TestIam {
            bucket,
            permissions,
        } => without_acl(test_iam_permissions(ctx, &bucket, permissions).await?),
        BucketRequest::Patch { bucket, params } => {
            write(request, ctx, &access, &bucket, params, WriteMode::Patch).await?
        }
        BucketRequest::Update { bucket, params } => {
            write(request, ctx, &access, &bucket, params, WriteMode::Replace).await?
        }
        BucketRequest::GetIam { bucket } => {
            return get_iam(&request, ctx, &access, &bucket, &fields).await;
        }
        BucketRequest::SetIam { bucket } => {
            without_acl(set_iam(request, ctx, &access, &bucket).await?)
        }
        BucketRequest::Insert(params) => insert(request, ctx, params).await?,
        BucketRequest::Get { bucket, params } => {
            return get(&request, ctx, &access, &bucket, params, &fields).await;
        }
        BucketRequest::List(params) => list(ctx, params).await?,
        BucketRequest::GetStorageLayout { bucket } => {
            let record = state
                .catalog
                .get_bucket(&bucket, &access)
                .await?
                .ok_or_else(|| bucket_catalog_error(CatalogError::NotFound, profile, None))?;
            without_acl(json::storage_layout_resource(&record, profile))
        }
        BucketRequest::Delete { bucket, conditions } => {
            return delete(ctx, &access, &bucket, conditions).await;
        }
        BucketRequest::GetSoftDeleted {
            bucket,
            generation,
            projection,
        } => {
            let version = state
                .catalog
                .get_soft_deleted_bucket(&bucket, generation, &access)
                .await?;
            without_acl(json::soft_deleted_bucket_resource(
                &version, base_url, projection, profile,
            )?)
        }
        BucketRequest::ListSoftDeleted(params) => {
            without_acl(list_soft_deleted(ctx, params).await?)
        }
        BucketRequest::Restore {
            bucket,
            generation,
            projection,
        } => {
            let record = state
                .catalog
                .restore_bucket(&bucket, generation, &access)
                .await?;
            rendered_bucket(&record, base_url, projection, profile)?
        }
    };
    if acl_full {
        acl::add_full_projection(state, caller, &mut rendered, base_url).await?;
        add_legacy_uniform_owner(&mut rendered.value, profile);
    }
    Ok(json_response_selected(&mut rendered.value, &fields, cache))
}

/// A response that renders no ACL fields: a policy, a permission test, a storage layout or a
/// soft-deleted version, whose stored ACL is kept for restore but not rendered.
fn without_acl(value: Value) -> Rendered {
    Rendered {
        value,
        subjects: AclSubjects::None,
    }
}

/// The resource of `record`, whose live ACL a full projection adds.
fn rendered_bucket(
    record: &BucketRecord,
    base_url: &str,
    projection: json::Projection,
    profile: CompatProfile,
) -> Result<Rendered, JsonError> {
    Ok(Rendered {
        value: json::bucket_resource_with_profile(record, base_url, projection, profile)?,
        subjects: AclSubjects::Bucket(record.into()),
    })
}

/// The fake-gcs-server profile keeps reporting the project owners team as `owner` for a bucket under
/// uniform bucket-level access; Cloud Storage reports none (recorded 2026-10-31).
fn add_legacy_uniform_owner(value: &mut Value, profile: CompatProfile) {
    let uniform = value
        .get("iamConfiguration")
        .and_then(|value| value.get("uniformBucketLevelAccess"))
        .and_then(|value| value.get("enabled"))
        .and_then(Value::as_bool)
        == Some(true);
    if profile == CompatProfile::FakeGcsServer && uniform && value.get("owner").is_none() {
        let number = value["projectNumber"].as_str().unwrap_or("0").to_owned();
        if let Some(value) = value.as_object_mut() {
            value.insert(
                "owner".into(),
                serde_json::json!({"entity": format!("project-owners-{number}")}),
            );
        }
    }
}

/// Whether a PATCH or PUT starts on a bucket whose uniform bucket-level access is enabled.
async fn leaves_uniform_access(
    state: &State,
    access: &BucketAccess,
    operation: &BucketRequest,
) -> Result<bool, JsonError> {
    let (BucketRequest::Patch { bucket, .. } | BucketRequest::Update { bucket, .. }) = operation
    else {
        return Ok(false);
    };
    Ok(state
        .catalog
        .get_bucket(bucket, access)
        .await?
        .is_some_and(|record| record.uniform_bucket_level_access_locked_time.is_some()))
}

async fn authorize(
    ctx: BucketCtx<'_>,
    operation: &BucketRequest,
) -> Result<BucketAccess, JsonError> {
    let BucketCtx { state, caller, .. } = ctx;
    Ok(match operation {
        BucketRequest::Insert(params) => {
            authorize_project(caller, auth::BUCKETS_CREATE, &params.project)?;
            if params.enable_object_retention {
                authorize_project(
                    caller,
                    auth::BUCKETS_ENABLE_OBJECT_RETENTION,
                    &params.project,
                )?;
            }
            BucketAccess::Permissive
        }
        BucketRequest::List(params) => {
            authorize_project(caller, auth::BUCKETS_LIST, &params.project)?;
            BucketAccess::Permissive
        }
        BucketRequest::Get { bucket, .. } => {
            authorize_bucket(state, caller, auth::BUCKETS_GET, bucket).await?
        }
        BucketRequest::Patch { bucket, .. }
        | BucketRequest::Update { bucket, .. }
        | BucketRequest::LockRetentionPolicy { bucket, .. } => {
            authorize_bucket(state, caller, auth::BUCKETS_UPDATE, bucket).await?
        }
        BucketRequest::Delete { bucket, .. } => {
            authorize_bucket(state, caller, auth::BUCKETS_DELETE, bucket).await?
        }
        BucketRequest::GetIam { bucket } => {
            authorize_bucket(state, caller, auth::BUCKETS_GET_IAM, bucket).await?
        }
        BucketRequest::SetIam { bucket } => {
            authorize_bucket(state, caller, auth::BUCKETS_SET_IAM, bucket).await?
        }
        BucketRequest::TestIam { .. } => BucketAccess::Permissive,
        // The reference requires `storage.objects.list`, not a `storage.buckets.*` permission.
        BucketRequest::GetStorageLayout { bucket } => {
            authorize_bucket(state, caller, auth::OBJECTS_LIST, bucket).await?
        }
        BucketRequest::ListSoftDeleted(params) => {
            authorize_project(caller, auth::BUCKETS_LIST, &params.project)?;
            BucketAccess::Permissive
        }
        BucketRequest::GetSoftDeleted { .. } => soft_deleted_access(caller, auth::BUCKETS_GET)?,
        BucketRequest::Restore { .. } => soft_deleted_access(caller, auth::BUCKETS_RESTORE)?,
    })
}

/// A soft-deleted bucket has no live policy and its project is known only to the catalog,
/// so the caller is authenticated here and the catalog checks the project-level grant.
fn soft_deleted_access(
    caller: Option<Caller<'_>>,
    permission: auth::Permission,
) -> Result<BucketAccess, JsonError> {
    let Some(caller) = caller else {
        return Ok(BucketAccess::Permissive);
    };
    caller.authorize(permission, None)?;
    let identity = match caller {
        Caller::Anonymous => None,
        Caller::Principal(identity) => Some(identity.clone()),
    };
    Ok(BucketAccess::Policy {
        identity,
        permission,
        project: None,
    })
}

/// Header and body checks that precede every operation's own validation.
fn validate_request(
    request: &Request<Incoming>,
    operation: &BucketRequest,
) -> Result<(), JsonError> {
    json::validate_read_condition_headers(request.headers(), operation.etag_conditions())?;
    if !operation.accepts_body() {
        require_empty_body(request.headers())?;
    }
    Ok(())
}

async fn lock_retention_policy(
    request: Request<Incoming>,
    ctx: BucketCtx<'_>,
    access: &BucketAccess,
    bucket: &str,
    metageneration: i64,
) -> Result<Rendered, JsonError> {
    validate_bodyless_post(
        request,
        &[("ifMetagenerationMatch".into(), metageneration.to_string())].into(),
    )
    .await?;
    let record = ctx
        .state
        .catalog
        .patch_bucket(
            bucket,
            crate::bucket::BucketMetadataUpdate {
                retention_policy: crate::bucket::RetentionPolicyUpdate::Lock,
                ..Default::default()
            },
            BucketConditions {
                if_metageneration_match: Some(metageneration),
                ..Default::default()
            },
            access,
        )
        .await?;
    rendered_bucket(&record, ctx.base_url, json::Projection::NoAcl, ctx.profile)
}

async fn test_iam_permissions(
    ctx: BucketCtx<'_>,
    bucket: &str,
    permissions: Vec<auth::Permission>,
) -> Result<Value, JsonError> {
    // This method requires no permission; existence is independent of policy visibility.
    ctx.state
        .catalog
        .get_policy(bucket, &BucketAccess::Permissive)
        .await?;
    let mut granted = Vec::new();
    for permission in permissions {
        match bucket_access(ctx.state, ctx.caller, permission, bucket).await {
            Ok(access)
                if ctx
                    .state
                    .catalog
                    .has_bucket_permission(bucket, &access)
                    .await? =>
            {
                granted.push(permission.as_str())
            }
            Ok(_) => {}
            Err(error) if error.is_refusal() => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(serde_json::json!({"kind": "storage#testIamPermissionsResponse", "permissions": granted}))
}

async fn write(
    request: Request<Incoming>,
    ctx: BucketCtx<'_>,
    access: &BucketAccess,
    bucket: &str,
    params: json::GetParams,
    mode: WriteMode,
) -> Result<Rendered, JsonError> {
    let BucketCtx { state, profile, .. } = ctx;
    let acl_query = request.uri().query().map(str::to_owned);
    let bytes = json::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    // Only the native profile checks the body name against the path (recorded 2026-10-31).
    if profile == CompatProfile::Native {
        json::require_matching_bucket_name(&bytes, bucket)?;
    }
    let mut update = match mode {
        WriteMode::Replace => json::parse_bucket_replacement(&bytes, profile)?,
        WriteMode::Patch => json::parse_bucket_patch(&bytes, profile)?,
    };
    let record = state
        .catalog
        .get_bucket(bucket, access)
        .await?
        .ok_or(CatalogError::NotFound)?;
    let predefined = json::bucket_predefined_acls(
        acl_query.as_deref(),
        json::UniformAccess::of(
            record.uniform_bucket_level_access_locked_time,
            update.uniform_bucket_level_access_enabled,
        ),
        json::BucketAclWrite::Update,
        profile,
    )?;
    if let Some(acl) = predefined.acl {
        update.acl = Some(acl.entries(&record.project));
    }
    if let Some(acl) = predefined.default_object_acl {
        update.default_object_acl = Some(acl.entries(&record.project));
    }
    let conditions = params.conditions;
    let match_failed = conditions
        .if_metageneration_match
        .is_some_and(|value| value != record.metageneration);
    // Observed on Cloud Storage: a PATCH or PUT answers 304 (no body) when
    // ifMetagenerationNotMatch names the current value, whether or not the body carries a
    // writable setting, and even when ifMetagenerationMatch fails too (recorded 2026-11). An
    // object mutation reports the failed match instead. That this check runs before the
    // catalog's own validation (for example a locked policy being shortened) is an inference.
    // Nothing is written on this path, so reading the metageneration outside the catalog's
    // transaction cannot let a mutation slip past its condition.
    if conditions.if_metageneration_not_match == Some(record.metageneration) {
        return Err(JsonError::not_modified());
    }
    let record = match mode {
        WriteMode::Replace => {
            state
                .catalog
                .replace_bucket(bucket, update, conditions, access)
                .await
        }
        WriteMode::Patch => {
            state
                .catalog
                .patch_bucket(bucket, update, conditions, access)
                .await
        }
    }
    .map_err(|error| {
        let failed = matches!(error, CatalogError::PreconditionFailed);
        let error = bucket_catalog_error(error, profile, None);
        // Observed 2026-10-31 for PATCH (empty or with labels) and PUT whose
        // ifMetagenerationMatch failed. The wording is reused for a failed
        // ifMetagenerationNotMatch (unobserved), but `If-Match` is named only when
        // ifMetagenerationMatch failed.
        if failed && profile == CompatProfile::Native && match_failed {
            error.at_if_match()
        } else {
            error
        }
    })?;
    rendered_bucket(&record, ctx.base_url, params.projection, profile)
}

async fn get_iam(
    request: &Request<Incoming>,
    ctx: BucketCtx<'_>,
    access: &BucketAccess,
    bucket: &str,
    fields: &json::Fields,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let policy = ctx.state.catalog.get_policy(bucket, access).await?;
    // The catalog renders every stored policy with an etag from its revision.
    let etag = policy
        .etag
        .as_deref()
        .ok_or_else(|| JsonError::service_unavailable("Stored IAM policy has no etag"))?;
    read_metadata_response(
        request.headers(),
        etag,
        json::policy_resource(&policy, bucket),
        fields,
        QueryPrecondition::Unmatched,
        super::IAM_POLICY_CACHE_CONTROL,
    )
}

async fn set_iam(
    request: Request<Incoming>,
    ctx: BucketCtx<'_>,
    access: &BucketAccess,
    bucket: &str,
) -> Result<Value, JsonError> {
    let if_match = policy_if_match_lines(request.headers());
    let bytes = json::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    let policy = json::parse_policy(&bytes)?;
    let policy =
        ctx.state
            .catalog
            .set_policy_checked(bucket, policy, access, move |current| {
                let etag = current
                    .etag
                    .as_deref()
                    .ok_or_else(|| unavailable("stored IAM policy has no etag"))?;
                // A quoted form of the bare tag getIamPolicy returns in its ETag header and body.
                crate::http_conditions::require_etag_match(&if_match, &format!("\"{etag}\""))
                    .map_err(|error| match error {
                        EtagError::Invalid => {
                            CatalogError::InvalidMetadata("Invalid If-Match precondition".into())
                        }
                        EtagError::PreconditionFailed => CatalogError::PreconditionFailed,
                    })
            })
            .await
            // Cloud Storage names the `If-Match` header for a stale policy etag.
            .map_err(|error| {
                let stale = matches!(error, CatalogError::PreconditionFailed);
                let error = JsonError::from(error);
                if stale { error.at_if_match() } else { error }
            })?;
    Ok(json::policy_resource(&policy, bucket))
}

/// Copies the `If-Match` field lines for evaluation inside the catalog transaction, quoting a
/// bare element. A policy's JSON `etag` is unquoted base64 and Google.Apis copies the fetched
/// body's `etag` verbatim into If-Match, so a bare value names that same opaque tag.
fn policy_if_match_lines(headers: &HeaderMap) -> HeaderMap {
    let mut lines = HeaderMap::new();
    for value in headers.get_all(IF_MATCH) {
        let tag = value.as_bytes().trim_ascii();
        let bare = !tag.is_empty() && tag != b"*" && !tag.contains(&b'"') && !tag.contains(&b',');
        let quoted = bare
            .then(|| HeaderValue::from_bytes(&[b"\"", tag, b"\""].concat()).ok())
            .flatten();
        lines.append(IF_MATCH, quoted.unwrap_or_else(|| value.clone()));
    }
    lines
}

async fn insert(
    request: Request<Incoming>,
    ctx: BucketCtx<'_>,
    params: json::InsertParams,
) -> Result<Rendered, JsonError> {
    let BucketCtx { state, profile, .. } = ctx;
    let acl_query = request.uri().query().map(str::to_owned);
    let bytes = json::read_body(request.into_body(), RESOURCE_BODY_LIMIT).await?;
    let mut record = json::parse_insert_with_profile(
        &bytes,
        &params.project,
        state.clock.now(),
        profile,
        &state.default_location,
    )?;
    if record.object_retention_enabled && !params.enable_object_retention {
        return Err(JsonError::invalid(
            "objectRetention is output-only; add the enableObjectRetention=true query parameter to enable object retention",
        ));
    }
    record.object_retention_enabled = params.enable_object_retention;
    let name = record.name.clone();
    json::bucket_predefined_acls(
        acl_query.as_deref(),
        json::UniformAccess::of(record.uniform_bucket_level_access_locked_time, None),
        json::BucketAclWrite::Insert,
        profile,
    )?;
    let record = state
        .catalog
        .insert_bucket(record)
        .await
        .map_err(|error| bucket_catalog_error(error, profile, Some(&name)))?;
    rendered_bucket(&record, ctx.base_url, params.projection, profile)
}

async fn get(
    request: &Request<Incoming>,
    ctx: BucketCtx<'_>,
    access: &BucketAccess,
    bucket: &str,
    params: json::GetParams,
    fields: &json::Fields,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let BucketCtx {
        state,
        caller,
        profile,
        ..
    } = ctx;
    let record = state
        .catalog
        .get_bucket(bucket, access)
        .await?
        .ok_or_else(|| bucket_catalog_error(CatalogError::NotFound, profile, None))?;
    let conditions = params.conditions;
    if conditions
        .if_metageneration_match
        .is_some_and(|v| v != record.metageneration)
    {
        return Err(CatalogError::PreconditionFailed.into());
    }
    let query = if conditions.if_metageneration_not_match == Some(record.metageneration) {
        QueryPrecondition::NotModified
    } else {
        QueryPrecondition::Unmatched
    };
    if query == QueryPrecondition::NotModified
        && !request.headers().contains_key("if-match")
        && !request.headers().contains_key("if-none-match")
    {
        return Ok(empty_response(StatusCode::NOT_MODIFIED));
    }
    let mut resource = rendered_bucket(&record, ctx.base_url, params.projection, profile)?;
    if params.projection == json::Projection::Full {
        acl::add_full_projection(state, caller, &mut resource, ctx.base_url).await?;
        add_legacy_uniform_owner(&mut resource.value, profile);
    }
    read_metadata_response(
        request.headers(),
        &json::bucket_etag(&record),
        resource.value,
        fields,
        query,
        JSON_RESOURCE_CACHE_CONTROL,
    )
}

async fn list(ctx: BucketCtx<'_>, params: json::ListParams) -> Result<Rendered, JsonError> {
    let limit = params.max_results as usize;
    let page = ctx
        .state
        .catalog
        .list_buckets_page(
            &params.project,
            Some(&params.prefix),
            params.start_after.as_deref(),
            limit,
        )
        .await?;
    // A page with more results always holds the bucket the next page continues after.
    let token = page
        .items
        .last()
        .filter(|_| page.has_more)
        .map(|last| json::encode_page_token(&params.project, &params.prefix, &last.name))
        .transpose()?;
    let value = json::bucket_list_with_profile(
        &page.items,
        token.as_deref(),
        ctx.base_url,
        params.projection,
        ctx.profile,
    )?;
    Ok(Rendered {
        value,
        subjects: AclSubjects::Buckets(page.items.iter().map(BucketVersion::from).collect()),
    })
}

async fn list_soft_deleted(
    ctx: BucketCtx<'_>,
    params: json::SoftDeletedListParams,
) -> Result<Value, JsonError> {
    let page = ctx
        .state
        .catalog
        .list_soft_deleted_buckets(
            &params.project,
            &params.prefix,
            params.start_after,
            params.max_results as usize,
        )
        .await?;
    let token = page
        .items
        .last()
        .filter(|_| page.has_more)
        .map(|last| {
            json::encode_soft_deleted_page_token(
                &params.project,
                &params.prefix,
                &last.record.name,
                last.record.generation,
            )
        })
        .transpose()?;
    json::soft_deleted_bucket_list(
        &page.items,
        token.as_deref(),
        ctx.base_url,
        params.projection,
        ctx.profile,
    )
}

async fn delete(
    ctx: BucketCtx<'_>,
    access: &BucketAccess,
    bucket: &str,
    conditions: BucketConditions,
) -> Result<Response<Full<Bytes>>, JsonError> {
    ctx.state
        .catalog
        .delete_bucket(bucket, conditions, access)
        .await
        .map_err(|error| bucket_catalog_error(error, ctx.profile, None))?;
    Ok(if ctx.profile == CompatProfile::FakeGcsServer {
        let mut response = Response::new(Full::new(Bytes::from_static(b"null\n")));
        response.headers_mut().insert(
            "content-type",
            hyper::header::HeaderValue::from_static("application/json"),
        );
        response
    } else {
        empty_response(StatusCode::NO_CONTENT)
    })
}

/// The fake-gcs-server profile words four bucket failures its own way.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other catalog error keeps the shared JSON mapping"
)]
fn bucket_catalog_error(
    error: CatalogError,
    profile: CompatProfile,
    name: Option<&str>,
) -> JsonError {
    if profile != CompatProfile::FakeGcsServer {
        return error.into();
    }
    match error {
        CatalogError::NotFound => JsonError::new(StatusCode::NOT_FOUND, "notFound", "Not Found"),
        CatalogError::PreconditionFailed => JsonError::new(
            StatusCode::PRECONDITION_FAILED,
            "conditionNotMet",
            "Precondition failed",
        ),
        CatalogError::NotEmpty => JsonError::new(
            StatusCode::PRECONDITION_FAILED,
            "conditionNotMet",
            "bucket must be empty prior to deletion",
        ),
        CatalogError::AlreadyExists => JsonError::new(
            StatusCode::CONFLICT,
            "conflict",
            format!(
                "A Cloud Storage bucket named '{}' already exists. Try another name. Bucket names must be globally unique across all Google Cloud projects, including those outside of your organization.",
                name.unwrap_or_default()
            ),
        ),
        error => error.into(),
    }
}
