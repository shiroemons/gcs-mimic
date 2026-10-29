//! Project-scoped JSON resources: `projects.hmacKeys` and `projects.serviceAccount`.
//!
//! Routing, query validation, body parsing and rendering only; the caller authorizes the
//! project and performs catalog operations. The supported subset is recorded in
//! `docs/en/slices/project-hmac-keys.md`.
use super::buckets::{encode_segment, timestamp};
use super::query::{Decoding, common, decode, finish, parse_query, validate_project};
use super::{Fields, JsonError};
use crate::storage::hmac_keys::{HmacKeyError, HmacKeyRecord, HmacKeyState};
use hyper::{Method, StatusCode, Uri};
use serde_json::{Value, json};

/// The documented list has no default page size; this bound keeps one page small and matches
/// the bucket listing cap.
pub const HMAC_KEY_PAGE_CAP: u32 = 1000;
const MAX_EMAIL_BYTES: usize = 254;
const MAX_TOKEN_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HmacKeyListParams {
    pub project: String,
    pub service_account_email: Option<String>,
    pub show_deleted_keys: bool,
    pub max_results: u32,
    /// Last access ID of the previous page, decoded from a token bound to these filters.
    pub start_after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectRequest {
    CreateHmacKey {
        project: String,
        service_account_email: String,
    },
    GetHmacKey {
        project: String,
        access_id: String,
    },
    ListHmacKeys(HmacKeyListParams),
    UpdateHmacKey {
        project: String,
        access_id: String,
    },
    DeleteHmacKey {
        project: String,
        access_id: String,
    },
    GetServiceAccount {
        project: String,
    },
}

/// The writable subset of an `HmacKeyMetadata` update body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HmacKeyUpdate {
    pub state: HmacKeyState,
    pub etag: Option<String>,
}

/// Match `/storage/v1/projects/{projectId}/...` before decoding the path segments.
pub fn route_project(
    method: &Method,
    uri: &Uri,
) -> Option<Result<(ProjectRequest, Fields), JsonError>> {
    let rest = uri.path().strip_prefix("/storage/v1/projects/")?;
    let mut segments = rest.split('/');
    let project = segments.next().filter(|segment| !segment.is_empty())?;
    let collection = segments.next()?;
    let item = segments.next();
    if segments.next().is_some() || item.is_some_and(str::is_empty) {
        return None;
    }
    let (allowed, kind) = match (collection, item) {
        ("hmacKeys", None) => ("GET, POST", Kind::HmacKeys),
        ("hmacKeys", Some(_)) => ("GET, DELETE, PUT", Kind::HmacKey),
        ("serviceAccount", None) => ("GET", Kind::ServiceAccount),
        _ => return None,
    };
    if !allowed.split(", ").any(|name| name == method.as_str()) {
        let mut error = JsonError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "methodNotAllowed",
            "HTTP method is not supported for this project endpoint",
        );
        error.allow = Some(allowed);
        return Some(Err(error));
    }
    Some(parse(
        method,
        kind,
        project,
        item,
        uri.query().unwrap_or_default(),
    ))
}

#[derive(Clone, Copy)]
enum Kind {
    HmacKeys,
    HmacKey,
    ServiceAccount,
}

