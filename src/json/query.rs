use super::buckets::validate_bucket_name_for_profile;
use super::{JsonError, decode_page_token};
use crate::{bucket::BucketConditions, config::CompatProfile};
use hyper::{Method, StatusCode, Uri};
use std::collections::BTreeMap;

pub(crate) const MAX_QUERY_BYTES: usize = 16 * 1024;

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
    super::Fields::take(&mut query, "Bucket")?;
    common(&mut query)?;
    let metageneration = query
        .remove("ifMetagenerationMatch")
        .ok_or_else(|| {
            JsonError::required(
                "ifMetagenerationMatch is required; read the bucket metageneration first",
            )
        })?
        .parse::<i64>()
        .map_err(|_| JsonError::invalid_parameter("ifMetagenerationMatch must be an integer"))?;
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
    super::Fields::take(&mut query, "Bucket")?;
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
            value
                .parse::<i64>()
                .ok()
                .filter(|generation| *generation > 0 && !value.starts_with('+'))
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
    super::Fields::take(&mut query, "BucketStorageLayout")?;
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
    let bucket = decode(segment, Decoding::Path)?;
    if segment.len() > 222 * 3 {
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
    super::Fields::take(&mut query, "Policy")?;
    common(&mut query)?;
    if *method == Method::GET {
        // Cloud Storage answered an out-of-range version such as -1 with its version 1 policy
        // (recorded 2026-10-31), so only a value that is not an integer is a bad parameter.
        if let Some(version) = query.remove("optionsRequestedPolicyVersion")
            && version.parse::<i32>().is_err()
        {
            return Err(JsonError::invalid_parameter(
                "optionsRequestedPolicyVersion must be an integer",
            ));
        }
        finish(&query)?;
        Ok(BucketRequest::GetIam { bucket })
    } else {
        finish(&query)?;
        Ok(BucketRequest::SetIam { bucket })
    }
}

pub(crate) fn route_with_profile(
    method: &Method,
    uri: &Uri,
    profile: CompatProfile,
    seed_project: &str,
) -> Option<Result<BucketRequest, JsonError>> {
    const COLLECTION: &str = "/storage/v1/b";
    if let Some(segment) = uri
        .path()
        .strip_prefix("/storage/v1/b/")
        .and_then(|path| path.strip_suffix("/lockRetentionPolicy"))
        && !segment.is_empty()
        && !segment.contains('/')
    {
        return Some(parse_lock_retention_policy(method, segment, uri, profile));
    }
    if let Some(segment) = uri
        .path()
        .strip_prefix("/storage/v1/b/")
        .and_then(|path| path.strip_suffix("/restore"))
        && !segment.is_empty()
        && !segment.contains('/')
    {
        return Some(parse_restore(method, segment, uri, profile));
    }
    if let Some(segment) = uri
        .path()
        .strip_prefix("/storage/v1/b/")
        .and_then(|path| path.strip_suffix("/storageLayout"))
        && !segment.is_empty()
        && !segment.contains('/')
    {
        return Some(parse_storage_layout(method, segment, uri, profile));
    }
    if let Some(result) = super::iam::route_test_permissions(method, uri, profile) {
        return Some(result);
    }
    if let Some(segment) = uri
        .path()
        .strip_prefix("/storage/v1/b/")
        .and_then(|path| path.strip_suffix("/iam"))
        && !segment.is_empty()
        && !segment.contains('/')
    {
        return Some(parse_iam(method, segment, uri, profile));
    }
    let bucket = if uri.path() == COLLECTION {
        None
    } else {
        let segment = uri.path().strip_prefix("/storage/v1/b/")?;
        if segment.is_empty() || segment.contains('/') {
            return None;
        }
        if segment.len() > 222 * 3 {
            return Some(Err(JsonError::invalid_parameter("Invalid bucket name")));
        }
        match decode(segment, Decoding::Path).and_then(|name| {
            if profile == CompatProfile::Native {
                validate_bucket_name_for_profile(&name, profile)?;
            }
            Ok(name)
        }) {
            Ok(name) => Some(name),
            Err(error) => return Some(Err(error)),
        }
    };
    let allowed = matches!(
        (bucket.is_some(), method),
        (false, &Method::GET | &Method::POST)
            | (
                true,
                &Method::GET | &Method::DELETE | &Method::PATCH | &Method::PUT
            )
    );
    if !allowed {
        let error = JsonError::method_not_allowed(
            "HTTP method is not supported for this bucket endpoint",
            Some(if bucket.is_some() {
                "GET, DELETE, PATCH, PUT"
            } else {
                "GET, POST"
            }),
        );
        return Some(Err(error));
    }
    Some(parse(
        method,
        bucket,
        uri.query().unwrap_or_default(),
        profile,
        seed_project,
    ))
}

