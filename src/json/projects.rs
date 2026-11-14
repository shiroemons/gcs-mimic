//! Project-scoped JSON resources: `projects.hmacKeys` and `projects.serviceAccount`.
//!
//! Routing, query validation, body parsing and rendering only; the caller authorizes the
//! project and performs catalog operations. The supported subset is recorded in
//! `docs/en/slices/project-hmac-keys.md`.
use super::buckets::{encode_segment, timestamp};
use super::page_token::{self, CursorTag};
use super::query::{Decoding, common, decode, finish, parse_query};
use super::{Fields, JsonError, Schema};
use crate::auth::ProjectId;
use crate::storage::Page;
use crate::storage::hmac_keys::{HmacKeyError, HmacKeyRecord, HmacKeyState};
use hyper::{Method, StatusCode, Uri};
use serde_json::{Map, Value, json};

/// The documented list has no default page size; this bound keeps one page small and matches
/// the bucket listing cap.
const HMAC_KEY_PAGE_CAP: u32 = 1000;
const MAX_EMAIL_BYTES: usize = 254;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HmacKeyListParams {
    pub project: ProjectId,
    pub service_account_email: Option<String>,
    pub show_deleted_keys: bool,
    pub max_results: u32,
    /// Last access ID of the previous page, decoded from a token bound to these filters.
    pub start_after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectRequest {
    CreateHmacKey {
        project: ProjectId,
        service_account_email: String,
    },
    GetHmacKey {
        project: ProjectId,
        access_id: String,
    },
    ListHmacKeys(HmacKeyListParams),
    UpdateHmacKey {
        project: ProjectId,
        access_id: String,
    },
    DeleteHmacKey {
        project: ProjectId,
        access_id: String,
    },
    GetServiceAccount {
        project: ProjectId,
    },
}

/// The writable subset of an `HmacKeyMetadata` update body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HmacKeyUpdate {
    pub state: HmacKeyState,
    pub etag: Option<String>,
}

/// Match `/storage/v1/projects/{projectId}/...` before decoding the path segments.
pub(crate) fn route_project(
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
    let call = ProjectCall::select(collection, item, method)?;
    Some(call.and_then(|call| parse(call, project, uri.query().unwrap_or_default())))
}

/// A project method, chosen once from the path and the HTTP method. `A` is an HMAC key's
/// access ID: the raw path segment until the path is decoded, then the decoded ID.
enum ProjectCall<A> {
    CreateHmacKey,
    ListHmacKeys,
    HmacKey { method: HmacKeyMethod, access_id: A },
    ServiceAccount,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HmacKeyMethod {
    Get,
    Update,
    Delete,
}

impl<'a> ProjectCall<&'a str> {
    /// `None` for a path this router does not serve; a served path with another method is 405.
    fn select(
        collection: &str,
        item: Option<&'a str>,
        method: &Method,
    ) -> Option<Result<Self, JsonError>> {
        let not_allowed = |allow| {
            Err(JsonError::method_not_allowed(
                "HTTP method is not supported for this project endpoint",
                Some(allow),
            ))
        };
        let hmac_key = |method, access_id| Ok(Self::HmacKey { method, access_id });
        Some(match (collection, item) {
            ("hmacKeys", None) => match *method {
                Method::GET => Ok(Self::ListHmacKeys),
                Method::POST => Ok(Self::CreateHmacKey),
                _ => not_allowed("GET, POST"),
            },
            ("hmacKeys", Some(access_id)) => match *method {
                Method::GET => hmac_key(HmacKeyMethod::Get, access_id),
                Method::DELETE => hmac_key(HmacKeyMethod::Delete, access_id),
                Method::PUT => hmac_key(HmacKeyMethod::Update, access_id),
                _ => not_allowed("GET, DELETE, PUT"),
            },
            ("serviceAccount", None) if *method == Method::GET => Ok(Self::ServiceAccount),
            ("serviceAccount", None) => not_allowed("GET"),
            _ => return None,
        })
    }