fn parse(
    method: &Method,
    kind: Kind,
    project: &str,
    item: Option<&str>,
    raw: &str,
) -> Result<(ProjectRequest, Fields), JsonError> {
    let project = decode(project, Decoding::Path)?;
    validate_project(&project)?;
    let access_id = item
        .map(|segment| {
            let access_id = decode(segment, Decoding::Path)?;
            if access_id.len() > 256 || access_id.chars().any(char::is_control) {
                return Err(JsonError::invalid_parameter(
                    "accessId must be an HMAC key access ID",
                ));
            }
            Ok(access_id)
        })
        .transpose()?;
    let mut query = parse_query(raw)?;
    let fields = if *method == Method::DELETE {
        query.remove("fields");
        Fields::from_query(None, "HmacKeyMetadata")?
    } else {
        let schema = match (kind, method) {
            (Kind::HmacKeys, &Method::POST) => "HmacKey",
            (Kind::HmacKeys, _) => "HmacKeysMetadata",
            (Kind::HmacKey, _) => "HmacKeyMetadata",
            (Kind::ServiceAccount, _) => "ServiceAccount",
        };
        Fields::take(&mut query, schema)?
    };
    common(&mut query)?;
    let request = match (kind, method, access_id) {
        (Kind::HmacKeys, &Method::POST, _) => {
            let email = query.remove("serviceAccountEmail").ok_or_else(|| {
                JsonError::new(
                    StatusCode::BAD_REQUEST,
                    "required",
                    "Missing required serviceAccountEmail; add ?serviceAccountEmail=SERVICE_ACCOUNT@PROJECT.iam.gserviceaccount.com",
                )
            })?;
            validate_service_account_email(&email)?;
            ProjectRequest::CreateHmacKey {
                project,
                service_account_email: email,
            }
        }
        (Kind::HmacKeys, _, _) => {
            let service_account_email = query.remove("serviceAccountEmail");
            if let Some(email) = &service_account_email {
                validate_service_account_email(email)?;
            }
            let show_deleted_keys = super::query::boolean(&mut query, "showDeletedKeys")?;
            let max_results = match query.remove("maxResults") {
                None => HMAC_KEY_PAGE_CAP,
                Some(value) => {
                    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(JsonError::invalid_parameter("maxResults must be a uint32"));
                    }
                    match value.parse::<u32>() {
                        Ok(0) => HMAC_KEY_PAGE_CAP,
                        Ok(size) => size.min(HMAC_KEY_PAGE_CAP),
                        Err(_) => {
                            return Err(JsonError::invalid_parameter(
                                "maxResults must be a uint32",
                            ));
                        }
                    }
                }
            };
            let mut params = HmacKeyListParams {
                project,
                service_account_email,
                show_deleted_keys,
                max_results,
                start_after: None,
            };
            params.start_after = query
                .remove("pageToken")
                .filter(|token| !token.is_empty())
                .map(|token| decode_list_token(&token, &params))
                .transpose()?;
            ProjectRequest::ListHmacKeys(params)
        }
        (Kind::HmacKey, method, Some(access_id)) => match *method {
            Method::GET => ProjectRequest::GetHmacKey { project, access_id },
            Method::PUT => ProjectRequest::UpdateHmacKey { project, access_id },
            _ => ProjectRequest::DeleteHmacKey { project, access_id },
        },
        (Kind::ServiceAccount, _, _) => ProjectRequest::GetServiceAccount { project },
        (Kind::HmacKey, _, None) => unreachable!("item routes carry an access ID"),
    };
    finish(query)?;
    Ok((request, fields))
}

/// Local keys can name any syntactically valid service account; no IAM directory exists.
fn validate_service_account_email(email: &str) -> Result<(), JsonError> {
    let valid = email.len() <= MAX_EMAIL_BYTES
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && !domain.is_empty() && !domain.contains('@')
        })
        && !email
            .chars()
            .any(|character| character.is_control() || character.is_whitespace());
    if valid {
        Ok(())
    } else {
        Err(JsonError::invalid_parameter(
            "serviceAccountEmail must be a service account email address such as SERVICE_ACCOUNT@PROJECT.iam.gserviceaccount.com",
        ))
    }
}

/// Parse a PUT body. Only `state` and the conditional `etag` are writable; output-only and
/// unrecognized fields are ignored like other metadata updates, so read-modify-write clients
/// can send back the whole resource.
pub fn parse_hmac_key_update(bytes: &[u8]) -> Result<HmacKeyUpdate, JsonError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        JsonError::new(
            StatusCode::BAD_REQUEST,
            "parseError",
            "HMAC key metadata must be valid JSON",
        )
    })?;
    let body = value
        .as_object()
        .ok_or_else(|| JsonError::invalid_argument("HMAC key metadata must be a JSON object"))?;
    let string = |field: &str| {
        body.get(field)
            .filter(|value| !value.is_null())
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| JsonError::invalid_argument(format!("{field} must be a string")))
            })
            .transpose()
    };
    let state = string("state")?.ok_or_else(|| {
        JsonError::new(
            StatusCode::BAD_REQUEST,
            "required",
            "Missing required state; set it to ACTIVE or INACTIVE",
        )
    })?;
    let state = match HmacKeyState::parse(&state) {
        Some(HmacKeyState::Deleted) => {
            return Err(JsonError::invalid_argument(
                "state can be updated only to ACTIVE or INACTIVE; use DELETE to delete the key",
            ));
        }
        Some(state) => state,
        None => {
            return Err(JsonError::invalid_argument(
                "state must be ACTIVE or INACTIVE",
            ));
        }
    };
    Ok(HmacKeyUpdate {
        state,
        etag: string("etag")?,
    })
}

