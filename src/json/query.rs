use super::buckets::validate_bucket_name_for_profile;
use super::{
    EtagConditions, FieldSelection, JsonError, ResponseCache, Schema, decode_page_token,
    integer::take_int64,
};
use crate::{
    bucket::{BucketConditions, name::MAX_BUCKET_NAME_BYTES},
    config::CompatProfile,
    decimal::Grammar,
};
use hyper::{Method, StatusCode, Uri};
use std::collections::BTreeMap;

pub(crate) const MAX_QUERY_BYTES: usize = 16 * 1024;

/// The path prefix of every bucket-scoped JSON API resource.
pub(super) const BUCKET_PATH: &str = "/storage/v1/b/";
/// The longest raw bucket segment a path may carry: every byte of the longest name
/// percent-encoded.
pub(super) const MAX_BUCKET_SEGMENT_BYTES: usize = 3 * MAX_BUCKET_NAME_BYTES;
/// The longest raw object segment: every byte of the longest name percent-encoded.
pub(super) const MAX_OBJECT_SEGMENT_BYTES: usize = 3 * crate::object::name::MAX_OBJECT_NAME_BYTES;

/// The raw bucket segment of `/storage/v1/b/{bucket}{suffix}`: one nonempty segment without `/`.
pub(super) fn bucket_subresource<'a>(path: &'a str, suffix: &str) -> Option<&'a str> {
    let segment = path.strip_prefix(BUCKET_PATH)?.strip_suffix(suffix)?;
    (!segment.is_empty() && !segment.contains('/')).then_some(segment)
}

/// Which profiles check the name of a bucket a path names. Bucket item and notification paths
/// check native names only: the fake-gcs-server profile served any decoded name there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BucketCheck {
    ForProfile,
    NativeOnly,
}

/// The decoded bucket of a raw path segment: at most [`MAX_BUCKET_SEGMENT_BYTES`] encoded
/// bytes, then decoded, then checked.
pub(super) fn path_bucket(
    segment: &str,
    profile: CompatProfile,
    check: BucketCheck,
) -> Result<String, JsonError> {
    let bucket = decode_bucket_segment(segment)?;
    if check == BucketCheck::ForProfile || profile == CompatProfile::Native {
        validate_bucket_name_for_profile(&bucket, profile)?;
    }
    Ok(bucket)
}

/// The decoded bucket of a raw path segment of at most [`MAX_BUCKET_SEGMENT_BYTES`] encoded bytes,
/// for a caller that checks the name by its own rules.
pub(crate) fn decode_bucket_segment(segment: &str) -> Result<String, JsonError> {
    if segment.len() > MAX_BUCKET_SEGMENT_BYTES {
        return Err(JsonError::invalid_parameter("Invalid bucket name"));
    }
    decode(segment, Decoding::Path)
}

