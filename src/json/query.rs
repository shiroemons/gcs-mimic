use super::buckets::validate_bucket_name_for_profile;
use super::{JsonError, decode_page_token, page_token::hex};
use crate::config::CompatProfile;
use hyper::{Method, StatusCode, Uri};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Projection {
    #[default]
    NoAcl,
    Full,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Conditions {
    pub if_metageneration_match: Option<i64>,
    pub if_metageneration_not_match: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InsertParams {
    pub project: String,
    pub projection: Projection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GetParams {
    pub conditions: Conditions,
    pub projection: Projection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListParams {
    pub project: String,
    pub projection: Projection,
    pub max_results: u32,
    pub prefix: String,
    /// Decoded last name, for a catalog keyset query; never a filesystem path.
    pub start_after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BucketRequest {
    Insert(InsertParams),
    Get {
        bucket: String,
        params: GetParams,
    },
    List(ListParams),
    Delete {
        bucket: String,
        conditions: Conditions,
    },
    GetIam {
        bucket: String,
    },
    SetIam {
        bucket: String,
    },
}

/// Match the raw URI before decoding any bucket path segment.
pub fn route(method: &Method, uri: &Uri) -> Option<Result<BucketRequest, JsonError>> {
    route_with_profile(method, uri, CompatProfile::Native, "local-project")
}

pub fn route_with_profile(
    method: &Method,
    uri: &Uri,
    profile: CompatProfile,
    seed_project: &str,
) -> Option<Result<BucketRequest, JsonError>> {
    const COLLECTION: &str = "/storage/v1/b";
    if let Some(segment) = uri
        .path()
        .strip_prefix("/storage/v1/b/")
        .and_then(|path| path.strip_suffix("/iam"))
        && !segment.is_empty()
        && !segment.contains('/')
    {
        return Some((|| {
            let bucket = decode(segment, false)?;
            if segment.len() > 222 * 3 {
                return Err(JsonError::invalid_parameter("Invalid bucket name"));
            }
            if profile == CompatProfile::Native {
                validate_bucket_name_for_profile(&bucket, profile)?;
            }
            if !matches!(*method, Method::GET | Method::PUT) {
                let mut error = JsonError::new(
                    StatusCode::METHOD_NOT_ALLOWED,
                    "methodNotAllowed",
                    "HTTP method is not supported for this IAM endpoint",
                );
                error.allow = Some("GET, PUT");
                return Err(error);
            }
            let mut query = parse_query(uri.query().unwrap_or_default())?;
            common(&mut query)?;
            if *method == Method::GET {
                if let Some(version) = query.remove("optionsRequestedPolicyVersion")
                    && !matches!(version.as_str(), "1" | "3")
                {
                    return Err(JsonError::invalid_parameter(
                        "optionsRequestedPolicyVersion must be 1 or 3",
                    ));
                }
                finish(query)?;
                Ok(BucketRequest::GetIam { bucket })
            } else {
                finish(query)?;
                Ok(BucketRequest::SetIam { bucket })
            }
        })());
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
        match decode(segment, false).and_then(|name| {
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
        (false, &Method::GET | &Method::POST) | (true, &Method::GET | &Method::DELETE)
    );
    if bucket.is_some() && matches!(*method, Method::PATCH | Method::PUT) {
        return Some(Err(JsonError::unsupported(
            "bucket metadata updates, including iamConfiguration",
        )));
    }
    if !allowed {
        let mut error = JsonError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "methodNotAllowed",
            "HTTP method is not supported for this bucket endpoint",
        );
        error.allow = Some(if bucket.is_some() {
            "GET, DELETE"
        } else {
            "GET, POST"
        });
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
    common(&mut query)?;
    match (method, bucket) {
        (&Method::POST, None) => {
            unsupported(
                &query,
                &[
                    "predefinedAcl",
                    "predefinedDefaultObjectAcl",
                    "enableObjectRetention",
                ],
            )?;
            if profile == CompatProfile::LegacyEmulator {
                query
                    .entry("project".into())
                    .or_insert_with(|| seed_project.into());
            }
            let project = project(&mut query)?;
            let projection = projection(&mut query)?;
            finish(query)?;
            Ok(BucketRequest::Insert(InsertParams {
                project,
                projection,
            }))
        }
        (&Method::GET, None) => {
            false_only(&mut query, "softDeleted")?;
            false_only(&mut query, "returnPartialSuccess")?;
            let project = project(&mut query)?;
            let projection = projection(&mut query)?;
            let page_cap = if projection == Projection::Full {
                200
            } else {
                1000
            };
            let max_results = match query.remove("maxResults") {
                None => page_cap,
                Some(value) => {
                    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(JsonError::invalid_parameter("maxResults must be a uint32"));
                    }
                    let size = value
                        .parse::<u32>()
                        .map_err(|_| JsonError::invalid_parameter("maxResults must be a uint32"))?;
                    if size == 0 {
                        page_cap
                    } else {
                        size.min(page_cap)
                    }
                }
            };
            let prefix = query.remove("prefix").unwrap_or_default();
            if prefix.len() > 222 {
                return Err(JsonError::invalid_parameter("prefix exceeds 222 bytes"));
            }
            let start_after = query
                .remove("pageToken")
                .filter(|token| !token.is_empty())
                .map(|token| decode_page_token(&token, &project, &prefix))
                .transpose()?;
            finish(query)?;
            Ok(BucketRequest::List(ListParams {
                project,
                projection,
                max_results,
                prefix,
                start_after,
            }))
        }
        (&Method::GET, Some(bucket)) => {
            unsupported(&query, &["generation"])?;
            false_only(&mut query, "softDeleted")?;
            let conditions = conditions(&mut query)?;
            let projection = projection(&mut query)?;
            finish(query)?;
            Ok(BucketRequest::Get {
                bucket,
                params: GetParams {
                    conditions,
                    projection,
                },
            })
        }
        (&Method::DELETE, Some(bucket)) => {
            let conditions = conditions(&mut query)?;
            finish(query)?;
            Ok(BucketRequest::Delete { bucket, conditions })
        }
        _ => unreachable!("route validates supported methods"),
    }
}

pub(crate) fn parse_query(raw: &str) -> Result<BTreeMap<String, String>, JsonError> {
    if raw.len() > 16 * 1024 {
        return Err(JsonError::invalid_parameter("Query exceeds 16 KiB"));
    }
    let mut query = BTreeMap::new();
    for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode(key, true)?;
        let value = decode(value, true)?;
        if query.insert(key.clone(), value).is_some() {
            return Err(JsonError::invalid_parameter(format!(
                "Duplicate query parameter: {key}"
            )));
        }
    }
    Ok(query)
}

fn common(query: &mut BTreeMap<String, String>) -> Result<(), JsonError> {
    unsupported(
        query,
        &[
            "fields",
            "key",
            "oauth_token",
            "quotaUser",
            "userIp",
            "uploadType",
            "userProject",
            "callback",
        ],
    )?;
    false_only(query, "prettyPrint")?;
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

fn project(query: &mut BTreeMap<String, String>) -> Result<String, JsonError> {
    let project = query.remove("project").ok_or_else(|| {
        JsonError::new(
            StatusCode::BAD_REQUEST,
            "required",
            "Missing required project",
        )
    })?;
    validate_project(&project)?;
    Ok(project)
}

pub fn validate_project(project: &str) -> Result<(), JsonError> {
    if project.is_empty()
        || project.len() > 256
        || project
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(JsonError::invalid_parameter(
            "project must be a nonempty identifier of at most 256 bytes",
        ));
    }
    Ok(())
}

pub(super) fn projection(query: &mut BTreeMap<String, String>) -> Result<Projection, JsonError> {
    match query.remove("projection").as_deref() {
        None | Some("noAcl") => Ok(Projection::NoAcl),
        Some("full") => Ok(Projection::Full),
        Some(_) => Err(JsonError::invalid_parameter(
            "projection must be noAcl or full",
        )),
    }
}

fn false_only(query: &mut BTreeMap<String, String>, key: &str) -> Result<(), JsonError> {
    match query.remove(key).as_deref() {
        None | Some("false") => Ok(()),
        Some("true") => Err(JsonError::unsupported(key)),
        Some(_) => Err(JsonError::invalid_parameter(format!(
            "{key} must be true or false"
        ))),
    }
}

fn conditions(query: &mut BTreeMap<String, String>) -> Result<Conditions, JsonError> {
    let mut integer = |key| {
        query
            .remove(key)
            .map(|value| {
                if value.starts_with('+') {
                    return Err(JsonError::invalid_parameter(format!(
                        "{key} must be an int64"
                    )));
                }
                value
                    .parse::<i64>()
                    .map_err(|_| JsonError::invalid_parameter(format!("{key} must be an int64")))
            })
            .transpose()
    };
    Ok(Conditions {
        if_metageneration_match: integer("ifMetagenerationMatch")?,
        if_metageneration_not_match: integer("ifMetagenerationNotMatch")?,
    })
}

pub(super) fn finish(query: BTreeMap<String, String>) -> Result<(), JsonError> {
    match query.first_key_value() {
        Some((key, _)) => Err(JsonError::invalid_parameter(format!(
            "Unknown query parameter for this method: {key}"
        ))),
        None => Ok(()),
    }
}

pub(crate) fn decode(raw: &str, form: bool) -> Result<String, JsonError> {
    let invalid = || JsonError::invalid_parameter("Malformed URI encoding");
    let mut bytes = Vec::with_capacity(raw.len());
    let mut input = raw.bytes();
    while let Some(byte) = input.next() {
        bytes.push(match byte {
            b'%' => {
                (hex(input.next().ok_or_else(invalid)?).ok_or_else(invalid)? << 4)
                    | hex(input.next().ok_or_else(invalid)?).ok_or_else(invalid)?
            }
            b'+' if form => b' ',
            byte => byte,
        });
    }
    String::from_utf8(bytes).map_err(|_| invalid())
}