pub fn hmac_key_metadata(record: &HmacKeyRecord, base_url: &str) -> Result<Value, JsonError> {
    Ok(json!({
        "kind": "storage#hmacKeyMetadata",
        "id": format!("{}/{}", record.project, record.access_id),
        "selfLink": format!(
            "{}/storage/v1/projects/{}/hmacKeys/{}",
            base_url.trim_end_matches('/'),
            encode_segment(&record.project),
            encode_segment(&record.access_id)
        ),
        "accessId": record.access_id,
        "projectId": record.project,
        "serviceAccountEmail": record.service_account_email,
        "state": record.state.as_str(),
        "timeCreated": timestamp(record.time_created)?,
        "updated": timestamp(record.updated)?,
        "etag": record.etag(),
    }))
}

pub fn hmac_key_resource(
    record: &HmacKeyRecord,
    secret: &str,
    base_url: &str,
) -> Result<Value, JsonError> {
    Ok(json!({
        "kind": "storage#hmacKey",
        "metadata": hmac_key_metadata(record, base_url)?,
        "secret": secret,
    }))
}

pub fn hmac_key_list(
    items: &[HmacKeyRecord],
    params: &HmacKeyListParams,
    has_more: bool,
    base_url: &str,
) -> Result<Value, JsonError> {
    let mut value = json!({
        "kind": "storage#hmacKeysMetadata",
        "items": items
            .iter()
            .map(|record| hmac_key_metadata(record, base_url))
            .collect::<Result<Vec<_>, _>>()?,
    });
    if has_more && let Some(last) = items.last() {
        value["nextPageToken"] = encode_list_token(params, &last.access_id).into();
    }
    Ok(value)
}

/// The project number the bucket resource reports for `project`: numeric project identifiers
/// are their own number; local named projects have no number and report 0.
fn project_number(project: &str) -> String {
    if project.bytes().all(|byte| byte.is_ascii_digit()) {
        project.parse::<u64>().unwrap_or(0).to_string()
    } else {
        "0".to_owned()
    }
}

/// The deterministic local Cloud Storage service agent, in Google's
/// `service-PROJECT_NUMBER@gs-project-accounts.iam.gserviceaccount.com` form.
pub fn service_account(project: &str) -> Value {
    json!({
        "kind": "storage#serviceAccount",
        "email_address": format!(
            "service-{}@gs-project-accounts.iam.gserviceaccount.com",
            project_number(project)
        ),
    })
}

impl From<HmacKeyError> for JsonError {
    fn from(error: HmacKeyError) -> Self {
        match error {
            HmacKeyError::NotFound => {
                Self::new(StatusCode::NOT_FOUND, "notFound", "HMAC key not found")
            }
            HmacKeyError::PreconditionFailed => Self::new(
                StatusCode::PRECONDITION_FAILED,
                "conditionNotMet",
                "HMAC key etag does not match; read the key again and retry with its current etag",
            ),
            HmacKeyError::DeleteActive => Self::new(
                StatusCode::BAD_REQUEST,
                "badRequest",
                "Cannot delete keys in 'ACTIVE' state; set the key state to INACTIVE first",
            ),
            HmacKeyError::Deleted => Self::new(
                StatusCode::BAD_REQUEST,
                "badRequest",
                "Cannot modify keys in 'DELETED' state; create a new key instead",
            ),
            HmacKeyError::LimitReached => Self::new(
                StatusCode::BAD_REQUEST,
                "badRequest",
                format!(
                    "A service account can have at most {} HMAC keys that are not DELETED; deactivate and delete an unused key first",
                    crate::storage::hmac_keys::MAX_KEYS_PER_SERVICE_ACCOUNT
                ),
            ),
            HmacKeyError::Catalog(error) => match error {
                crate::bucket::CatalogError::InvalidMetadata(reason) => {
                    Self::invalid_argument(reason)
                }
                _ => Self::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "HMAC key catalog is unavailable",
                ),
            },
        }
    }
}

