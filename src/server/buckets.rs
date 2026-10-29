//! JSON API bucket operations: one handler per operation after shared authorization and
//! request validation.

use super::{
    BUCKET_BODY_LIMIT, State, acl, authorize_bucket, authorize_project, empty_response,
    json_response, read_metadata_response, validate_bodyless_post,
};
use crate::{
    auth::{self, Caller},
    bucket::{BucketAccess, CatalogError},
    config::CompatProfile,
    json::{self, BucketRequest, JsonError},
};
use http_body_util::Full;
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
};
use serde_json::Value;

/// What every bucket operation needs besides its request.
#[derive(Clone, Copy)]
struct BucketCtx<'a> {
    state: &'a State,
    caller: Option<Caller<'a>>,
    profile: CompatProfile,
}

/// Whether a bucket write merges into or replaces the stored metadata.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BucketWrite {
    Patch,
    Replace,
}

pub(super) async fn respond<'a>(
    request: Request<Incoming>,
    operation: BucketRequest,
    caller: Option<Caller<'a>>,
    state: &'a State,
    profile: CompatProfile,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let ctx = BucketCtx {
        state,
        caller,
        profile,
    };
    let access = authorize(ctx, &operation).await?;
    validate_request(&request, &operation)?;
    let fields = response_fields(&request, &operation)?;
    let acl_full = acl::full_projection(request.method(), request.uri().query());
    let mut value = match operation {
        BucketRequest::LockRetentionPolicy {
            bucket,
            metageneration,
        } => lock_retention_policy(request, ctx, &access, &bucket, metageneration).await?,
        BucketRequest::TestIam {
            bucket,
            permissions,
        } => test_iam_permissions(ctx, &bucket, permissions).await?,
        BucketRequest::Patch { bucket, params } => {
            write(request, ctx, &access, &bucket, params, BucketWrite::Patch).await?
        }
        BucketRequest::Update { bucket, params } => {
            write(request, ctx, &access, &bucket, params, BucketWrite::Replace).await?
        }
        BucketRequest::GetIam { bucket } => {
            return get_iam(&request, ctx, &access, &bucket, &fields).await;
        }
        BucketRequest::SetIam { bucket } => set_iam(request, ctx, &access, &bucket).await?,
        BucketRequest::Insert(params) => insert(request, ctx, params).await?,
        BucketRequest::Get { bucket, params } => {
            return get(&request, ctx, &access, &bucket, params, &fields).await;
        }
        BucketRequest::List(params) => list(ctx, params).await?,
        BucketRequest::Delete { bucket, conditions } => {
            return delete(ctx, &access, &bucket, conditions).await;
        }
    };
    if acl_full {
        acl::add_full_projection(state, caller, &mut value).await?;
    }
    fields.apply(&mut value);
    Ok(json_response(&value))
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
    })
}

/// Header and body checks that precede every operation's own validation.
fn validate_request(
    request: &Request<Incoming>,
    operation: &BucketRequest,
) -> Result<(), JsonError> {
    json::validate_read_condition_headers(
        request.headers(),
        matches!(
            operation,
            BucketRequest::Get { .. } | BucketRequest::GetIam { .. }
        ),
    )?;
    if !matches!(
        operation,
        BucketRequest::Insert(_)
            | BucketRequest::SetIam { .. }
            | BucketRequest::Patch { .. }
            | BucketRequest::Update { .. }
            | BucketRequest::LockRetentionPolicy { .. }
    ) && (request.headers().contains_key("transfer-encoding")
        || request
            .headers()
            .get("content-length")
            .is_some_and(|v| v != "0"))
    {
        return Err(JsonError::invalid_argument(
            "This method requires an empty body",
        ));
    }
    Ok(())
}

fn response_fields(
    request: &Request<Incoming>,
    operation: &BucketRequest,
) -> Result<json::Fields, JsonError> {
    if matches!(operation, BucketRequest::TestIam { .. }) {
        return Ok(json::test_permissions_query(request.uri().query())?.1);
    }
    if matches!(operation, BucketRequest::Delete { .. }) {
        return json::Fields::from_query(None, "Bucket");
    }
    let schema = match operation {
        BucketRequest::List(_) => "Buckets",
        BucketRequest::GetIam { .. } | BucketRequest::SetIam { .. } => "Policy",
        _ => "Bucket",
    };
    json::Fields::from_query(request.uri().query(), schema)
}