fn parse(
    method: &Method,
    bucket: Option<String>,
    raw: &str,
    profile: CompatProfile,
    seed_project: &str,
) -> Result<BucketRequest, JsonError> {
    let mut query = parse_query(raw)?;
    if method == Method::DELETE {
        query.remove("fields");
    } else {
        super::Fields::take(
            &mut query,
            if bucket.is_none() && method == Method::GET {
                "Buckets"
            } else {
                "Bucket"
            },
        )?;
    }
    common(&mut query)?;
    match (method, bucket) {
        (&Method::POST, None) => {
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
            finish_bucket(&query, profile)?;
            Ok(BucketRequest::Insert(InsertParams {
                project,
                projection,
                enable_object_retention,
            }))
        }
        (&Method::GET, None) => parse_bucket_list(&mut query, profile, seed_project),
        (&Method::GET | &Method::PATCH | &Method::PUT, Some(bucket)) => {
            let predefined = query.contains_key("predefinedAcl")
                || query.contains_key("predefinedDefaultObjectAcl");
            // Recorded 2026-10-31: native PATCH without projection returned acl, defaultObjectAcl
            // and owner; PUT did so only with predefined ACL parameters. Insert/get default to
            // noAcl. The fake-gcs-server profile retains its earlier PUT-only full default.
            let default_projection = if (profile == CompatProfile::FakeGcsServer
                && *method == Method::PUT)
                || (profile == CompatProfile::Native
                    && (*method == Method::PATCH || (*method == Method::PUT && predefined)))
            {
                Projection::Full
            } else {
                Projection::NoAcl
            };
            if *method == Method::GET {
                if let Some(request) = soft_deleted_get(&mut query, &bucket)? {
                    return Ok(request);
                }
            } else {
                query.remove("predefinedAcl");
                query.remove("predefinedDefaultObjectAcl");
            }
            let conditions = conditions(&mut query)?;
            let projection = projection_or(&mut query, default_projection)?;
            finish_bucket(&query, profile)?;
            let params = GetParams {
                conditions,
                projection,
            };
            Ok(if *method == Method::PATCH {
                BucketRequest::Patch { bucket, params }
            } else if *method == Method::PUT {
                BucketRequest::Update { bucket, params }
            } else {
                BucketRequest::Get { bucket, params }
            })
        }
        (&Method::DELETE, Some(bucket)) => {
            let conditions = conditions(&mut query)?;
            finish_bucket(&query, profile)?;
            Ok(BucketRequest::Delete { bucket, conditions })
        }
        _ => unreachable!("route validates supported methods"),
    }
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
    if prefix.len() > 222 {
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
    finish_bucket(query, profile)?;
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
/// report success for a query token, quota attribution or JSONP that never happened.
const UNSUPPORTED_STANDARD_PARAMETERS: &[&str] =
    &["oauth_token", "quotaUser", "userIp", "callback"];

/// Standard parameters whose value can change neither the caller nor the result here, so they
/// are discarded unchecked:
///
/// - `key`: Google uses an API key for quota attribution, never as a credential, and anonymous
///   gsutil sends one on every request.
/// - `userProject`: it names the project billed for a Requester Pays bucket. A gcs-mimic bucket
///   cannot enable Requester Pays, and Cloud Storage served the parameter on a bucket and a
///   project without it as if it were absent (cases bkt-edges-hmac-keys and
///   bkt-edges-notification-ids).
const IGNORED_STANDARD_PARAMETERS: &[&str] = &["key", "userProject"];

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
    let invalid =
        || JsonError::invalid_parameter("maxResults must be a uint32").at_parameter("maxResults");
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let size = value.parse::<u32>().map_err(|_| invalid())?;
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
    let mut integer = |key| {
        query
            .remove(key)
            .map(|value| {
                if value.starts_with('+') {
                    return Err(
                        JsonError::invalid_parameter(format!("{key} must be an int64"))
                            .at_parameter(key),
                    );
                }
                value.parse::<i64>().map_err(|_| {
                    JsonError::invalid_parameter(format!("{key} must be an int64"))
                        .at_parameter(key)
                })
            })
            .transpose()
    };
    Ok(BucketConditions {
        if_metageneration_match: integer("ifMetagenerationMatch")?,
        if_metageneration_not_match: integer("ifMetagenerationNotMatch")?,
    })
}

/// Bucket reads and writes ignore a query parameter they do not define, as Cloud Storage does for
/// `append=false` on insert and PATCH (recorded 2026-10-31). The fake-gcs-server profile rejects it.
fn finish_bucket(
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