fn token_scope(params: &HmacKeyListParams) -> Value {
    json!([
        "h1",
        params.project,
        params.service_account_email,
        params.show_deleted_keys
    ])
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

/// Tokens detect corruption and reuse with other filters; they are not credentials.
fn encode_list_token(params: &HmacKeyListParams, last_access_id: &str) -> String {
    let payload = json!([token_scope(params), last_access_id]).to_string();
    format!(
        "{}.{:016x}",
        crate::hashing::encode_lower(payload.as_bytes()),
        checksum(payload.as_bytes())
    )
}

fn decode_list_token(token: &str, params: &HmacKeyListParams) -> Result<String, JsonError> {
    let invalid = || JsonError::invalid_parameter("Invalid pageToken for this HMAC key listing");
    if token.len() > MAX_TOKEN_BYTES {
        return Err(invalid());
    }
    let (encoded, digest) = token.split_once('.').ok_or_else(invalid)?;
    if encoded.len() % 2 != 0 || digest.len() != 16 || !encoded.is_ascii() {
        return Err(invalid());
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().as_chunks::<2>().0 {
        let digit = |byte| super::page_token::hex(byte).ok_or_else(invalid);
        bytes.push((digit(pair[0])? << 4) | digit(pair[1])?);
    }
    if u64::from_str_radix(digest, 16).map_err(|_| invalid())? != checksum(&bytes) {
        return Err(invalid());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    match value.as_array().map(Vec::as_slice) {
        Some([scope, Value::String(last)]) if *scope == token_scope(params) => Ok(last.clone()),
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(method: Method, path: &str) -> Result<ProjectRequest, JsonError> {
        route_project(&method, &path.parse().unwrap())
            .expect("project route")
            .map(|(request, _)| request)
    }

    #[test]
    fn routes_hmac_key_and_service_account_methods() {
        assert_eq!(
            route(
                Method::POST,
                "/storage/v1/projects/p/hmacKeys?serviceAccountEmail=sa%40example.invalid"
            )
            .unwrap(),
            ProjectRequest::CreateHmacKey {
                project: "p".into(),
                service_account_email: "sa@example.invalid".into()
            }
        );
        assert_eq!(
            route(Method::GET, "/storage/v1/projects/p/hmacKeys/GOOG1EX").unwrap(),
            ProjectRequest::GetHmacKey {
                project: "p".into(),
                access_id: "GOOG1EX".into()
            }
        );
        assert!(matches!(
            route(Method::PUT, "/storage/v1/projects/p/hmacKeys/A").unwrap(),
            ProjectRequest::UpdateHmacKey { .. }
        ));
        assert!(matches!(
            route(Method::DELETE, "/storage/v1/projects/p/hmacKeys/A?fields=x").unwrap(),
            ProjectRequest::DeleteHmacKey { .. }
        ));
        assert_eq!(
            route(Method::GET, "/storage/v1/projects/123/serviceAccount").unwrap(),
            ProjectRequest::GetServiceAccount {
                project: "123".into()
            }
        );
        let ProjectRequest::ListHmacKeys(params) = route(
            Method::GET,
            "/storage/v1/projects/p/hmacKeys?maxResults=5000&showDeletedKeys=true&serviceAccountEmail=a%40b",
        )
        .unwrap() else {
            panic!("list route");
        };
        assert_eq!(params.max_results, HMAC_KEY_PAGE_CAP);
        assert!(params.show_deleted_keys);
        assert_eq!(params.service_account_email.as_deref(), Some("a@b"));
        for path in [
            "/storage/v1/projects/p",
            "/storage/v1/projects/p/other",
            "/storage/v1/projects/p/hmacKeys/a/b",
            "/storage/v1/projects//hmacKeys",
            "/storage/v1/projects/p/hmacKeys/",
        ] {
            assert!(
                route_project(&Method::GET, &path.parse().unwrap()).is_none(),
                "{path}"
            );
        }
    }

    #[test]
    fn rejects_invalid_methods_and_queries() {
        let error = route(Method::PATCH, "/storage/v1/projects/p/hmacKeys/a").unwrap_err();
        assert_eq!(error.status, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(error.allow, Some("GET, DELETE, PUT"));
        let error = route(Method::POST, "/storage/v1/projects/p/serviceAccount").unwrap_err();
        assert_eq!(error.allow, Some("GET"));
        let error = route(Method::POST, "/storage/v1/projects/p/hmacKeys").unwrap_err();
        assert_eq!(error.reason, "required");
        for path in [
            "/storage/v1/projects/p/hmacKeys?serviceAccountEmail=no-at",
            "/storage/v1/projects/p/hmacKeys?serviceAccountEmail=a%40b%40c",
            "/storage/v1/projects/p/hmacKeys?serviceAccountEmail=%40b",
        ] {
            assert_eq!(
                route(Method::POST, path).unwrap_err().reason,
                "invalidParameter",
                "{path}"
            );
        }
        for query in [
            "maxResults=-1",
            "maxResults=4294967296",
            "showDeletedKeys=yes",
            "pageToken=bogus",
            "unknown=1",
        ] {
            assert_eq!(
                route(
                    Method::GET,
                    &format!("/storage/v1/projects/p/hmacKeys?{query}")
                )
                .unwrap_err()
                .reason,
                "invalidParameter",
                "{query}"
            );
        }
        assert_eq!(
            route(
                Method::GET,
                "/storage/v1/projects/p/serviceAccount?userProject=p"
            )
            .unwrap_err()
            .reason,
            "badRequest"
        );
        assert!(
            route_project(
                &Method::GET,
                &"/storage/v1/projects/p/hmacKeys?fields=items(bogus)"
                    .parse()
                    .unwrap()
            )
            .unwrap()
            .is_err()
        );
    }

    #[test]
    fn page_tokens_are_bound_to_the_listing_filters() {
        let params = HmacKeyListParams {
            project: "p".into(),
            service_account_email: Some("a@b".into()),
            show_deleted_keys: false,
            max_results: 1,
            start_after: None,
        };
        let token = encode_list_token(&params, "GOOG1EA");
        assert_eq!(decode_list_token(&token, &params).unwrap(), "GOOG1EA");
        for other in [
            HmacKeyListParams {
                project: "q".into(),
                ..params.clone()
            },
            HmacKeyListParams {
                service_account_email: None,
                ..params.clone()
            },
            HmacKeyListParams {
                show_deleted_keys: true,
                ..params.clone()
            },
        ] {
            assert!(decode_list_token(&token, &other).is_err());
        }
        let mut corrupted = token.clone();
        corrupted.replace_range(0..2, "00");
        assert!(decode_list_token(&corrupted, &params).is_err());
    }

    #[test]
    fn update_body_accepts_round_tripped_metadata_and_rejects_invalid_states() {
        let update = parse_hmac_key_update(
            br#"{"kind":"storage#hmacKeyMetadata","accessId":"A","state":"INACTIVE","etag":"e","timeCreated":"x","unknown":1}"#,
        )
        .unwrap();
        assert_eq!(update.state, HmacKeyState::Inactive);
        assert_eq!(update.etag.as_deref(), Some("e"));
        assert_eq!(
            parse_hmac_key_update(br#"{"state":"ACTIVE"}"#).unwrap(),
            HmacKeyUpdate {
                state: HmacKeyState::Active,
                etag: None
            }
        );
        for (body, reason) in [
            (&br#"{"state":"DELETED"}"#[..], "invalidArgument"),
            (br#"{"state":"active"}"#, "invalidArgument"),
            (br#"{"state":1}"#, "invalidArgument"),
            (br#"{"state":"ACTIVE","etag":1}"#, "invalidArgument"),
            (br#"{}"#, "required"),
            (br#"[]"#, "invalidArgument"),
            (br#"{"#, "parseError"),
        ] {
            assert_eq!(
                parse_hmac_key_update(body).unwrap_err().reason,
                reason,
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn service_account_uses_the_reported_project_number() {
        assert_eq!(
            service_account("123")["email_address"],
            "service-123@gs-project-accounts.iam.gserviceaccount.com"
        );
        assert_eq!(
            service_account("local-project")["email_address"],
            "service-0@gs-project-accounts.iam.gserviceaccount.com"
        );
        assert_eq!(service_account("p")["kind"], "storage#serviceAccount");
    }
}
