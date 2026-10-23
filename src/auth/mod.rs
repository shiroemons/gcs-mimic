//! Transport-independent authentication profiles, local principals and project-role checks.
//!
//! `permissive` ignores credentials and performs no permission checks. `strict` resolves the
//! caller from a local identity registry loaded once at startup and evaluates permissions from
//! the pinned predefined-role snapshot in [`roles`] and scoped bucket policies.
//! ACLs are not modeled. Strict XML query signatures use registered RSA public keys.
//! Nothing here contacts Google or the metadata server.

mod policy;
pub(crate) mod roles;
pub mod signature;
pub use policy::{Binding, Policy};

use hyper::{HeaderMap, header::AUTHORIZATION};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    fs::File,
    io::Read,
    path::Path,
    str::FromStr,
};

pub const BUCKETS_CREATE: &str = "storage.buckets.create";
pub const BUCKETS_DELETE: &str = "storage.buckets.delete";
pub const BUCKETS_GET: &str = "storage.buckets.get";
pub const BUCKETS_UPDATE: &str = "storage.buckets.update";
pub const BUCKETS_LIST: &str = "storage.buckets.list";
pub const BUCKETS_GET_IAM: &str = "storage.buckets.getIamPolicy";
pub const BUCKETS_SET_IAM: &str = "storage.buckets.setIamPolicy";
pub const OBJECTS_CREATE: &str = "storage.objects.create";
pub const OBJECTS_DELETE: &str = "storage.objects.delete";
pub const OBJECTS_GET: &str = "storage.objects.get";
pub const OBJECTS_LIST: &str = "storage.objects.list";
pub const OBJECTS_UPDATE: &str = "storage.objects.update";

const MAX_REGISTRY_BYTES: u64 = 64 * 1024;
const MAX_IDENTITIES: usize = 256;
const MAX_ID_BYTES: usize = 254;
const MAX_TOKENS: usize = 16;
const MAX_TOKEN_BYTES: usize = 512;
const MAX_PROJECTS: usize = 64;
const MAX_ROLES: usize = 32;

/// Query parameters that carry credentials in Google APIs (OAuth tokens and V2/V4 signatures).
/// Strict JSON/bearer authentication rejects them instead of falling back to anonymous access;
/// the XML adapter dispatches supported V4 query credentials to the RSA verifier separately.
const CREDENTIAL_QUERY_PARAMETERS: &[&str] = &[
    "access_token",
    "oauth_token",
    "X-Goog-Signature",
    "X-Goog-Credential",
    "X-Goog-Algorithm",
    "X-Goog-Date",
    "X-Goog-Expires",
    "X-Goog-SignedHeaders",
    "Signature",
    "GoogleAccessId",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthMode {
    #[default]
    Permissive,
    Strict,
}

impl AuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permissive => "permissive",
            Self::Strict => "strict",
        }
    }
}

impl FromStr for AuthMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "permissive" => Ok(Self::Permissive),
            "strict" => Ok(Self::Strict),
            _ => Err("auth_mode must be permissive or strict".into()),
        }
    }
}

/// Transport-independent authentication or authorization failure; adapters render it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthFailure {
    /// Credentials were presented but are unknown, malformed or of an unsupported kind.
    InvalidCredentials,
    /// An anonymous caller needs a permission.
    LoginRequired,
    /// An authenticated caller lacks the permission.
    Forbidden,
}

impl fmt::Display for AuthFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidCredentials => "invalid credentials",
            Self::LoginRequired => "login required",
            Self::Forbidden => "forbidden",
        })
    }
}

impl std::error::Error for AuthFailure {}

/// A registered local principal with its project-scoped permissions expanded at load time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    id: String,
    permissions: BTreeMap<String, BTreeSet<&'static str>>,
}

impl Identity {
    pub(crate) fn upload_snapshot(&self) -> Value {
        serde_json::json!({"id": self.id, "projects": self.permissions.iter().map(|(project, permissions)| {
            (project.clone(), permissions.iter().filter(|p| matches!(**p, OBJECTS_CREATE | OBJECTS_DELETE)).copied().collect::<Vec<_>>())
        }).collect::<BTreeMap<_, _>>()})
    }