    /// Decodes and checks the access ID an HMAC key path names.
    fn decode_access_id(self) -> Result<ProjectCall<String>, JsonError> {
        Ok(match self {
            Self::CreateHmacKey => ProjectCall::CreateHmacKey,
            Self::ListHmacKeys => ProjectCall::ListHmacKeys,
            Self::HmacKey { method, access_id } => {
                let access_id = decode(access_id, Decoding::Path)?;
                if access_id.len() > 256 || access_id.chars().any(char::is_control) {
                    return Err(JsonError::invalid_parameter(
                        "accessId must be an HMAC key access ID",
                    ));
                }
                ProjectCall::HmacKey { method, access_id }
            }
            Self::ServiceAccount => ProjectCall::ServiceAccount,
        })
    }
}

impl<A> ProjectCall<A> {
    fn schema(&self) -> Schema {
        match self {
            Self::CreateHmacKey => Schema::HmacKey,
            Self::ListHmacKeys => Schema::HmacKeysMetadata,
            Self::HmacKey { .. } => Schema::HmacKeyMetadata,
            Self::ServiceAccount => Schema::ServiceAccount,
        }
    }
}

/// `project` is the raw path segment the route matched.
fn parse(
    call: ProjectCall<&str>,
    project: &str,
    raw: &str,
) -> Result<(ProjectRequest, Fields), JsonError> {
    let project = ProjectId::try_from(decode(project, Decoding::Path)?)?;
    let call = call.decode_access_id()?;
    let mut query = parse_query(raw)?;
    let fields = if matches!(
        call,
        ProjectCall::HmacKey {
            method: HmacKeyMethod::Delete,
            ..
        }
    ) {
        query.remove("fields");
        Fields::whole()
    } else {
        Fields::take(&mut query, call.schema())?
    };
    common(&mut query)?;
    let request = match call {
        ProjectCall::CreateHmacKey => {
            let email = query.remove("serviceAccountEmail").ok_or_else(|| {
                JsonError::required("Required parameter: serviceAccountEmail")
                    .at_parameter("serviceAccountEmail")
            })?;
            validate_service_account_email(&email)?;
            ProjectRequest::CreateHmacKey {
                project,
                service_account_email: email,
            }
        }
        ProjectCall::ListHmacKeys => {
            let service_account_email = query.remove("serviceAccountEmail");
            if let Some(email) = &service_account_email {
                validate_service_account_email(email)?;
            }
            let show_deleted_keys = super::query::boolean(&mut query, "showDeletedKeys")?;
            // Cloud Storage's wording for maxResults=-1 (case bkt-edges-hmac-keys).
            let max_results = match super::integer::take_max_results(&mut query)? {
                None | Some(0) => HMAC_KEY_PAGE_CAP,
                Some(size) => size.min(HMAC_KEY_PAGE_CAP),
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
        ProjectCall::HmacKey { method, access_id } => match method {
            HmacKeyMethod::Get => ProjectRequest::GetHmacKey { project, access_id },
            HmacKeyMethod::Update => ProjectRequest::UpdateHmacKey { project, access_id },
            HmacKeyMethod::Delete => ProjectRequest::DeleteHmacKey { project, access_id },
        },
        ProjectCall::ServiceAccount => ProjectRequest::GetServiceAccount { project },
    };
    finish(&query)?;
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
pub(crate) fn parse_hmac_key_update(bytes: &[u8]) -> Result<HmacKeyUpdate, JsonError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| JsonError::legacy_parse_error("HMAC key metadata must be valid JSON"))?;
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
    // Cloud Storage's reasons and wording for a missing state and for DELETED, which it reports
    // before looking the key up (case bkt-edges-hmac-keys).
    let state =
        string("state")?.ok_or_else(|| JsonError::invalid("Must specify resource.state."))?;
    let state = match HmacKeyState::parse(&state) {
        Some(HmacKeyState::Deleted) => {
            return Err(JsonError::invalid("Cannot set state to 'DELETED'."));
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

pub(crate) fn hmac_key_metadata(
    record: &HmacKeyRecord,
    base_url: &str,
) -> Result<Value, JsonError> {
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

pub(crate) fn hmac_key_resource(
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

pub(crate) fn hmac_key_list(
    page: &Page<HmacKeyRecord>,
    params: &HmacKeyListParams,
    base_url: &str,
) -> Result<Value, JsonError> {
    let items = &page.items;
    let mut value = Map::from_iter([("kind".into(), "storage#hmacKeysMetadata".into())]);
    // Cloud Storage omits an empty `items` (case bkt-edges-hmac-keys).
    if !items.is_empty() {
        let metadata = items
            .iter()
            .map(|record| hmac_key_metadata(record, base_url))
            .collect::<Result<Vec<_>, _>>()?;
        value.insert("items".into(), metadata.into());
    }
    if page.has_more
        && let Some(last) = items.last()
    {
        value.insert(
            "nextPageToken".into(),
            encode_list_token(params, &last.access_id).into(),
        );
    }
    Ok(Value::Object(value))
}

/// The deterministic local Cloud Storage service agent, in Google's
/// `service-PROJECT_NUMBER@gs-project-accounts.iam.gserviceaccount.com` form.
pub(crate) fn service_account(project: &str) -> Value {
    json!({
        "kind": "storage#serviceAccount",
        "email_address": service_account_email(project),
    })
}

pub(crate) fn service_account_email(project: &str) -> String {
    format!(
        "service-{}@gs-project-accounts.iam.gserviceaccount.com",
        crate::acl::project_number(project)
    )
}

impl From<HmacKeyError> for JsonError {
    fn from(error: HmacKeyError) -> Self {
        match error {
            HmacKeyError::NotFound => Self::new(
                StatusCode::NOT_FOUND,
                "notFound",
                "Access ID not found in project.",
            ),
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
            HmacKeyError::Catalog(error) => error.into(),
        }
    }
}

/// Tokens detect corruption and reuse with other filters; they are not credentials. The tag
/// leads the nested filter tuple, followed by the last access ID.
fn encode_list_token(params: &HmacKeyListParams, last_access_id: &str) -> String {
    page_token::seal(&(
        (
            CursorTag::HmacKeys.as_str(),
            params.project.as_str(),
            &params.service_account_email,
            params.show_deleted_keys,
        ),
        last_access_id,
    ))
}

fn decode_list_token(token: &str, params: &HmacKeyListParams) -> Result<String, JsonError> {
    let ((tag, project, email, show_deleted_keys), last): (
        (String, String, Option<String>, bool),
        String,
    ) = page_token::open(token, page_token::HMAC_TOKEN_MAX_BYTES).ok_or_else(|| {
        JsonError::invalid_parameter("Invalid pageToken for this HMAC key listing")
    })?;
    if tag != CursorTag::HmacKeys.as_str()
        || project != params.project.as_str()
        || email != params.service_account_email
        || show_deleted_keys != params.show_deleted_keys
    {
        return Err(JsonError::invalid_parameter(
            "Invalid pageToken for this HMAC key listing",
        ));
    }
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_catalog_errors_keep_capacity_guidance_and_catalog_statuses() {
        use crate::bucket::CatalogError;
        use crate::storage::failure::BackendFailure;

        for (cause, status, reason, message) in [
            (
                CatalogError::Io(std::io::ErrorKind::StorageFull.into()),
                StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                "Storage capacity exhausted;",
            ),
            (
                CatalogError::Unavailable(BackendFailure::CatalogClosed.into()),
                StatusCode::SERVICE_UNAVAILABLE,
                "backendError",
                "Bucket catalog is unavailable: catalog is closed;",
            ),
            (
                CatalogError::InvalidMetadata("invalid metadata".into()),
                StatusCode::BAD_REQUEST,
                "invalid",
                "invalid metadata",
            ),
            (
                CatalogError::NotFound,
                StatusCode::NOT_FOUND,
                "notFound",
                super::super::error::BUCKET_NOT_FOUND,
            ),
            (
                CatalogError::Forbidden,
                StatusCode::FORBIDDEN,
                "forbidden",
                "Forbidden",
            ),
        ] {
            let error = JsonError::from(HmacKeyError::Catalog(cause));
            assert_eq!(error.status, status);
            assert_eq!(error.reason, reason);
            assert!(error.message.starts_with(message), "{error:?}");
        }
    }

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
                project: "p".parse().unwrap(),
                service_account_email: "sa@example.invalid".into()
            }
        );
        assert_eq!(
            route(Method::GET, "/storage/v1/projects/p/hmacKeys/GOOG1EX").unwrap(),
            ProjectRequest::GetHmacKey {
                project: "p".parse().unwrap(),
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
                project: "123".parse().unwrap()
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

    /// Each project path names its own methods in `Allow`, and refuses another method before it
    /// decodes the project or the access ID.
    #[test]
    fn every_project_path_lists_its_methods_before_parsing() {
        for (path, refused, allow) in [
            (
                "hmacKeys",
                [Method::PUT, Method::DELETE, Method::HEAD],
                "GET, POST",
            ),
            (
                "hmacKeys/A%ZZ",
                [Method::POST, Method::PATCH, Method::HEAD],
                "GET, DELETE, PUT",
            ),
            (
                "serviceAccount",
                [Method::POST, Method::DELETE, Method::HEAD],
                "GET",
            ),
        ] {
            for method in refused {
                // `%ZZ` is not percent-encoding, so parsing would reject the project first.
                let uri = format!("/storage/v1/projects/p%ZZ/{path}?unknown=1");
                let error = route_project(&method, &uri.parse().unwrap())
                    .unwrap()
                    .unwrap_err();
                assert_eq!(
                    error.status,
                    StatusCode::METHOD_NOT_ALLOWED,
                    "{method} {path}"
                );
                assert_eq!(error.allow, Some(allow), "{method} {path}");
                assert_eq!(
                    error.message,
                    "HTTP method is not supported for this project endpoint"
                );
            }
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
        // The Requester Pays billing project is ignored, as on every JSON method.
        assert!(
            route(
                Method::GET,
                "/storage/v1/projects/p/serviceAccount?userProject=p"
            )
            .is_ok()
        );
        assert_eq!(
            route(
                Method::GET,
                "/storage/v1/projects/p/serviceAccount?callback=p"
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
            project: "p".parse().unwrap(),
            service_account_email: Some("a@b".into()),
            show_deleted_keys: false,
            max_results: 1,
            start_after: None,
        };
        let token = encode_list_token(&params, "GOOG1EA");
        assert_eq!(decode_list_token(&token, &params).unwrap(), "GOOG1EA");
        for other in [
            HmacKeyListParams {
                project: "q".parse().unwrap(),
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
        let mut corrupted = token;
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
            (&br#"{"state":"DELETED"}"#[..], "invalid"),
            (br#"{"state":"active"}"#, "invalidArgument"),
            (br#"{"state":1}"#, "invalidArgument"),
            (br#"{"state":"ACTIVE","etag":1}"#, "invalidArgument"),
            (br#"{}"#, "invalid"),
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

    /// Clients keep HMAC key listing tokens across requests, so a token must keep its bytes and
    /// a token issued earlier must still open for the same filters.
    #[test]
    fn hmac_key_list_tokens_keep_their_bytes_and_open() {
        for (token, email, show_deleted_keys) in [
            (
                "5b5b226831222c2270726f6a6563742d31222c6e756c6c2c66616c73655d2c22474f4f47314558414d504c45225d.affac523806258da",
                None,
                false,
            ),
            (
                "5b5b226831222c2270726f6a6563742d31222c2273614070726f6a6563742e69616d2e67736572766963656163636f756e742e636f6d222c747275655d2c22474f4f47314558414d504c45225d.c41050cb7937b2ba",
                Some("sa@project.iam.gserviceaccount.com"),
                true,
            ),
        ] {
            let params = HmacKeyListParams {
                project: "project-1".parse().unwrap(),
                service_account_email: email.map(str::to_owned),
                show_deleted_keys,
                max_results: 10,
                start_after: None,
            };
            assert_eq!(encode_list_token(&params, "GOOG1EXAMPLE"), token);
            assert_eq!(decode_list_token(token, &params).unwrap(), "GOOG1EXAMPLE");
        }
    }
}