/// Reads an advisory parameter with the same decoding as the validating route parser.
pub(crate) fn query_param<'a>(query: &'a str, name: &str) -> Option<std::borrow::Cow<'a, str>> {
    fn component(raw: &str) -> Option<std::borrow::Cow<'_, str>> {
        if raw.contains(['%', '+']) {
            decode(raw, Decoding::Form)
                .ok()
                .map(std::borrow::Cow::Owned)
        } else {
            Some(std::borrow::Cow::Borrowed(raw))
        }
    }
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (component(key)? == name)
            .then(|| component(value))
            .flatten()
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Projection {
    #[default]
    NoAcl,
    Full,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InsertParams {
    pub project: String,
    pub projection: Projection,
    pub enable_object_retention: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GetParams {
    pub conditions: BucketConditions,
    pub projection: Projection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListParams {
    pub project: String,
    pub projection: Projection,
    pub max_results: u32,
    pub prefix: String,
    /// Decoded last name, for a catalog keyset query; never a filesystem path.
    pub start_after: Option<String>,
}

/// `buckets.list` with `softDeleted=true`; the cursor is the last (name, generation).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SoftDeletedListParams {
    pub project: String,
    pub projection: Projection,
    pub max_results: u32,
    pub prefix: String,
    pub start_after: Option<(String, i64)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BucketRequest {
    /// `buckets.get` with `softDeleted=true` and the selected `generation`.
    GetSoftDeleted {
        bucket: String,
        generation: i64,
        projection: Projection,
    },
    ListSoftDeleted(SoftDeletedListParams),
    Restore {
        bucket: String,
        generation: i64,
        projection: Projection,
    },
    LockRetentionPolicy {
        bucket: String,
        metageneration: i64,
    },
    Insert(InsertParams),
    Get {
        bucket: String,
        params: GetParams,
    },
    Patch {
        bucket: String,
        params: GetParams,
    },
    Update {
        bucket: String,
        params: GetParams,
    },
    List(ListParams),
    Delete {
        bucket: String,
        conditions: BucketConditions,
    },
    GetIam {
        bucket: String,
    },
    SetIam {
        bucket: String,
    },
    TestIam {
        bucket: String,
        permissions: Vec<crate::auth::Permission>,
    },
    /// `buckets.getStorageLayout`; `prefix` only scopes the permission check (see the parser).
    GetStorageLayout {
        bucket: String,
    },
}

impl BucketRequest {
    /// The project a project-scoped call names: an insert or a listing.
    pub(crate) fn project_mut(&mut self) -> Option<&mut String> {
        match self {
            Self::Insert(InsertParams { project, .. })
            | Self::List(ListParams { project, .. })
            | Self::ListSoftDeleted(SoftDeletedListParams { project, .. }) => Some(project),
            Self::GetSoftDeleted { .. }
            | Self::Restore { .. }
            | Self::LockRetentionPolicy { .. }
            | Self::Get { .. }
            | Self::Patch { .. }
            | Self::Update { .. }
            | Self::Delete { .. }
            | Self::GetIam { .. }
            | Self::SetIam { .. }
            | Self::TestIam { .. }
            | Self::GetStorageLayout { .. } => None,
        }
    }

    pub(crate) fn projection(&self) -> Projection {
        match self {
            Self::GetSoftDeleted { projection, .. } | Self::Restore { projection, .. } => {
                *projection
            }
            Self::ListSoftDeleted(params) => params.projection,
            Self::Insert(params) => params.projection,
            Self::Get { params, .. } | Self::Patch { params, .. } | Self::Update { params, .. } => {
                params.projection
            }
            Self::List(params) => params.projection,
            Self::LockRetentionPolicy { .. }
            | Self::Delete { .. }
            | Self::GetIam { .. }
            | Self::SetIam { .. }
            | Self::TestIam { .. }
            | Self::GetStorageLayout { .. } => Projection::NoAcl,
        }
    }

    /// Whether a full projection renders ACL fields. The ACL entries of a soft-deleted version
    /// are kept for restore but not rendered, and a storage layout has none.
    pub(crate) fn renders_acl(&self) -> bool {
        match self {
            Self::GetSoftDeleted { .. }
            | Self::ListSoftDeleted(_)
            | Self::GetStorageLayout { .. } => false,
            Self::Restore { .. }
            | Self::LockRetentionPolicy { .. }
            | Self::Insert(_)
            | Self::Get { .. }
            | Self::Patch { .. }
            | Self::Update { .. }
            | Self::List(_)
            | Self::Delete { .. }
            | Self::GetIam { .. }
            | Self::SetIam { .. }
            | Self::TestIam { .. } => true,
        }
    }

    /// Only the recorded bucket list carries a cache header; other bucket operations keep none.
    pub(crate) fn response_cache(&self) -> ResponseCache {
        match self {
            Self::List(_) => ResponseCache::Resource,
            Self::GetSoftDeleted { .. }
            | Self::ListSoftDeleted(_)
            | Self::Restore { .. }
            | Self::LockRetentionPolicy { .. }
            | Self::Insert(_)
            | Self::Get { .. }
            | Self::Patch { .. }
            | Self::Update { .. }
            | Self::Delete { .. }
            | Self::GetIam { .. }
            | Self::SetIam { .. }
            | Self::TestIam { .. }
            | Self::GetStorageLayout { .. } => ResponseCache::Unrecorded,
        }
    }

    /// How the request's entity-tag condition headers are treated.
    #[expect(
        clippy::match_same_arms,
        reason = "bucket updates and listings ignore the entity-tag headers for different recorded reasons, each kept beside its comment"
    )]
    pub(crate) fn etag_conditions(&self) -> EtagConditions {
        match self {
            Self::Get { .. } | Self::GetIam { .. } => EtagConditions::Supported,
            // Google's .NET client sends the fetched resource's etag as If-Match on these writes.
            // Cloud Storage was observed to ignore both entity-tag headers on bucket updates, so
            // a stale tag still applies; setIamPolicy evaluates If-Match.
            Self::Patch { .. } | Self::Update { .. } => EtagConditions::Ignored,
            Self::SetIam { .. } => EtagConditions::MatchOnly,
            // A listing has no entity tag. Cloud Storage ignored both headers on the object
            // listing (case obj-collection-validators); bucket listings are treated the same way.
            Self::List(_) | Self::ListSoftDeleted(_) => EtagConditions::Ignored,
            Self::GetSoftDeleted { .. }
            | Self::Restore { .. }
            | Self::LockRetentionPolicy { .. }
            | Self::Insert(_)
            | Self::Delete { .. }
            | Self::TestIam { .. }
            | Self::GetStorageLayout { .. } => EtagConditions::Unsupported,
        }
    }

    /// Whether the method takes a request body.
    pub(crate) fn accepts_body(&self) -> bool {
        match self {
            Self::Insert(_)
            | Self::SetIam { .. }
            | Self::Patch { .. }
            | Self::Update { .. }
            | Self::Restore { .. }
            | Self::LockRetentionPolicy { .. } => true,
            Self::GetSoftDeleted { .. }
            | Self::ListSoftDeleted(_)
            | Self::Get { .. }
            | Self::List(_)
            | Self::Delete { .. }
            | Self::GetIam { .. }
            | Self::TestIam { .. }
            | Self::GetStorageLayout { .. } => false,
        }
    }

    /// Where the response takes its `fields` selection from; a delete answers without a body.
    pub(crate) fn fields(&self) -> FieldSelection {
        match self {
            Self::TestIam { .. } => FieldSelection::TestPermissions,
            Self::Delete { .. } => FieldSelection::Ignored,
            Self::List(_) | Self::ListSoftDeleted(_) => FieldSelection::Query(Schema::Buckets),
            Self::GetIam { .. } | Self::SetIam { .. } => FieldSelection::Query(Schema::Policy),
            Self::GetStorageLayout { .. } => FieldSelection::Query(Schema::BucketStorageLayout),
            Self::GetSoftDeleted { .. }
            | Self::Restore { .. }
            | Self::LockRetentionPolicy { .. }
            | Self::Insert(_)
            | Self::Get { .. }
            | Self::Patch { .. }
            | Self::Update { .. } => FieldSelection::Query(Schema::Bucket),
        }
    }
}

/// Match the raw URI before decoding any bucket path segment.
#[cfg(test)]
pub(crate) fn route(method: &Method, uri: &Uri) -> Option<Result<BucketRequest, JsonError>> {
    route_with_profile(method, uri, CompatProfile::Native, "local-project")
}

/// `POST /storage/v1/b/BUCKET/lockRetentionPolicy`; `segment` is the raw bucket segment.
fn parse_lock_retention_policy(
    method: &Method,
    segment: &str,
    uri: &Uri,
    profile: CompatProfile,
) -> Result<BucketRequest, JsonError> {
    if *method != Method::POST {
        return Err(JsonError::method_not_allowed(
            "Use POST to lock a retention policy",
            None,
        ));
    }
    let bucket = decode(segment, Decoding::Path)?;
    validate_bucket_name_for_profile(&bucket, profile)?;
    let mut query = parse_query(uri.query().unwrap_or_default())?;
    super::Fields::take(&mut query, super::Schema::Bucket)?;
    common(&mut query)?;
    let metageneration = query.remove("ifMetagenerationMatch").ok_or_else(|| {
        JsonError::required(
            "ifMetagenerationMatch is required; read the bucket metageneration first",
        )
    })?;
    // Unlike the other int64 parameters, this one keeps accepting a leading `+` (`%2B1`).
    let metageneration = Grammar::Signed
        .parse::<i64>(&metageneration)
        .ok_or_else(|| JsonError::invalid_parameter("ifMetagenerationMatch must be an integer"))?;
    finish(&query)?;
    Ok(BucketRequest::LockRetentionPolicy {
        bucket,
        metageneration,
    })
}

/// `POST /storage/v1/b/BUCKET/restore?generation=G`; `segment` is the raw bucket segment.
fn parse_restore(
    method: &Method,
    segment: &str,
    uri: &Uri,
    profile: CompatProfile,
) -> Result<BucketRequest, JsonError> {
    if *method != Method::POST {
        return Err(JsonError::method_not_allowed(
            "Use POST to restore a soft-deleted bucket",
            Some("POST"),
        ));
    }
    let bucket = decode(segment, Decoding::Path)?;
    validate_bucket_name_for_profile(&bucket, profile)?;
    let mut query = parse_query(uri.query().unwrap_or_default())?;
    super::Fields::take(&mut query, super::Schema::Bucket)?;
    common(&mut query)?;
    let generation = bucket_generation(&mut query)?.ok_or_else(generation_required)?;
    let projection = projection(&mut query)?;
    finish(&query)?;
    Ok(BucketRequest::Restore {
        bucket,
        generation,
        projection,
    })
}

fn bucket_generation(query: &mut BTreeMap<String, String>) -> Result<Option<i64>, JsonError> {
    query
        .remove("generation")
        .map(|value| {
            Grammar::SignedNoPlus
                .parse::<i64>(&value)
                .filter(|generation| *generation > 0)
                .ok_or_else(|| JsonError::invalid_parameter("generation must be a positive int64"))
        })
        .transpose()
}

/// Matches the observed Cloud Storage message for a soft-deleted read without a generation.
fn generation_required() -> JsonError {
    JsonError::required("Bucket generation is required.")
}

/// `GET /storage/v1/b?softDeleted=true`; called after the shared list parameters are parsed.
fn soft_deleted_list(
    query: &mut BTreeMap<String, String>,
    project: String,
    projection: Projection,
    max_results: u32,
    prefix: String,
) -> Result<BucketRequest, JsonError> {
    let start_after = query
        .remove("pageToken")
        .filter(|token| !token.is_empty())
        .map(|token| super::decode_soft_deleted_page_token(&token, &project, &prefix))
        .transpose()?;
    finish(query)?;
    Ok(BucketRequest::ListSoftDeleted(SoftDeletedListParams {
        project,
        projection,
        max_results,
        prefix,
        start_after,
    }))
}

/// `GET /storage/v1/b/BUCKET?softDeleted=true&generation=G`. `softDeleted` and `generation`
/// require each other; metageneration conditions are not modeled for soft-deleted reads.
fn soft_deleted_get(
    query: &mut BTreeMap<String, String>,
    bucket: &str,
) -> Result<Option<BucketRequest>, JsonError> {
    let soft_deleted = boolean(query, "softDeleted")?;
    let generation = bucket_generation(query)?;
    let generation = match (soft_deleted, generation) {
        (false, None) => return Ok(None),
        (true, Some(generation)) => generation,
        (true, None) => return Err(generation_required()),
        (false, Some(_)) => {
            return Err(JsonError::required(
                "generation selects a soft-deleted bucket; add softDeleted=true",
            ));
        }
    };
    unsupported(
        query,
        &["ifMetagenerationMatch", "ifMetagenerationNotMatch"],
    )?;
    let projection = projection(query)?;
    finish(query)?;
    Ok(Some(BucketRequest::GetSoftDeleted {
        bucket: bucket.to_owned(),
        generation,
        projection,
    }))
}

/// `GET /storage/v1/b/BUCKET/storageLayout`; `segment` is the raw bucket segment.
///
/// The reference checks `storage.objects.list` for `prefix` when it is set. Local IAM has no
/// conditions (bindings with one are rejected), so a grant always covers the whole bucket and
/// the bucket-level check already decides every prefix; the value is accepted and not stored.
fn parse_storage_layout(
    method: &Method,
    segment: &str,
    uri: &Uri,
    profile: CompatProfile,
) -> Result<BucketRequest, JsonError> {
    if *method != Method::GET {
        return Err(JsonError::method_not_allowed(
            "Use GET to read a bucket storage layout",
            Some("GET"),
        ));
    }
    let bucket = decode(segment, Decoding::Path)?;
    validate_bucket_name_for_profile(&bucket, profile)?;
    let mut query = parse_query(uri.query().unwrap_or_default())?;
    super::Fields::take(&mut query, super::Schema::BucketStorageLayout)?;
    common(&mut query)?;
    query.remove("prefix");
    finish(&query)?;
    Ok(BucketRequest::GetStorageLayout { bucket })
}

/// `GET` or `PUT /storage/v1/b/BUCKET/iam`; `segment` is the raw bucket segment.
fn parse_iam(
    method: &Method,
    segment: &str,
    uri: &Uri,
    profile: CompatProfile,
) -> Result<BucketRequest, JsonError> {
    // Unlike the other bucket paths, IAM decodes the segment before it checks the length.
    let bucket = decode(segment, Decoding::Path)?;
    if segment.len() > MAX_BUCKET_SEGMENT_BYTES {
        return Err(JsonError::invalid_parameter("Invalid bucket name"));
    }
    if profile == CompatProfile::Native {
        validate_bucket_name_for_profile(&bucket, profile)?;
    }
    if !matches!(*method, Method::GET | Method::PUT) {
        return Err(JsonError::method_not_allowed(
            "HTTP method is not supported for this IAM endpoint",
            Some("GET, PUT"),
        ));
    }
    let mut query = parse_query(uri.query().unwrap_or_default())?;
    super::Fields::take(&mut query, super::Schema::Policy)?;
    common(&mut query)?;
    if *method == Method::GET {
        // Cloud Storage answered an out-of-range version such as -1 with its version 1 policy
        // (recorded 2026-10-31), so only a value that is not an integer is a bad parameter.
        if let Some(version) = query.remove("optionsRequestedPolicyVersion")
            && Grammar::Signed.parse::<i32>(&version).is_none()
        {
            return Err(JsonError::invalid_parameter(
                "optionsRequestedPolicyVersion must be an integer",
            ));
        }
    }
    // Cloud Storage read the policy with an undeclared `gcsmimicUnknown=1` (case
    // bkt-probe-undeclared-query-resources); setIamPolicy shares this parser. fake-gcs-server
    // has no IAM route, so its profile keeps rejecting one.
    finish_for_profile(&query, profile)?;
    Ok(if *method == Method::GET {
        BucketRequest::GetIam { bucket }
    } else {
        BucketRequest::SetIam { bucket }
    })
}

pub(crate) fn route_with_profile(
    method: &Method,
    uri: &Uri,
    profile: CompatProfile,
    seed_project: &str,
) -> Option<Result<BucketRequest, JsonError>> {
    const COLLECTION: &str = "/storage/v1/b";
    let path = uri.path();
    if let Some(segment) = bucket_subresource(path, "/lockRetentionPolicy") {
        return Some(parse_lock_retention_policy(method, segment, uri, profile));
    }
    if let Some(segment) = bucket_subresource(path, "/restore") {
        return Some(parse_restore(method, segment, uri, profile));
    }
    if let Some(segment) = bucket_subresource(path, "/storageLayout") {
        return Some(parse_storage_layout(method, segment, uri, profile));
    }
    if let Some(result) = super::iam::route_test_permissions(method, uri, profile) {
        return Some(result);
    }
    if let Some(segment) = bucket_subresource(path, "/iam") {
        return Some(parse_iam(method, segment, uri, profile));
    }
    let segment = if path == COLLECTION {
        None
    } else {
        let segment = path.strip_prefix(BUCKET_PATH)?;
        if segment.is_empty() || segment.contains('/') {
            return None;
        }
        Some(segment)
    };
    Some(parse_bucket_route(
        method,
        segment,
        uri,
        profile,
        seed_project,
    ))
}

/// The collection (`segment` is `None`) or one bucket: the segment's length, decoding and
/// native name checks, then the method.
fn parse_bucket_route(
    method: &Method,
    segment: Option<&str>,
    uri: &Uri,
    profile: CompatProfile,
    seed_project: &str,
) -> Result<BucketRequest, JsonError> {
    let bucket = segment
        .map(|segment| path_bucket(segment, profile, BucketCheck::NativeOnly))
        .transpose()?;
    let call = BucketMethod::select(method, bucket)?;
    parse(call, uri.query().unwrap_or_default(), profile, seed_project)
}

/// A `/storage/v1/b` or `/storage/v1/b/{bucket}` method, chosen once from the HTTP method.
enum BucketMethod {
    Insert,
    List,
    /// GET, PATCH or PUT of one bucket, which share their query parameters.
    Item {
        bucket: String,
        method: ItemMethod,
    },
    Delete {
        bucket: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemMethod {
    Get,
    Patch,
    Update,
}

impl BucketMethod {
    fn select(method: &Method, bucket: Option<String>) -> Result<Self, JsonError> {
        let item = |bucket, method| Self::Item { bucket, method };
        match (method, bucket) {
            (&Method::POST, None) => Ok(Self::Insert),
            (&Method::GET, None) => Ok(Self::List),
            (&Method::GET, Some(bucket)) => Ok(item(bucket, ItemMethod::Get)),
            (&Method::PATCH, Some(bucket)) => Ok(item(bucket, ItemMethod::Patch)),
            (&Method::PUT, Some(bucket)) => Ok(item(bucket, ItemMethod::Update)),
            (&Method::DELETE, Some(bucket)) => Ok(Self::Delete { bucket }),
            (_, bucket) => Err(JsonError::method_not_allowed(
                "HTTP method is not supported for this bucket endpoint",
                Some(if bucket.is_some() {
                    "GET, DELETE, PATCH, PUT"
                } else {
                    "GET, POST"
                }),
            )),
        }
    }
}

fn parse(
    call: BucketMethod,
    raw: &str,
    profile: CompatProfile,
    seed_project: &str,
) -> Result<BucketRequest, JsonError> {
    let mut query = parse_query(raw)?;
    match &call {
        BucketMethod::Delete { .. } => {
            query.remove("fields");
        }
        BucketMethod::List => {
            super::Fields::take(&mut query, super::Schema::Buckets)?;
        }
        BucketMethod::Insert | BucketMethod::Item { .. } => {
            super::Fields::take(&mut query, super::Schema::Bucket)?;
        }
    }
    common(&mut query)?;
    match call {
        BucketMethod::Insert => {
            let enable_object_retention = boolean(&mut query, "enableObjectRetention")?;
            if profile == CompatProfile::FakeGcsServer {
                query
                    .entry("project".into())
                    .or_insert_with(|| seed_project.into());
            }
            let project = project(&mut query)?;
            let projection = projection(&mut query)?;
            query.remove("predefinedAcl");
            query.remove("predefinedDefaultObjectAcl");
            finish_for_profile(&query, profile)?;
            Ok(BucketRequest::Insert(InsertParams {
                project,
                projection,
                enable_object_retention,
            }))
        }
        BucketMethod::List => parse_bucket_list(&mut query, profile, seed_project),
        BucketMethod::Item { bucket, method } => {
            parse_bucket_item(&mut query, bucket, method, profile)
        }
        BucketMethod::Delete { bucket } => {
            let conditions = conditions(&mut query)?;
            finish_for_profile(&query, profile)?;
            Ok(BucketRequest::Delete { bucket, conditions })
        }
    }
}

/// `GET`, `PATCH` or `PUT /storage/v1/b/{bucket}`.
fn parse_bucket_item(
    query: &mut BTreeMap<String, String>,
    bucket: String,
    method: ItemMethod,
    profile: CompatProfile,
) -> Result<BucketRequest, JsonError> {
    let predefined =
        query.contains_key("predefinedAcl") || query.contains_key("predefinedDefaultObjectAcl");
    // Recorded 2026-10-31: native PATCH without projection returned acl, defaultObjectAcl and
    // owner; PUT did so only with predefined ACL parameters. Insert/get default to noAcl. The
    // fake-gcs-server profile retains its earlier PUT-only full default.
    let default_projection = match (profile, method) {
        (CompatProfile::FakeGcsServer, ItemMethod::Update)
        | (CompatProfile::Native, ItemMethod::Patch) => Projection::Full,
        (CompatProfile::Native, ItemMethod::Update) if predefined => Projection::Full,
        (CompatProfile::FakeGcsServer, ItemMethod::Get | ItemMethod::Patch)
        | (CompatProfile::Native, ItemMethod::Get | ItemMethod::Update) => Projection::NoAcl,
    };
    match method {
        ItemMethod::Get => {
            if let Some(request) = soft_deleted_get(query, &bucket)? {
                return Ok(request);
            }
        }
        ItemMethod::Patch | ItemMethod::Update => {
            query.remove("predefinedAcl");
            query.remove("predefinedDefaultObjectAcl");
        }
    }
    let conditions = conditions(query)?;
    let projection = projection_or(query, default_projection)?;
    finish_for_profile(query, profile)?;
    let params = GetParams {
        conditions,
        projection,
    };
    Ok(match method {
        ItemMethod::Get => BucketRequest::Get { bucket, params },
        ItemMethod::Patch => BucketRequest::Patch { bucket, params },
        ItemMethod::Update => BucketRequest::Update { bucket, params },
    })
}

/// `GET /storage/v1/b`: the project's buckets, or its soft-deleted ones.
fn parse_bucket_list(
    query: &mut BTreeMap<String, String>,
    profile: CompatProfile,
    seed_project: &str,
) -> Result<BucketRequest, JsonError> {
    let soft_deleted = boolean(query, "softDeleted")?;
    false_only(query, "returnPartialSuccess")?;
    if profile == CompatProfile::FakeGcsServer {
        query
            .entry("project".into())
            .or_insert_with(|| seed_project.into());
    }
    let project = project(query)?;
    let projection = projection(query)?;
    let page_cap = if projection == Projection::Full {
        200
    } else {
        1000
    };
    let max_results = list_page_size(query, page_cap)?;
    let prefix = query.remove("prefix").unwrap_or_default();
    if prefix.len() > MAX_BUCKET_NAME_BYTES {
        return Err(JsonError::invalid_parameter("prefix exceeds 222 bytes"));
    }
    if soft_deleted {
        return soft_deleted_list(query, project, projection, max_results, prefix);
    }
    let start_after = query
        .remove("pageToken")
        .filter(|token| !token.is_empty())
        .map(|token| decode_page_token(&token, &project, &prefix))
        .transpose()?;
    finish_for_profile(query, profile)?;
    Ok(BucketRequest::List(ListParams {
        project,
        projection,
        max_results,
        prefix,
        start_after,
    }))
}

pub(crate) fn parse_query(raw: &str) -> Result<BTreeMap<String, String>, JsonError> {
    if raw.len() > MAX_QUERY_BYTES {
        return Err(JsonError::invalid_parameter("Query exceeds 16 KiB"));
    }
    let mut query = BTreeMap::new();
    for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode(key, Decoding::Form)?;
        let value = decode(value, Decoding::Form)?;
        if query.insert(key.clone(), value).is_some() {
            return Err(JsonError::invalid_parameter(format!(
                "Duplicate query parameter: {key}"
            )));
        }
    }
    Ok(query)
}

/// Parameters every JSON method may carry and gcs-mimic does not implement. Accepting one would
/// report success for a query token or a JSONP response that never happened.
const UNSUPPORTED_STANDARD_PARAMETERS: &[&str] = &["oauth_token", "callback"];

/// Standard parameters whose value can change neither the caller nor the result here, so they
/// are discarded unchecked:
///
/// - `key`: Google uses an API key for quota attribution, never as a credential, and anonymous
///   gsutil sends one on every request.
/// - `userProject`: it names the project billed for a Requester Pays bucket. A gcs-mimic bucket
///   cannot enable Requester Pays, and Cloud Storage served the parameter on a bucket and a
///   project without it as if it were absent (cases bkt-edges-hmac-keys and
///   bkt-edges-notification-ids).
/// - `quotaUser` and `userIp`: they attribute quota, which gcs-mimic does not meter. Cloud
///   Storage answered buckets.get with either one as it does without (case
///   bkt-unsupported-parameters).
const IGNORED_STANDARD_PARAMETERS: &[&str] = &["key", "userProject", "quotaUser", "userIp"];

pub(super) fn ignore_accepted_parameters(query: &mut BTreeMap<String, String>) {
    for name in IGNORED_STANDARD_PARAMETERS {
        query.remove(*name);
    }
}

/// Applies the standard parameters a route shares with every other JSON method: discards the
/// ignored ones, then rejects `route_specific` followed by the standard parameters gcs-mimic
/// does not implement. `rejection` words the error for the route's API surface.
pub(super) fn standard_parameters(
    query: &mut BTreeMap<String, String>,
    route_specific: &[&str],
    rejection: fn(&str) -> JsonError,
) -> Result<(), JsonError> {
    ignore_accepted_parameters(query);
    route_specific
        .iter()
        .chain(UNSUPPORTED_STANDARD_PARAMETERS)
        .find(|name| query.contains_key(**name))
        .map_or(Ok(()), |name| Err(rejection(name)))
}

pub(super) fn common(query: &mut BTreeMap<String, String>) -> Result<(), JsonError> {
    standard_parameters(query, &["uploadType"], JsonError::unsupported)?;
    // The routing layer applies the value; prettyPrint=false selects compact bodies.
    boolean(query, "prettyPrint")?;
    if let Some(alt) = query.remove("alt") {
        match alt.as_str() {
            "json" => {}
            "media" => return Err(JsonError::unsupported("alt=media")),
            _ => {
                return Err(JsonError::new(
                    StatusCode::BAD_REQUEST,
                    "invalidAltValue",
                    "alt must be json for bucket metadata",
                ));
            }
        }
    }
    Ok(())
}

fn unsupported(query: &BTreeMap<String, String>, keys: &[&str]) -> Result<(), JsonError> {
    for key in keys {
        if query.contains_key(*key) {
            return Err(JsonError::unsupported(key));
        }
    }
    Ok(())
}

/// The `maxResults` of a bucket list: `0` or absent selects the page cap, which also bounds larger
/// values.
fn list_page_size(query: &mut BTreeMap<String, String>, page_cap: u32) -> Result<u32, JsonError> {
    let Some(value) = query.remove("maxResults") else {
        return Ok(page_cap);
    };
    let size = Grammar::Digits.parse::<u32>(&value).ok_or_else(|| {
        JsonError::invalid_parameter("maxResults must be a uint32").at_parameter("maxResults")
    })?;
    Ok(if size == 0 {
        page_cap
    } else {
        size.min(page_cap)
    })
}

fn project(query: &mut BTreeMap<String, String>) -> Result<String, JsonError> {
    let project = query.remove("project").ok_or_else(|| {
        JsonError::required("Required parameter: project").at_parameter("project")
    })?;
    validate_project(&project)?;
    Ok(project)
}

pub(crate) fn validate_project(project: &str) -> Result<(), JsonError> {
    if !crate::auth::valid_project_id(project) {
        return Err(JsonError::invalid_parameter(
            "project must be a nonempty identifier of at most 256 bytes",
        ));
    }
    Ok(())
}

pub(super) fn projection(query: &mut BTreeMap<String, String>) -> Result<Projection, JsonError> {
    projection_or(query, Projection::NoAcl)
}

fn projection_or(
    query: &mut BTreeMap<String, String>,
    default: Projection,
) -> Result<Projection, JsonError> {
    match query.remove("projection").as_deref() {
        None => Ok(default),
        Some("noAcl") => Ok(Projection::NoAcl),
        Some("full") => Ok(Projection::Full),
        Some(other) => Err(JsonError::invalid_parameter(format!(
            "Invalid string value: '{other}'. Allowed values: [full, noacl]"
        ))
        .at_parameter("projection")),
    }
}

pub(super) fn boolean(query: &mut BTreeMap<String, String>, key: &str) -> Result<bool, JsonError> {
    match query.remove(key).as_deref() {
        None | Some("false" | "False") => Ok(false),
        Some("true" | "True") => Ok(true),
        Some(_) => Err(
            JsonError::invalid_parameter(format!("{key} must be true or false")).at_parameter(key),
        ),
    }
}

fn false_only(query: &mut BTreeMap<String, String>, key: &str) -> Result<(), JsonError> {
    if boolean(query, key)? {
        Err(JsonError::unsupported(key))
    } else {
        Ok(())
    }
}

fn conditions(query: &mut BTreeMap<String, String>) -> Result<BucketConditions, JsonError> {
    Ok(BucketConditions {
        if_metageneration_match: take_int64(query, "ifMetagenerationMatch")?,
        if_metageneration_not_match: take_int64(query, "ifMetagenerationNotMatch")?,
    })
}

/// [`finish`] in the fake-gcs-server profile only. The native profile ignores a query parameter
/// the method does not declare, as Cloud Storage did on every JSON method recorded with one (for
/// example `append=false` on a bucket insert and PATCH, recorded 2026-10-31). A method whose
/// fake-gcs-server route reads only the parameters it knows skips `finish` in both profiles
/// instead.
pub(super) fn finish_for_profile(
    query: &BTreeMap<String, String>,
    profile: CompatProfile,
) -> Result<(), JsonError> {
    if profile == CompatProfile::Native {
        Ok(())
    } else {
        finish(query)
    }
}

pub(super) fn finish(query: &BTreeMap<String, String>) -> Result<(), JsonError> {
    match query.first_key_value() {
        Some((key, _)) => Err(JsonError::invalid_parameter(format!(
            "Unknown query parameter for this method: {key}"
        ))
        .at_parameter(key)),
        None => Ok(()),
    }
}

/// How `decode` treats `+`: form-encoded query components use it for a space, while path
/// segments keep it literally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decoding {
    Path,
    Form,
}

pub(crate) fn decode(raw: &str, decoding: Decoding) -> Result<String, JsonError> {
    let invalid = || JsonError::invalid_parameter("Malformed URI encoding");
    let mut bytes = Vec::with_capacity(raw.len());
    let mut input = raw.bytes();
    while let Some(byte) = input.next() {
        bytes.push(match byte {
            b'%' => crate::hex::byte(&[
                input.next().ok_or_else(invalid)?,
                input.next().ok_or_else(invalid)?,
            ])
            .ok_or_else(invalid)?,
            b'+' if decoding == Decoding::Form => b' ',
            byte => byte,
        });
    }
    String::from_utf8(bytes).map_err(|_| invalid())
}