async fn lock_retention_policy(
    request: Request<Incoming>,
    ctx: BucketCtx<'_>,
    access: &BucketAccess,
    bucket: &str,
    metageneration: i64,
) -> Result<Value, JsonError> {
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
                lock_retention_policy: true,
                ..Default::default()
            },
            Some(metageneration),
            None,
            access,
        )
        .await?;
    json::bucket_resource_with_profile(
        &record,
        &ctx.state.api_base_url,
        json::Projection::NoAcl,
        ctx.profile,
    )
}

async fn test_iam_permissions(
    ctx: BucketCtx<'_>,
    bucket: &str,
    permissions: Vec<String>,
) -> Result<Value, JsonError> {
    // This method requires no permission; existence is independent of policy visibility.
    ctx.state
        .catalog
        .get_policy(bucket, &BucketAccess::Permissive)
        .await?;
    let mut granted = Vec::new();
    for permission in permissions {
        match authorize_bucket(ctx.state, ctx.caller, &permission, bucket).await {
            Ok(_) => granted.push(permission),
            Err(error) if error.status == StatusCode::FORBIDDEN => {}
            Err(error) => return Err(error),
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
    mode: BucketWrite,
) -> Result<Value, JsonError> {
    let BucketCtx { state, profile, .. } = ctx;
    let acl_query = request.uri().query().map(str::to_owned);
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    let update = match mode {
        BucketWrite::Replace => json::parse_bucket_replacement(&bytes, profile)?,
        BucketWrite::Patch => json::parse_bucket_patch(&bytes, profile)?,
    };
    let record = state
        .catalog
        .get_bucket_authorized(bucket, access)
        .await?
        .ok_or(CatalogError::NotFound)?;
    json::reject_predefined_acl(
        acl_query.as_deref(),
        record.uniform_bucket_level_access_locked_time.is_some()
            || update.uniform_bucket_level_access_enabled == Some(true),
        false,
    )?;
    let conditions = params.conditions;
    let record = match mode {
        BucketWrite::Replace => {
            state
                .catalog
                .replace_bucket(
                    bucket,
                    update,
                    conditions.if_metageneration_match,
                    conditions.if_metageneration_not_match,
                    access,
                )
                .await
        }
        BucketWrite::Patch => {
            state
                .catalog
                .patch_bucket(
                    bucket,
                    update,
                    conditions.if_metageneration_match,
                    conditions.if_metageneration_not_match,
                    access,
                )
                .await
        }
    }
    .map_err(|error| bucket_catalog_error(error, profile, None))?;
    json::bucket_resource_with_profile(&record, &state.api_base_url, params.projection, profile)
}

async fn get_iam(
    request: &Request<Incoming>,
    ctx: BucketCtx<'_>,
    access: &BucketAccess,
    bucket: &str,
    fields: &json::Fields,
) -> Result<Response<Full<Bytes>>, JsonError> {
    let policy = ctx.state.catalog.get_policy(bucket, access).await?;
    read_metadata_response(
        request.headers(),
        &format!(
            "\"{}\"",
            policy.etag.as_deref().expect("stored policy etag")
        ),
        json::policy_resource(&policy, bucket),
        fields,
        false,
    )
}

async fn set_iam(
    request: Request<Incoming>,
    ctx: BucketCtx<'_>,
    access: &BucketAccess,
    bucket: &str,
) -> Result<Value, JsonError> {
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    let policy = json::parse_policy(&bytes)?;
    let policy = ctx.state.catalog.set_policy(bucket, policy, access).await?;
    Ok(json::policy_resource(&policy, bucket))
}

async fn insert(
    request: Request<Incoming>,
    ctx: BucketCtx<'_>,
    params: json::InsertParams,
) -> Result<Value, JsonError> {
    let BucketCtx { state, profile, .. } = ctx;
    let acl_query = request.uri().query().map(str::to_owned);
    let bytes = json::read_body(request.into_body(), BUCKET_BODY_LIMIT).await?;
    let mut record = json::parse_insert_with_profile(
        &bytes,
        &params.project,
        state.clock.now(),
        profile,
        &state.default_location,
    )?;
    if record.object_retention_enabled && !params.enable_object_retention {
        return Err(JsonError::invalid_argument(
            "objectRetention is output-only; add the enableObjectRetention=true query parameter to enable object retention",
        ));
    }
    record.object_retention_enabled = params.enable_object_retention;
    let name = record.name.clone();
    json::reject_predefined_acl(
        acl_query.as_deref(),
        record.uniform_bucket_level_access_locked_time.is_some(),
        false,
    )?;
    let record = state
        .catalog
        .insert_bucket(record)
        .await
        .map_err(|error| bucket_catalog_error(error, profile, Some(&name)))?;
    json::bucket_resource_with_profile(&record, &state.api_base_url, params.projection, profile)
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
    } = ctx;
    let record = state
        .catalog
        .get_bucket_authorized(bucket, access)
        .await?
        .ok_or_else(|| bucket_catalog_error(CatalogError::NotFound, profile, None))?;
    let conditions = params.conditions;
    if conditions
        .if_metageneration_match
        .is_some_and(|v| v != record.metageneration)
    {
        return Err(CatalogError::PreconditionFailed.into());
    }
    let query_not_modified = conditions.if_metageneration_not_match == Some(record.metageneration);
    if query_not_modified
        && !request.headers().contains_key("if-match")
        && !request.headers().contains_key("if-none-match")
    {
        return Ok(empty_response(StatusCode::NOT_MODIFIED));
    }
    let mut resource = json::bucket_resource_with_profile(
        &record,
        &state.api_base_url,
        params.projection,
        profile,
    )?;
    if params.projection == json::Projection::Full {
        acl::add_full_projection(state, caller, &mut resource).await?;
    }
    read_metadata_response(
        request.headers(),
        &json::bucket_etag(&record),
        resource,
        fields,
        query_not_modified,
    )
}

async fn list(ctx: BucketCtx<'_>, params: json::ListParams) -> Result<Value, JsonError> {
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
    let token = if page.has_more {
        Some(json::encode_page_token(
            &params.project,
            &params.prefix,
            &page.items.last().expect("nonempty continuation page").name,
        )?)
    } else {
        None
    };
    json::bucket_list_with_profile(
        &page.items,
        token.as_deref(),
        &ctx.state.api_base_url,
        params.projection,
        ctx.profile,
    )
}

async fn delete(
    ctx: BucketCtx<'_>,
    access: &BucketAccess,
    bucket: &str,
    conditions: json::Conditions,
) -> Result<Response<Full<Bytes>>, JsonError> {
    ctx.state
        .catalog
        .delete_bucket_authorized(
            bucket,
            conditions.if_metageneration_match,
            conditions.if_metageneration_not_match,
            access,
        )
        .await
        .map_err(|error| bucket_catalog_error(error, ctx.profile, None))?;
    Ok(if ctx.profile == CompatProfile::LegacyEmulator {
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

fn bucket_catalog_error(
    error: CatalogError,
    profile: CompatProfile,
    name: Option<&str>,
) -> JsonError {
    if profile == CompatProfile::LegacyEmulator {
        match error {
            CatalogError::NotFound => {
                return JsonError::new(StatusCode::NOT_FOUND, "notFound", "Not Found");
            }
            CatalogError::NotEmpty => {
                return JsonError::new(
                    StatusCode::PRECONDITION_FAILED,
                    "conditionNotMet",
                    "bucket must be empty prior to deletion",
                );
            }
            CatalogError::AlreadyExists => {
                return JsonError::new(
                    StatusCode::CONFLICT,
                    "conflict",
                    format!(
                        "A Cloud Storage bucket named '{}' already exists. Try another name. Bucket names must be globally unique across all Google Cloud projects, including those outside of your organization.",
                        name.unwrap_or_default()
                    ),
                );
            }
            _ => {}
        }
    }
    error.into()
}