    pub(crate) fn from_upload_snapshot(value: &Value) -> Result<Self, String> {
        let invalid = || "invalid upload identity snapshot".to_owned();
        let id = value["id"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= MAX_ID_BYTES)
            .ok_or_else(invalid)?
            .to_owned();
        let projects = value["projects"]
            .as_object()
            .filter(|p| p.len() <= MAX_PROJECTS)
            .ok_or_else(invalid)?;
        let mut permissions = BTreeMap::new();
        for (project, entries) in projects {
            let mut set = BTreeSet::new();
            for entry in entries.as_array().ok_or_else(invalid)? {
                set.insert(match entry.as_str() {
                    Some(OBJECTS_CREATE) => OBJECTS_CREATE,
                    Some(OBJECTS_DELETE) => OBJECTS_DELETE,
                    _ => return Err(invalid()),
                });
            }
            permissions.insert(project.clone(), set);
        }
        Ok(Self { id, permissions })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Whether any granted role in `project` contains `permission`.
    pub fn allows(&self, permission: &str, project: &str) -> bool {
        self.permissions
            .get(project)
            .is_some_and(|permissions| permissions.contains(permission))
    }

    pub fn allows_anywhere(&self, permission: &str) -> bool {
        self.permissions
            .values()
            .any(|permissions| permissions.contains(permission))
    }
}

/// The resolved strict-mode caller.
#[derive(Clone, Copy, Debug)]
pub enum Caller<'a> {
    Anonymous,
    Principal(&'a Identity),
}

impl<'a> Caller<'a> {
    pub fn principal(self) -> Result<&'a Identity, AuthFailure> {
        match self {
            Self::Anonymous => Err(AuthFailure::LoginRequired),
            Self::Principal(identity) => Ok(identity),
        }
    }

    /// Authorize `permission` on a resource owned by `project`.
    ///
    /// `None` means the target bucket does not exist. The local rule returns success (so the
    /// operation reports 404) only when the caller holds the permission in at least one
    /// project; otherwise the caller cannot learn whether the bucket exists.
    pub fn authorize(self, permission: &str, project: Option<&str>) -> Result<(), AuthFailure> {
        let identity = self.principal()?;
        let allowed = match project {
            Some(project) => identity.allows(permission, project),
            None => identity.allows_anywhere(permission),
        };
        if allowed {
            Ok(())
        } else {
            Err(AuthFailure::Forbidden)
        }
    }
}

/// Local principal registry loaded from the strict-mode identity file.
#[derive(Debug)]
pub struct Registry {
    identities: Vec<Identity>,
    tokens: HashMap<String, usize>,
    signing_keys: HashMap<String, Vec<signature::PublicKey>>,
    public_access_prevention_projects: Vec<String>,
}

impl Registry {
    pub fn load(path: &Path) -> Result<Self, String> {
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(|e| format!("identity_file: {e}"))?
            .take(MAX_REGISTRY_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("identity_file: {e}"))?;
        if bytes.len() as u64 > MAX_REGISTRY_BYTES {
            return Err("identity_file exceeds 64 KiB".into());
        }
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let error = |message: String| format!("identity_file: {message}");
        let value: Value = serde_json::from_slice(bytes).map_err(|e| error(e.to_string()))?;
        let top = value
            .as_object()
            .ok_or_else(|| error("top level must be a JSON object".into()))?;
        if let Some(key) = top.keys().find(|key| {
            !matches!(
                key.as_str(),
                "identities" | "public_access_prevention_projects"
            )
        }) {
            return Err(error(format!("unknown field: {key}")));
        }
        let mut public_access_prevention_projects = Vec::new();
        if let Some(value) = top.get("public_access_prevention_projects") {
            let projects = value.as_array().filter(|projects| projects.len() <= MAX_PROJECTS)
                .ok_or_else(|| error("public_access_prevention_projects must be an array of at most 64 project identifiers".into()))?;
            for project in projects {
                let project = project.as_str().ok_or_else(|| {
                    error("public_access_prevention_projects entries must be strings".into())
                })?;
                crate::json::validate_project(project)
                    .map_err(|_| error("invalid public_access_prevention_projects entry".into()))?;
                if public_access_prevention_projects
                    .iter()
                    .any(|existing| existing == project)
                {
                    return Err(error(
                        "duplicate public_access_prevention_projects entry".into(),
                    ));
                }
                public_access_prevention_projects.push(project.to_owned());
            }
        }
        let entries = top
            .get("identities")
            .ok_or_else(|| error("missing required field: identities".into()))?
            .as_array()
            .ok_or_else(|| error("identities must be an array".into()))?;
        if !(1..=MAX_IDENTITIES).contains(&entries.len()) {
            return Err(error(format!(
                "identities must contain 1..={MAX_IDENTITIES} entries"
            )));
        }
        let mut identities = Vec::with_capacity(entries.len());
        let mut ids = BTreeSet::new();
        let mut tokens = HashMap::new();
        let mut signing_keys = HashMap::new();
        for (index, entry) in entries.iter().enumerate() {
            let error = |message: String| format!("identity_file: identities[{index}]: {message}");
            let entry = entry
                .as_object()
                .ok_or_else(|| error("must be a JSON object".into()))?;
            if let Some(key) = entry.keys().find(|key| {
                !matches!(
                    key.as_str(),
                    "id" | "tokens" | "project_roles" | "public_keys"
                )
            }) {
                return Err(error(format!("unknown field: {key}")));
            }
            let id = entry
                .get("id")
                .ok_or_else(|| error("missing required field: id".into()))?
                .as_str()
                .ok_or_else(|| error("id must be a string".into()))?;
            if !visible_ascii(id, MAX_ID_BYTES) {
                return Err(error(format!(
                    "id must be 1..={MAX_ID_BYTES} bytes of printable ASCII without whitespace"
                )));
            }
            if !ids.insert(id.to_owned()) {
                return Err(error(format!("duplicate id: {id}")));
            }
            if let Some(keys) = entry.get("public_keys") {
                let keys = keys
                    .as_array()
                    .filter(|keys| (1..=8).contains(&keys.len()))
                    .ok_or_else(|| {
                        error(
                            "public_keys must be an array of 1..=8 inline public PEM strings"
                                .into(),
                        )
                    })?;
                let keys =
                    keys.iter()
                        .map(|key| {
                            signature::PublicKey::parse(key.as_str().ok_or_else(|| {
                                error("public_keys entries must be strings".into())
                            })?)
                            .map_err(error)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                signing_keys.insert(id.to_owned(), keys);
            }
            let token_values = entry
                .get("tokens")
                .ok_or_else(|| error("missing required field: tokens".into()))?
                .as_array()
                .ok_or_else(|| error("tokens must be an array".into()))?;
            if token_values.len() > MAX_TOKENS
                || (token_values.is_empty() && !signing_keys.contains_key(id))
            {
                return Err(error(format!(
                    "tokens must contain 1..={MAX_TOKENS} entries"
                )));
            }
            for token in token_values {
                let token = token
                    .as_str()
                    .filter(|token| visible_ascii(token, MAX_TOKEN_BYTES))
                    .ok_or_else(|| {
                        error(format!(
                            "each token must be 1..={MAX_TOKEN_BYTES} bytes of visible ASCII"
                        ))
                    })?;
                // Never echo token values into startup errors or logs.
                if tokens.insert(token.to_owned(), index).is_some() {
                    return Err(error("duplicate token".into()));
                }
            }
            let mut permissions = BTreeMap::new();
            if let Some(projects) = entry.get("project_roles") {
                let projects = projects
                    .as_object()
                    .ok_or_else(|| error("project_roles must be a JSON object".into()))?;
                if projects.len() > MAX_PROJECTS {
                    return Err(error(format!(
                        "project_roles must contain at most {MAX_PROJECTS} projects"
                    )));
                }
                for (project, role_values) in projects {
                    crate::json::validate_project(project)
                        .map_err(|e| error(format!("project {project:?}: {}", e.message)))?;
                    let role_values = role_values
                        .as_array()
                        .filter(|roles| (1..=MAX_ROLES).contains(&roles.len()))
                        .ok_or_else(|| {
                            error(format!(
                                "project {project:?}: roles must be an array of 1..={MAX_ROLES} role names"
                            ))
                        })?;
                    let mut names = BTreeSet::new();
                    let granted: &mut BTreeSet<&'static str> =
                        permissions.entry(project.clone()).or_default();
                    for role in role_values {
                        let role = role.as_str().ok_or_else(|| {
                            error(format!("project {project:?}: role names must be strings"))
                        })?;
                        if !names.insert(role) {
                            return Err(error(format!(
                                "project {project:?}: duplicate role {role}"
                            )));
                        }
                        let expanded = roles::permissions(role).ok_or_else(|| {
                            error(format!(
                                "project {project:?}: unsupported role {role} (only pinned predefined Cloud Storage roles are available)"
                            ))
                        })?;
                        granted.extend(expanded.iter().copied());
                    }
                }
            }
            identities.push(Identity {
                id: id.to_owned(),
                permissions,
            });
        }
        Ok(Self {
            identities,
            tokens,
            signing_keys,
            public_access_prevention_projects,
        })
    }

    pub fn len(&self) -> usize {
        self.identities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.identities.is_empty()
    }
}

fn visible_ascii(value: &str, max: usize) -> bool {
    (1..=max).contains(&value.len()) && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

/// The active authentication profile. The default is permissive.
#[derive(Debug, Default)]
pub struct Auth {
    registry: Option<Registry>,
}

impl Auth {
    pub fn permissive() -> Self {
        Self::default()
    }

    pub fn strict(registry: Registry) -> Self {
        Self {
            registry: Some(registry),
        }
    }

    pub fn mode(&self) -> AuthMode {
        if self.registry.is_some() {
            AuthMode::Strict
        } else {
            AuthMode::Permissive
        }
    }

    pub fn identity_count(&self) -> Option<usize> {
        self.registry.as_ref().map(Registry::len)
    }

    /// Local simulation of the effective parent organization policy, loaded before readiness.
    pub fn public_access_prevention_projects(&self) -> &[String] {
        self.registry
            .as_ref()
            .map_or(&[], |registry| &registry.public_access_prevention_projects)
    }

    /// Resolve the caller from request metadata only; never reads a body or the catalog.
    ///
    /// Returns `Ok(None)` in permissive mode, where no permission checks apply.
    pub fn authenticate(
        &self,
        headers: &HeaderMap,
        query: Option<&str>,
    ) -> Result<Option<Caller<'_>>, AuthFailure> {
        let Some(registry) = &self.registry else {
            return Ok(None);
        };
        if query.is_some_and(has_credential_parameter) {
            return Err(AuthFailure::InvalidCredentials);
        }
        let mut values = headers.get_all(AUTHORIZATION).iter();
        let Some(value) = values.next() else {
            return Ok(Some(Caller::Anonymous));
        };
        if values.next().is_some() {
            return Err(AuthFailure::InvalidCredentials);
        }
        let value = value
            .to_str()
            .map_err(|_| AuthFailure::InvalidCredentials)?;
        let (scheme, token) = value.split_once(' ').unwrap_or((value, ""));
        if !scheme.eq_ignore_ascii_case("bearer") {
            return Err(AuthFailure::InvalidCredentials);
        }
        let token = token.trim_start_matches(' ');
        // Registry tokens are visible ASCII, so an empty or space-containing token never matches.
        registry
            .tokens
            .get(token)
            .map(|index| Some(Caller::Principal(&registry.identities[*index])))
            .ok_or(AuthFailure::InvalidCredentials)
    }
}

fn has_credential_parameter(query: &str) -> bool {
    query
        .split('&')
        .map(|pair| pair.split_once('=').map_or(pair, |(key, _)| key))
        .any(|key| {
            let key = lenient_decode(key);
            CREDENTIAL_QUERY_PARAMETERS
                .iter()
                .any(|name| key.eq_ignore_ascii_case(name.as_bytes()))
        })
}

/// Percent-decode a query key without failing, so malformed encodings cannot hide credentials.
fn lenient_decode(raw: &str) -> Vec<u8> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let hex = |index: usize| {
        bytes
            .get(index)
            .and_then(|byte| char::from(*byte).to_digit(16))
            .map(|digit| digit as u8)
    };
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let (Some(high), Some(low)) = (hex(index + 1), hex(index + 2))
        {
            decoded.push(high << 4 | low);
            index += 3;
            continue;
        }
        decoded.push(if bytes[index] == b'+' {
            b' '
        } else {
            bytes[index]
        });
        index += 1;
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pap_parent_fixture_rejects_malformed_and_unbounded_projects() {
        for projects in [
            serde_json::json!(null),
            serde_json::json!(true),
            serde_json::json!({}),
            serde_json::json!([1]),
            serde_json::json!([""]),
            serde_json::json!(["has space"]),
            serde_json::json!(["p", "p"]),
            serde_json::json!((0..65).map(|n| format!("p{n}")).collect::<Vec<_>>()),
        ] {
            let value = serde_json::json!({"identities":[{"id":"local","tokens":["token"]}],"public_access_prevention_projects":projects});
            assert!(
                Registry::parse(value.to_string().as_bytes())
                    .unwrap_err()
                    .contains("public_access_prevention_projects")
            );
        }
    }

    #[tokio::test]
    async fn pap_mutations_require_update_and_set_iam_permissions() {
        use crate::{
            bucket::{BucketAccess, BucketMetadataUpdate, CatalogError},
            storage::Catalog,
        };
        let dir =
            std::env::temp_dir().join(format!("gcs-mimic-pap-permissions-{}", std::process::id()));
        let catalog = Catalog::open(&dir).await.unwrap();
        let record = crate::json::parse_insert(
            br#"{"name":"pap-permissions"}"#,
            "project",
            std::time::UNIX_EPOCH,
        )
        .unwrap();
        catalog.insert_bucket(record.clone()).await.unwrap();
        let identity = Identity {
            id: "local".into(),
            permissions: BTreeMap::from([("project".into(), BTreeSet::from([BUCKETS_UPDATE]))]),
        };
        let access = BucketAccess::Policy {
            identity: Some(identity),
            permission: BUCKETS_UPDATE.into(),
            project: Some("project".into()),
        };
        for replace in [false, true] {
            let update = BucketMetadataUpdate {
                public_access_prevention_enforced: Some(true),
                ..Default::default()
            };
            let result = if replace {
                catalog
                    .replace_bucket("pap-permissions", update, None, None, &access)
                    .await
            } else {
                catalog
                    .patch_bucket("pap-permissions", update, None, None, &access)
                    .await
            };
            assert_eq!(result, Err(CatalogError::Forbidden));
            assert_eq!(
                catalog.get_bucket("pap-permissions").await.unwrap(),
                Some(record.clone())
            );
        }
        catalog.close().await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn role_snapshot_is_sorted_unique_storage_permission_data() {
        let names: Vec<_> = roles::PREDEFINED_ROLES
            .iter()
            .map(|(name, _, _)| *name)
            .collect();
        assert!(names.windows(2).all(|pair| pair[0] < pair[1]), "{names:?}");
        for (role, _, permissions) in roles::PREDEFINED_ROLES {
            assert!(role.starts_with("roles/storage."), "{role}");
            assert!(!permissions.is_empty(), "{role}");
            assert!(
                permissions.windows(2).all(|pair| pair[0] < pair[1]),
                "{role} permissions must be sorted and unique"
            );
            for permission in *permissions {
                assert!(permission.starts_with("storage."), "{role}: {permission}");
                assert_eq!(permission.split('.').count(), 3, "{role}: {permission}");
            }
            assert_eq!(roles::permissions(role), Some(*permissions));
        }
        for basic in [
            "roles/owner",
            "roles/editor",
            "roles/viewer",
            "roles/storage.",
        ] {
            assert_eq!(roles::permissions(basic), None);
        }
    }

    #[test]
    fn evaluation_is_driven_by_snapshot_data() {
        // Grant every role in its own project and check each permission only via the data.
        let projects: Vec<_> = (0..roles::PREDEFINED_ROLES.len())
            .map(|index| format!("project-{index}"))
            .collect();
        let mut registry = Vec::new();
        for (chunk_index, chunk) in roles::PREDEFINED_ROLES.chunks(MAX_PROJECTS).enumerate() {
            let project_roles: serde_json::Map<_, _> = chunk
                .iter()
                .enumerate()
                .map(|(offset, (role, _, _))| {
                    (
                        projects[chunk_index * MAX_PROJECTS + offset].clone(),
                        serde_json::json!([role]),
                    )
                })
                .collect();
            registry.push(serde_json::json!({
                "id": format!("user-{chunk_index}@example.invalid"),
                "tokens": [format!("token-{chunk_index}")],
                "project_roles": project_roles,
            }));
        }
        let registry = Registry::parse(
            serde_json::json!({ "identities": registry })
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        let identity = &registry.identities[0];
        let all: BTreeSet<_> = roles::PREDEFINED_ROLES
            .iter()
            .flat_map(|(_, _, permissions)| permissions.iter().copied())
            .collect();
        for (index, (_, _, permissions)) in roles::PREDEFINED_ROLES.iter().enumerate() {
            for permission in &all {
                assert_eq!(
                    identity.allows(permission, &projects[index]),
                    permissions.contains(permission),
                    "{index} {permission}"
                );
            }
        }
        assert!(!identity.allows(OBJECTS_GET, "unlisted-project"));
        assert!(identity.allows_anywhere(OBJECTS_GET));
        let caller = Caller::Principal(identity);
        assert_eq!(caller.authorize(BUCKETS_GET, None), Ok(()));
        assert_eq!(
            caller.authorize(BUCKETS_GET, Some("unlisted-project")),
            Err(AuthFailure::Forbidden)
        );
        assert_eq!(
            Caller::Anonymous.authorize(BUCKETS_GET, None),
            Err(AuthFailure::LoginRequired)
        );
    }

    #[test]
    fn notable_role_consequences_follow_the_snapshot() {
        let has =
            |role: &str, permission: &str| roles::permissions(role).unwrap().contains(&permission);
        assert!(!has("roles/storage.objectViewer", BUCKETS_GET));
        assert!(has("roles/storage.objectViewer", OBJECTS_GET));
        assert!(has("roles/storage.objectCreator", OBJECTS_CREATE));
        assert!(!has("roles/storage.objectCreator", OBJECTS_DELETE));
        assert!(!has("roles/storage.objectCreator", OBJECTS_GET));
        assert!(has("roles/storage.editor", BUCKETS_CREATE));
        assert!(!has("roles/storage.editor", BUCKETS_GET));
        assert!(!has("roles/storage.objectAdmin", BUCKETS_CREATE));
    }

    fn strict() -> Auth {
        Auth::strict(
            Registry::parse(
                br#"{"identities":[{"id":"a@example.invalid","tokens":["tok-a","tok-b"],"project_roles":{"p":["roles/storage.admin"]}}]}"#,
            )
            .unwrap(),
        )
    }

    fn headers(values: &[&[u8]]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(
                AUTHORIZATION,
                hyper::header::HeaderValue::from_bytes(value).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn strict_caller_resolution() {
        let auth = strict();
        let id = |result: Result<Option<Caller<'_>>, AuthFailure>| match result {
            Ok(Some(Caller::Principal(identity))) => Ok(Some(identity.id().to_owned())),
            Ok(Some(Caller::Anonymous)) => Ok(None),
            Ok(None) => panic!("strict mode must resolve a caller"),
            Err(error) => Err(error),
        };
        assert_eq!(id(auth.authenticate(&headers(&[]), None)), Ok(None));
        for value in [&b"Bearer tok-a"[..], b"bearer tok-b", b"BEARER   tok-a"] {
            assert_eq!(
                id(auth.authenticate(&headers(&[value]), Some("project=p"))),
                Ok(Some("a@example.invalid".into())),
                "{}",
                String::from_utf8_lossy(value)
            );
        }
        for value in [
            &b"Bearer"[..],
            b"Bearer ",
            b"Bearer tok-c",
            b"Bearer tok-a extra",
            b"Bearer\ttok-a",
            b"Basic dG9rLWE6",
            b"GOOG4-RSA-SHA256 Credential=x",
            b"GOOG4-HMAC-SHA256 Credential=x",
            b"AWS4-HMAC-SHA256 Credential=x",
            b"tok-a",
            b"Bearer \xfftok",
        ] {
            assert_eq!(
                id(auth.authenticate(&headers(&[value]), None)),
                Err(AuthFailure::InvalidCredentials),
                "{}",
                String::from_utf8_lossy(value)
            );
        }
        assert_eq!(
            id(auth.authenticate(&headers(&[b"Bearer tok-a", b"Bearer tok-a"]), None)),
            Err(AuthFailure::InvalidCredentials)
        );
        for query in [
            "access_token=tok-a",
            "a=b&X-Goog-Signature=00",
            "x-goog-credential=x",
            "X-Goog-Algorithm",
            "Signature=x&GoogleAccessId=y",
            "access%5Ftoken=tok-a",
            "oauth_token=tok-a",
        ] {
            assert_eq!(
                id(auth.authenticate(&headers(&[b"Bearer tok-a"]), Some(query))),
                Err(AuthFailure::InvalidCredentials),
                "{query}"
            );
            assert_eq!(
                id(auth.authenticate(&headers(&[]), Some(query))),
                Err(AuthFailure::InvalidCredentials),
                "{query}"
            );
        }
        let permissive = Auth::permissive();
        assert!(matches!(
            permissive.authenticate(&headers(&[b"Basic garbage", b"x"]), Some("access_token=x")),
            Ok(None)
        ));
    }

    #[test]
    fn registry_validation_rejects_invalid_files_without_partial_load() {
        let valid = |identity: &str| format!(r#"{{"identities":[{identity}]}}"#);
        let ok =
            r#"{"id":"a","tokens":["t"],"project_roles":{"p":["roles/storage.objectViewer"]}}"#;
        assert_eq!(Registry::parse(valid(ok).as_bytes()).unwrap().len(), 1);
        assert_eq!(
            Registry::parse(valid(r#"{"id":"a","tokens":["t"]}"#).as_bytes())
                .unwrap()
                .len(),
            1
        );
        let long_id = "a".repeat(255);
        let long_token = "t".repeat(513);
        let many_tokens: Vec<_> = (0..17).map(|n| format!("t{n}")).collect();
        let many_roles = vec!["roles/storage.objectViewer"; 33];
        let many_projects: serde_json::Map<_, _> = (0..65)
            .map(|n| (format!("p{n}"), serde_json::json!(["roles/storage.viewer"])))
            .collect();
        let many_identities: Vec<_> = (0..257)
            .map(|n| serde_json::json!({"id": format!("u{n}"), "tokens": [format!("t{n}")]}))
            .collect();
        for (case, document) in [
            ("malformed", "{".to_owned()),
            ("not object", "[]".into()),
            ("missing identities", "{}".into()),
            ("unknown top", r#"{"identities":[],"extra":1}"#.into()),
            ("empty", r#"{"identities":[]}"#.into()),
            (
                "too many identities",
                serde_json::json!({ "identities": many_identities }).to_string(),
            ),
            (
                "unknown field",
                valid(r#"{"id":"a","tokens":["t"],"role":"x"}"#),
            ),
            ("missing id", valid(r#"{"tokens":["t"]}"#)),
            ("empty id", valid(r#"{"id":"","tokens":["t"]}"#)),
            ("space id", valid(r#"{"id":"a b","tokens":["t"]}"#)),
            (
                "long id",
                valid(&format!(r#"{{"id":"{long_id}","tokens":["t"]}}"#)),
            ),
            (
                "duplicate id",
                valid(r#"{"id":"a","tokens":["t"]},{"id":"a","tokens":["u"]}"#),
            ),
            ("no tokens", valid(r#"{"id":"a","tokens":[]}"#)),
            ("missing tokens", valid(r#"{"id":"a"}"#)),
            (
                "too many tokens",
                valid(&serde_json::json!({"id":"a","tokens":many_tokens}).to_string()),
            ),
            ("empty token", valid(r#"{"id":"a","tokens":[""]}"#)),
            ("space token", valid(r#"{"id":"a","tokens":["a b"]}"#)),
            ("non-ascii token", valid(r#"{"id":"a","tokens":["tö"]}"#)),
            (
                "long token",
                valid(&format!(r#"{{"id":"a","tokens":["{long_token}"]}}"#)),
            ),
            (
                "duplicate token",
                valid(r#"{"id":"a","tokens":["t"]},{"id":"b","tokens":["t"]}"#),
            ),
            (
                "duplicate own token",
                valid(r#"{"id":"a","tokens":["t","t"]}"#),
            ),
            (
                "unknown role",
                valid(
                    r#"{"id":"a","tokens":["t"],"project_roles":{"p":["roles/storage.nothing"]}}"#,
                ),
            ),
            (
                "basic owner",
                valid(r#"{"id":"a","tokens":["t"],"project_roles":{"p":["roles/owner"]}}"#),
            ),
            (
                "basic viewer",
                valid(r#"{"id":"a","tokens":["t"],"project_roles":{"p":["roles/viewer"]}}"#),
            ),
            (
                "empty roles",
                valid(r#"{"id":"a","tokens":["t"],"project_roles":{"p":[]}}"#),
            ),
            (
                "duplicate role",
                valid(
                    r#"{"id":"a","tokens":["t"],"project_roles":{"p":["roles/storage.admin","roles/storage.admin"]}}"#,
                ),
            ),
            (
                "too many roles",
                valid(
                    &serde_json::json!({"id":"a","tokens":["t"],"project_roles":{"p":many_roles}})
                        .to_string(),
                ),
            ),
            (
                "too many projects",
                valid(
                    &serde_json::json!({"id":"a","tokens":["t"],"project_roles":many_projects})
                        .to_string(),
                ),
            ),
            (
                "invalid project",
                valid(
                    r#"{"id":"a","tokens":["t"],"project_roles":{"bad project":["roles/storage.admin"]}}"#,
                ),
            ),
            (
                "empty project",
                valid(r#"{"id":"a","tokens":["t"],"project_roles":{"":["roles/storage.admin"]}}"#),
            ),
        ] {
            let error = Registry::parse(document.as_bytes()).unwrap_err();
            assert!(error.starts_with("identity_file"), "{case}: {error}");
            assert!(
                !error.contains("\"t\""),
                "{case}: tokens must not be echoed"
            );
        }
    }

    #[test]
    fn registry_file_read_is_bounded() {
        let path = std::env::temp_dir().join(format!(
            "gcs-mimic-identity-bound-{}.json",
            std::process::id()
        ));
        let mut document = r#"{"identities":[{"id":"a","tokens":["t"]}]}"#.to_owned();
        document.push_str(&" ".repeat(64 * 1024 - document.len()));
        std::fs::write(&path, &document).unwrap();
        assert_eq!(Registry::load(&path).unwrap().len(), 1);
        document.push(' ');
        std::fs::write(&path, &document).unwrap();
        assert_eq!(
            Registry::load(&path).unwrap_err(),
            "identity_file exceeds 64 KiB"
        );
        std::fs::remove_file(&path).unwrap();
        assert!(
            Registry::load(&path)
                .unwrap_err()
                .starts_with("identity_file: ")
        );
    }
}
