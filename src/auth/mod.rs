//! Transport-independent authentication profiles, local principals and project-role checks.
//!
//! `permissive` ignores credentials and performs no permission checks; with `signed_urls=verify`
//! it first verifies signed XML requests as strict mode does. `strict` resolves the
//! caller from a local identity registry loaded once at startup and evaluates permissions from
//! the pinned predefined-role snapshot in [`roles`] and scoped bucket policies.
//! Selected object generations also evaluate ACL read grants in catalog transactions.
//! Strict XML query signatures use registered RSA public keys.
//! Nothing here contacts Google or the metadata server.

mod acl_identity;
pub(crate) use acl_identity::validate_local_creator_email;
mod jwt;
mod policy;
pub(crate) mod policy_input;
/// Shared identifier rules; adapters supply their own field names and error messages.
pub(crate) fn valid_project_id(project: &str) -> bool {
    !project.is_empty()
        && project.len() <= 256
        && !project
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}
pub(crate) mod roles;
pub mod signature;
mod signing_keys;
use signing_keys::SigningKeys;
pub mod test_signer;
pub(crate) use policy::PublicAccess;
pub(crate) use policy::{Binding, Policy};

use crate::config::{AuthProfile, DocumentKind, DocumentSource, Form, Setting};
use hyper::{HeaderMap, header::AUTHORIZATION};
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// The documented 401 body cannot explain JWT setup failures, so log guidance once.
static WARNED_SELF_SIGNED_JWT: AtomicBool = AtomicBool::new(false);

// `untestable` excludes a permission from bucket testIamPermissions, not authorization.
macro_rules! permissions {
    ($($variant:ident $(($constant:ident))? => $name:literal $([$untestable:ident])?),+ $(,)?) => {
        $($(pub(crate) const $constant: Permission = Permission::$variant;)?)+

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum Permission {
            $($variant),+
        }

        impl Permission {
            pub(crate) const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub(crate) fn parse_testable(value: &str) -> Option<Self> {
                match value { $($name => permissions!(@parsed Self::$variant $(, $untestable)?),)+ _ => None }
            }

            const fn bit(self) -> u32 { 1 << self as u32 }

            pub(crate) fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }
        }

        impl TryFrom<&str> for Permission {
            type Error = UnsupportedPermission;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::parse_testable(value).ok_or(UnsupportedPermission)
            }
        }
        impl fmt::Display for Permission {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }
    };
    (@parsed $variant:path) => { Some($variant) };
    (@parsed $variant:path, untestable) => { None };
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct UnsupportedPermission;

impl fmt::Display for UnsupportedPermission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unsupported permission")
    }
}

impl std::error::Error for UnsupportedPermission {}

permissions! {
    BucketsCreate(BUCKETS_CREATE) => "storage.buckets.create",
    BucketsDelete(BUCKETS_DELETE) => "storage.buckets.delete",
    BucketsGet(BUCKETS_GET) => "storage.buckets.get",
    BucketsUpdate(BUCKETS_UPDATE) => "storage.buckets.update",
    BucketsList(BUCKETS_LIST) => "storage.buckets.list",
    BucketsRestore(BUCKETS_RESTORE) => "storage.buckets.restore",
    BucketsGetIam(BUCKETS_GET_IAM) => "storage.buckets.getIamPolicy",
    BucketsSetIam(BUCKETS_SET_IAM) => "storage.buckets.setIamPolicy",
    ObjectsCreate(OBJECTS_CREATE) => "storage.objects.create",
    ObjectsDelete(OBJECTS_DELETE) => "storage.objects.delete",
    ObjectsGet(OBJECTS_GET) => "storage.objects.get",
    ObjectsList(OBJECTS_LIST) => "storage.objects.list",
    ObjectsUpdate(OBJECTS_UPDATE) => "storage.objects.update",
    ObjectsRestore(OBJECTS_RESTORE) => "storage.objects.restore",
    ObjectsSetRetention(OBJECTS_SET_RETENTION) => "storage.objects.setRetention",
    ObjectsOverrideUnlockedRetention(OBJECTS_OVERRIDE_UNLOCKED_RETENTION) => "storage.objects.overrideUnlockedRetention",
    ObjectsGetIam(OBJECTS_GET_IAM) => "storage.objects.getIamPolicy",
    ObjectsSetIam(OBJECTS_SET_IAM) => "storage.objects.setIamPolicy",
    MultipartCreate => "storage.multipartUploads.create",
    MultipartAbort => "storage.multipartUploads.abort",
    MultipartList => "storage.multipartUploads.list",
    MultipartListParts => "storage.multipartUploads.listParts",
    BucketsEnableObjectRetention(BUCKETS_ENABLE_OBJECT_RETENTION) => "storage.buckets.enableObjectRetention" [untestable],
    HmacKeysCreate => "storage.hmacKeys.create" [untestable],
    HmacKeysDelete => "storage.hmacKeys.delete" [untestable],
    HmacKeysGet => "storage.hmacKeys.get" [untestable],
    HmacKeysList => "storage.hmacKeys.list" [untestable],
    HmacKeysUpdate => "storage.hmacKeys.update" [untestable],
}

const _: () = assert!(Permission::ALL.len() <= 32);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PermissionSet(u32);

impl PermissionSet {
    fn contains(self, permission: Permission) -> bool {
        self.0 & permission.bit() != 0
    }
    fn insert(&mut self, permission: Permission) {
        self.0 |= permission.bit();
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ProjectGrant {
    permissions: PermissionSet,
    any_storage_permission: bool,
    explicit_storage_admin: bool,
}

impl ProjectGrant {
    fn grant(&mut self, permission: Permission) {
        self.any_storage_permission = true;
        self.permissions.insert(permission);
    }
}

#[cfg(test)]
impl FromIterator<Permission> for ProjectGrant {
    fn from_iter<T: IntoIterator<Item = Permission>>(iter: T) -> Self {
        let mut grant = Self::default();
        for permission in iter {
            grant.grant(permission);
        }
        grant
    }
}

/// The file is trusted and read once at startup, and every lookup goes through an index, so its
/// size is all that needs a bound: it is held in memory while it parses.
const MAX_REGISTRY_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REGISTRY_MIB: u64 = MAX_REGISTRY_BYTES / (1024 * 1024);
/// The longest email address; object owners and ACL entities store the id with little to spare.
const MAX_ID_BYTES: usize = 254;
/// A registered token arrives in the same header as a JWT, so the same bound fits both.
const MAX_TOKEN_BYTES: usize = jwt::MAX_TOKEN_BYTES;

/// Query parameters that carry credentials in Google APIs (OAuth tokens and V2/V4 signatures).
/// Unlike XML signing parameters this set includes OAuth tokens and excludes standalone Expires.
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
    "X-Amz-Algorithm",
    "X-Amz-Credential",
    "X-Amz-Date",
    "X-Amz-Expires",
    "X-Amz-SignedHeaders",
    "X-Amz-Signature",
];

crate::config::string_enum! {
    pub enum AuthMode {
        #[default]
        Permissive => "permissive",
        Strict => "strict",
    }
    "auth_mode must be permissive or strict"
}

crate::config::string_enum! {
    no_default
    /// Whether the built-in [`test_signer`] identity exists. Its private key is published, so
    /// the default depends on the profile; see [`TestSignerChoice::select`].
    pub enum TestSigner {
        Enabled => "enabled",
        Disabled => "disabled",
    }
    "test_signer must be enabled or disabled"
}

/// How a profile settles the built-in test signer. Validation selects the rule; only strict
/// mode's default needs the identity document, so loading settles that one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestSignerChoice {
    /// An explicit `test_signer`, or a default that the profile alone decides.
    Fixed(TestSigner),
    /// Strict mode's default: enabled exactly when the identity document declares
    /// `test-signer@gcs-mimic.invalid`.
    WhenDeclared,
}

impl TestSignerChoice {
    /// An explicit setting wins. Otherwise permissive mode, which authorizes nothing by
    /// identity, enables the signer wherever signatures verify; where they are ignored it would
    /// do nothing. Strict mode leaves it to the identity document: anyone can sign with the
    /// published key, so it would turn `allAuthenticatedUsers` grants into public access, and
    /// an entry for its ID says that this local setup asked for it.
    pub fn select(configured: Option<TestSigner>, mode: AuthMode, signed_urls: SignedUrls) -> Self {
        match (configured, mode, signed_urls) {
            (Some(signer), _, _) => Self::Fixed(signer),
            (None, AuthMode::Permissive, SignedUrls::Verify) => Self::Fixed(TestSigner::Enabled),
            (None, AuthMode::Permissive, SignedUrls::Ignore) => Self::Fixed(TestSigner::Disabled),
            (None, AuthMode::Strict, SignedUrls::Ignore | SignedUrls::Verify) => Self::WhenDeclared,
        }
    }

    /// The effective value, given whether the identity document declares the signer's ID.
    fn settle(self, declares_signer: impl FnOnce() -> bool) -> TestSigner {
        match self {
            Self::Fixed(signer) => signer,
            Self::WhenDeclared if declares_signer() => TestSigner::Enabled,
            Self::WhenDeclared => TestSigner::Disabled,
        }
    }
}

crate::config::string_enum! {
    no_default
    /// How signed XML requests are treated. Strict mode always verifies them, because their
    /// signer is the caller; permissive mode ignores their credentials unless asked to verify.
    pub enum SignedUrls {
        Ignore => "ignore",
        Verify => "verify",
    }
    "signed_urls must be ignore or verify"
}

impl SignedUrls {
    /// An explicit setting wins; otherwise strict mode verifies and permissive mode ignores.
    pub fn resolve(configured: Option<Self>, mode: AuthMode) -> Self {
        configured.unwrap_or(match mode {
            AuthMode::Permissive => Self::Ignore,
            AuthMode::Strict => Self::Verify,
        })
    }
}

/// What an identity file supplies, which depends on the profile that loads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IdentityUse {
    /// Strict mode: principals with tokens, grants, ACL aliases and signing keys.
    Principals,
    /// Permissive mode with `signed_urls=verify`: signing keys only, because nothing is
    /// authorized by identity there.
    SigningKeys,
}

impl IdentityUse {
    /// A declared field this use would otherwise accept and ignore.
    fn rejected_field(self, entry: &serde_json::Map<String, Value>) -> Option<&'static str> {
        match self {
            Self::Principals => None,
            Self::SigningKeys => ["project_roles", "acl_entity"]
                .into_iter()
                .find(|field| entry.contains_key(*field))
                .or_else(|| {
                    entry
                        .get("tokens")
                        .and_then(Value::as_array)
                        .is_some_and(|tokens| !tokens.is_empty())
                        .then_some("tokens")
                }),
        }
    }
}

fn strict_only(field: &str) -> String {
    format!(
        "{field} requires auth_mode=strict; with auth_mode=permissive and signed_urls=verify an identity file supplies only id and public_keys"
    )
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

#[derive(Debug)]
pub(crate) enum AuthError {
    Failure(AuthFailure),
    Unavailable(crate::storage::failure::BackendCause),
}

impl From<AuthFailure> for AuthError {
    fn from(error: AuthFailure) -> Self {
        Self::Failure(error)
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failure(error) => error.fmt(f),
            Self::Unavailable(_) => f.write_str("Authentication verifier unavailable"),
        }
    }
}

impl std::error::Error for AuthError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Failure(error) => Some(error),
            Self::Unavailable(error) => Some(error),
        }
    }
}

enum Presented<'a> {
    Anonymous,
    Token(&'a Identity),
    Jwt {
        candidate: jwt::Candidate<'a>,
        warn: bool,
    },
}

/// A registered local principal with its project-scoped permissions expanded at load time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    id: String,
    acl_entity: Option<String>,
    /// Shared because requests clone the caller, and a principal may hold any number of projects.
    projects: Arc<BTreeMap<String, ProjectGrant>>,
}

fn upload_grants(grant: &ProjectGrant) -> Vec<&'static str> {
    [Permission::ObjectsCreate, Permission::ObjectsDelete]
        .into_iter()
        .filter(|permission| grant.permissions.contains(*permission))
        .map(Permission::as_str)
        .collect()
}

impl Identity {
    pub(crate) fn upload_snapshot(&self) -> Value {
        let projects: BTreeMap<_, _> = self
            .projects
            .iter()
            .map(|(project, grant)| (project, upload_grants(grant)))
            .collect();
        serde_json::json!({"id": self.id, "projects": projects})
    }

    /// The snapshot an upload in a bucket of `project` stores. Stored options hold at most 64 KiB
    /// and only the bucket's own project can authorize the upload, so the others stay out however
    /// many a principal holds.
    pub(crate) fn upload_snapshot_for(&self, project: &str) -> Value {
        let projects: BTreeMap<_, _> = self
            .projects
            .get_key_value(project)
            .map(|(project, grant)| (project, upload_grants(grant)))
            .into_iter()
            .collect();
        serde_json::json!({"id": self.id, "projects": projects})
    }

    pub(crate) fn from_upload_snapshot(value: &Value) -> Result<Self, InvalidUploadSnapshot> {
        let invalid = || InvalidUploadSnapshot;
        let id = value["id"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= MAX_ID_BYTES)
            .ok_or_else(invalid)?
            .to_owned();
        let projects = value["projects"].as_object().ok_or_else(invalid)?;
        let mut permissions = BTreeMap::new();
        for (project, entries) in projects {
            let mut set = ProjectGrant::default();
            for entry in entries.as_array().ok_or_else(invalid)? {
                set.grant(match entry.as_str() {
                    Some("storage.objects.create") => OBJECTS_CREATE,
                    Some("storage.objects.delete") => OBJECTS_DELETE,
                    _ => return Err(invalid()),
                });
            }
            permissions.insert(project.clone(), set);
        }
        Ok(Self {
            id,
            acl_entity: None,
            projects: permissions.into(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn has_explicit_storage_admin(&self, project: &str) -> bool {
        self.projects
            .get(project)
            .is_some_and(|grant| grant.explicit_storage_admin)
    }

    /// Whether any granted role in `project` contains `permission`.
    pub(crate) fn allows(&self, permission: Permission, project: &str) -> bool {
        self.projects
            .get(project)
            .is_some_and(|grant| grant.permissions.contains(permission))
    }

    /// Whether any role is granted in `project`. The pinned snapshot keeps only `storage.*`
    /// permissions, so this stands in for project-level checks such as
    /// `resourcemanager.projects.get`.
    pub(crate) fn has_project_grant(&self, project: &str) -> bool {
        self.projects
            .get(project)
            .is_some_and(|grant| grant.any_storage_permission)
    }

    pub(crate) fn allows_anywhere(&self, permission: Permission) -> bool {
        self.projects
            .values()
            .any(|grant| grant.permissions.contains(permission))
    }

    /// Projects in which any granted role contains `permission`.
    pub(crate) fn projects_allowing(&self, permission: Permission) -> Vec<String> {
        self.projects
            .iter()
            .filter(|(_, grant)| grant.permissions.contains(permission))
            .map(|(project, _)| project.clone())
            .collect()
    }
}

/// The listener a request arrived on. It decides only who a request without credentials acts
/// as: presented credentials authenticate the same way on every listener, and the caller then
/// goes through the same authorization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ListenerRole {
    /// Serves browsers and every other client. A request without credentials is anonymous, so
    /// only `allUsers` grants authorize it.
    Public,
    /// Serves the application's own servers on a private network. A request without credentials
    /// acts as `trusted_principal`.
    Trusted,
}

/// Why `trusted_principal` cannot name the identity it was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustedPrincipalError {
    /// Only strict mode has registered identities to act as.
    RequiresStrict,
    /// No identity has that id: neither the identity file nor the built-in test signer.
    Unknown,
}

impl fmt::Display for TrustedPrincipalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RequiresStrict => "trusted_principal requires auth_mode=strict",
            Self::Unknown => "trusted_principal is not a registered identity",
        })
    }
}

impl std::error::Error for TrustedPrincipalError {}

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
    pub(crate) fn authorize(
        self,
        permission: Permission,
        project: Option<&str>,
    ) -> Result<(), AuthFailure> {
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

/// A persisted resumable-upload identity snapshot that no longer decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidUploadSnapshot;

impl fmt::Display for InvalidUploadSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid upload identity snapshot")
    }
}

impl std::error::Error for InvalidUploadSnapshot {}

/// Why an identity document is invalid. `Display` names, for an invalid entry, its index; token
/// values are never included. [`IdentityError`] adds the setting that supplied the document.
#[derive(Debug)]
pub enum RegistryError {
    TooLarge,
    Io(std::io::Error),
    /// The file is not valid JSON.
    Syntax(serde_json::Error),
    Invalid {
        entry: Option<usize>,
        reason: String,
    },
    /// An identity's `public_keys` entry is not a supported RSA public key.
    PublicKey {
        entry: usize,
        source: signature::InvalidPublicKey,
    },
    /// An identity's `project_roles` names a project the JSON API rejects.
    Project {
        entry: usize,
        project: String,
        source: crate::json::JsonError,
    },
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => write!(f, "exceeds {MAX_REGISTRY_MIB} MiB"),
            Self::Io(error) => write!(f, "{error}"),
            Self::Syntax(error) => write!(f, "{error}"),
            Self::Invalid {
                entry: None,
                reason,
            } => f.write_str(reason),
            Self::Invalid {
                entry: Some(index),
                reason,
            } => write!(f, "identities[{index}]: {reason}"),
            Self::PublicKey { entry, source } => {
                write!(f, "identities[{entry}]: {source}")
            }
            Self::Project {
                entry,
                project,
                source,
            } => write!(
                f,
                "identities[{entry}]: project {project:?}: {}",
                source.message
            ),
        }
    }
}

impl std::error::Error for RegistryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Syntax(error) => Some(error),
            Self::PublicKey { source, .. } => Some(source),
            Self::Project { source, .. } => Some(source),
            Self::TooLarge | Self::Invalid { .. } => None,
        }
    }
}

/// Why the identity document could not be loaded, with the setting that supplied it:
/// `identity_file` for a file and `identities` for the inline document.
#[derive(Debug)]
pub struct IdentityError {
    form: Form,
    source: RegistryError,
}

impl IdentityError {
    fn new(document: &DocumentSource, source: RegistryError) -> Self {
        Self {
            form: document.form(),
            source,
        }
    }

    /// The setting that supplied the document.
    pub(crate) fn setting(&self) -> Setting {
        DocumentKind::Identities.setting(self.form)
    }
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let setting = self.setting().as_str();
        match &self.source {
            RegistryError::TooLarge => write!(f, "{setting} exceeds {MAX_REGISTRY_MIB} MiB"),
            source => write!(f, "{setting}: {source}"),
        }
    }
}

impl std::error::Error for IdentityError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Local principal registry loaded from the strict-mode identity document.
#[derive(Debug)]
pub struct Registry {
    identities: Vec<Identity>,
    /// Identities the file declares; the ready event counts these, not the built-in signer.
    declared: usize,
    /// Authentication resolves a signer per request, so it must not walk the identities.
    ids: HashMap<String, usize>,
    tokens: HashMap<String, usize>,
    signing_keys: SigningKeys,
    public_access_prevention_projects: Vec<String>,
    /// The identity that requests without credentials on the trusted listener act as.
    trusted: Option<usize>,
}

/// The identity document's bytes. Inline text is held to the same limit as a file, so both forms
/// accept the same documents.
fn read_identities(source: &DocumentSource) -> Result<Cow<'_, [u8]>, RegistryError> {
    let bytes = match source {
        DocumentSource::File(path) => crate::bounded_file::read(path, MAX_REGISTRY_BYTES)
            .map(Cow::Owned)
            .map_err(|error| match error {
                crate::bounded_file::ReadError::Limit => RegistryError::TooLarge,
                crate::bounded_file::ReadError::Io(error) => RegistryError::Io(error),
            })?,
        DocumentSource::Inline(text) => Cow::Borrowed(text.as_bytes()),
    };
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_REGISTRY_BYTES) {
        return Err(RegistryError::TooLarge);
    }
    Ok(bytes)
}

impl Registry {
    /// Strict mode's principals, with the built-in test signer when `signer` settles to enabled.
    fn load_principals(
        source: &DocumentSource,
        signer: TestSignerChoice,
    ) -> Result<(Self, TestSigner), IdentityError> {
        let load = || {
            let mut registry = Self::parse(&read_identities(source)?)?;
            let signer = signer.settle(|| registry.ids.contains_key(test_signer::ID));
            match signer {
                TestSigner::Enabled => registry.add_test_signer()?,
                TestSigner::Disabled => {}
            }
            Ok((registry, signer))
        };
        load().map_err(|error| IdentityError::new(source, error))
    }

    /// The keys permissive verification accepts: an optional identity document's and the
    /// built-in test signer's when `signer` settles to enabled.
    fn load_signing_keys(
        source: Option<&DocumentSource>,
        signer: TestSignerChoice,
    ) -> Result<(SigningKeys, TestSigner), IdentityError> {
        let mut keys = match source {
            Some(source) => {
                read_identities(source)
                    .and_then(|bytes| Self::parse_for(&bytes, IdentityUse::SigningKeys))
                    .map_err(|error| IdentityError::new(source, error))?
                    .signing_keys
            }
            None => SigningKeys::default(),
        };
        let signer = signer.settle(|| keys.contains(test_signer::ID));
        match signer {
            TestSigner::Enabled => keys.add(test_signer::ID, test_signer::public_key()),
            TestSigner::Disabled => {}
        }
        Ok((keys, signer))
    }

    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, RegistryError> {
        Self::parse_for(bytes, IdentityUse::Principals)
    }

    fn parse_for(bytes: &[u8], usage: IdentityUse) -> Result<Self, RegistryError> {
        let error = |reason: String| RegistryError::Invalid {
            entry: None,
            reason,
        };
        let value: Value = serde_json::from_slice(bytes).map_err(RegistryError::Syntax)?;
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
        let pap_projects = top.get("public_access_prevention_projects");
        if usage == IdentityUse::SigningKeys && pap_projects.is_some() {
            return Err(error(strict_only("public_access_prevention_projects")));
        }
        let public_access_prevention_projects = parse_pap_projects(pap_projects)?;
        let entries = top
            .get("identities")
            .ok_or_else(|| error("missing required field: identities".into()))?
            .as_array()
            .ok_or_else(|| error("identities must be an array".into()))?;
        if entries.is_empty() {
            return Err(error("identities must contain at least one entry".into()));
        }
        let mut registry = Self {
            identities: Vec::with_capacity(entries.len()),
            declared: entries.len(),
            ids: HashMap::with_capacity(entries.len()),
            tokens: HashMap::new(),
            signing_keys: SigningKeys::default(),
            public_access_prevention_projects,
            trusted: None,
        };
        for (index, entry) in entries.iter().enumerate() {
            registry.add_entry(entry, index, usage)?;
        }
        acl_identity::validate_aliases(&registry.identities)?;
        Ok(registry)
    }

    /// Adds one `identities` entry after checking it against the fields `usage` accepts.
    fn add_entry(
        &mut self,
        entry: &Value,
        index: usize,
        usage: IdentityUse,
    ) -> Result<(), RegistryError> {
        let error = |reason: String| RegistryError::Invalid {
            entry: Some(index),
            reason,
        };
        let entry = entry
            .as_object()
            .ok_or_else(|| error("must be a JSON object".into()))?;
        if let Some(key) = entry.keys().find(|key| {
            !matches!(
                key.as_str(),
                "id" | "tokens" | "project_roles" | "public_keys" | "acl_entity"
            )
        }) {
            return Err(error(format!("unknown field: {key}")));
        }
        if let Some(field) = usage.rejected_field(entry) {
            return Err(error(strict_only(field)));
        }
        let id = entry
            .get("id")
            .ok_or_else(|| error("missing required field: id".into()))?
            .as_str()
            .ok_or_else(|| error("id must be a string".into()))?;
        if !valid_principal_id(id) {
            return Err(error(format!(
                "id must be 1..={MAX_ID_BYTES} bytes of printable ASCII without whitespace"
            )));
        }
        if id.starts_with(crate::object::owner::RESERVED_ID_PREFIX) {
            return Err(error(format!(
                "id must not start with {}; that namespace is reserved for emulator-internal principals",
                crate::object::owner::RESERVED_ID_PREFIX
            )));
        }
        if self.ids.insert(id.to_owned(), index).is_some() {
            return Err(error(format!("duplicate id: {id}")));
        }
        match (entry.get("public_keys"), usage) {
            (Some(keys), _) => self
                .signing_keys
                .insert(id, parse_public_keys(keys, index)?),
            (None, IdentityUse::SigningKeys) => {
                return Err(error(
                    "missing required field: public_keys; with auth_mode=permissive an identity file supplies only signing keys".into(),
                ));
            }
            (None, IdentityUse::Principals) => {}
        }
        let tokens: &[Value] = match (entry.get("tokens"), usage) {
            (Some(tokens), _) => tokens
                .as_array()
                .ok_or_else(|| error("tokens must be an array".into()))?,
            (None, IdentityUse::SigningKeys) => &[],
            (None, IdentityUse::Principals) => {
                return Err(error("missing required field: tokens".into()));
            }
        };
        self.add_tokens(tokens, index)?;
        let permissions = match entry.get("project_roles") {
            Some(projects) => parse_project_roles(projects, index)?,
            None => BTreeMap::new(),
        };
        self.identities.push(Identity {
            id: id.to_owned(),
            acl_entity: acl_identity::parse_entity(entry.get("acl_entity"), index)?,
            projects: permissions.into(),
        });
        Ok(())
    }

    fn add_tokens(&mut self, tokens: &[Value], index: usize) -> Result<(), RegistryError> {
        let error = |reason: String| RegistryError::Invalid {
            entry: Some(index),
            reason,
        };
        for token in tokens {
            let token = token
                .as_str()
                .filter(|token| visible_ascii(token, MAX_TOKEN_BYTES))
                .ok_or_else(|| {
                    error(format!(
                        "each token must be 1..={MAX_TOKEN_BYTES} bytes of visible ASCII"
                    ))
                })?;
            // Never echo token values into startup errors or logs.
            if jwt_shaped(token) {
                return Err(error(
                    "tokens must not be JWT-shaped (three dot-separated segments)".into(),
                ));
            }
            if self.tokens.insert(token.to_owned(), index).is_some() {
                return Err(error("duplicate token".into()));
            }
        }
        Ok(())
    }

    /// Adds the built-in test signer's public key. An entry the file declares with its ID keeps
    /// its tokens, project roles and ACL entity; otherwise the signer exists without project
    /// roles, so only bucket or object grants authorize it.
    fn add_test_signer(&mut self) -> Result<(), RegistryError> {
        let id = test_signer::ID;
        if !self.ids.contains_key(id) {
            self.ids.insert(id.to_owned(), self.identities.len());
            self.identities.push(Identity {
                id: id.to_owned(),
                acl_entity: None,
                projects: Arc::default(),
            });
            // The file's own aliases were already validated, so a collision involves this signer.
            acl_identity::validate_aliases(&self.identities).map_err(|_| RegistryError::Invalid {
                entry: None,
                reason: format!(
                    "another identity's id or acl_entity names the built-in test signer {id}; declare {id} itself or set test_signer=disabled"
                ),
            })?;
        }
        self.signing_keys.add(id, test_signer::public_key());
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.identities.len()
    }

    /// Who a request that presents no credentials acts as on `listener`.
    fn caller_without_credentials(&self, listener: ListenerRole) -> Caller<'_> {
        let trusted = match listener {
            ListenerRole::Public => None,
            ListenerRole::Trusted => self.trusted.and_then(|index| self.identities.get(index)),
        };
        // A trusted listener without a principal stays anonymous, so a missing setting can only
        // take access away.
        trusted.map_or(Caller::Anonymous, Caller::Principal)
    }

    fn identity(&self, id: &str) -> Option<&Identity> {
        self.identities.get(*self.ids.get(id)?)
    }
}

fn parse_pap_projects(value: Option<&Value>) -> Result<Vec<String>, RegistryError> {
    let error = |reason: String| RegistryError::Invalid {
        entry: None,
        reason,
    };
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let projects = value.as_array().ok_or_else(|| {
        error("public_access_prevention_projects must be an array of project identifiers".into())
    })?;
    let mut seen = BTreeSet::new();
    let mut public_access_prevention_projects = Vec::with_capacity(projects.len());
    for project in projects {
        let project = project.as_str().ok_or_else(|| {
            error("public_access_prevention_projects entries must be strings".into())
        })?;
        crate::json::validate_project(project)
            .map_err(|_| error("invalid public_access_prevention_projects entry".into()))?;
        if !seen.insert(project) {
            return Err(error(
                "duplicate public_access_prevention_projects entry".into(),
            ));
        }
        public_access_prevention_projects.push(project.to_owned());
    }
    Ok(public_access_prevention_projects)
}

fn parse_public_keys(
    keys: &Value,
    index: usize,
) -> Result<Vec<signature::PublicKey>, RegistryError> {
    let error = |reason: String| RegistryError::Invalid {
        entry: Some(index),
        reason,
    };
    let keys = keys
        .as_array()
        .filter(|keys| !keys.is_empty())
        .ok_or_else(|| {
            error("public_keys must be a nonempty array of inline public PEM strings".into())
        })?;
    keys.iter()
        .map(|key| {
            signature::PublicKey::parse(
                key.as_str()
                    .ok_or_else(|| error("public_keys entries must be strings".into()))?,
            )
            .map_err(|source| RegistryError::PublicKey {
                entry: index,
                source,
            })
        })
        .collect()
}

/// Expands an identity's `project_roles` into the permissions granted per project.
fn parse_project_roles(
    projects: &Value,
    index: usize,
) -> Result<BTreeMap<String, ProjectGrant>, RegistryError> {
    let error = |reason: String| RegistryError::Invalid {
        entry: Some(index),
        reason,
    };
    let mut permissions = BTreeMap::new();
    let projects = projects
        .as_object()
        .ok_or_else(|| error("project_roles must be a JSON object".into()))?;
    for (project, role_values) in projects {
        crate::json::validate_project(project).map_err(|source| RegistryError::Project {
            entry: index,
            project: project.clone(),
            source,
        })?;
        let role_values = role_values
            .as_array()
            .filter(|roles| !roles.is_empty())
            .ok_or_else(|| {
                error(format!(
                    "project {project:?}: roles must be a nonempty array of role names"
                ))
            })?;
        let mut names = BTreeSet::new();
        let granted: &mut ProjectGrant = permissions.entry(project.clone()).or_default();
        for role in role_values {
            let role = role
                .as_str()
                .ok_or_else(|| error(format!("project {project:?}: role names must be strings")))?;
            if !names.insert(role) {
                return Err(error(format!("project {project:?}: duplicate role {role}")));
            }
            let expanded = roles::permissions(role).ok_or_else(|| {
                error(format!(
                    "project {project:?}: unsupported role {role} (only pinned predefined Cloud Storage roles are available)"
                ))
            })?;
            granted.any_storage_permission |= !expanded.is_empty();
            for permission in Permission::ALL {
                if expanded.contains(&permission.as_str()) {
                    granted.grant(*permission);
                }
            }
            granted.explicit_storage_admin |= role == "roles/storage.admin";
        }
    }
    Ok(permissions)
}

fn visible_ascii(value: &str, max: usize) -> bool {
    (1..=max).contains(&value.len()) && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

pub(crate) fn valid_principal_id(value: &str) -> bool {
    visible_ascii(value, MAX_ID_BYTES)
}

/// The active profile and how it treats signed XML requests.
#[derive(Debug, Default)]
enum Profile {
    /// No permission checks, and XML signing credentials are ignored.
    #[default]
    Permissive,
    /// No permission checks, but a signed XML request must verify first (`signed_urls=verify`).
    /// A verified signature grants nothing an unsigned request lacks.
    VerifyingPermissive(SigningKeys),
    /// Registered principals and their grants authorize every request.
    Strict(Registry),
}

/// The active authentication profile. The default is permissive.
#[derive(Debug)]
pub struct Auth {
    profile: Profile,
    verifiers: signature::Verifiers,
    /// Whether V4 verification also accepts a canonical host without the request's port.
    signed_host_port: signature::SignedHostPort,
    /// Whether the built-in test signer exists, as loading settled it.
    test_signer: TestSigner,
    /// Permissive mode only: the ACL entity (`user-EMAIL`) recorded as creator of new objects.
    /// It never authorizes anything.
    local_creator_entity: Option<String>,
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            profile: Profile::default(),
            verifiers: signature::Verifiers::default(),
            signed_host_port: signature::SignedHostPort::default(),
            test_signer: TestSigner::Disabled,
            local_creator_entity: None,
        }
    }
}

impl Auth {
    pub fn permissive() -> Self {
        Self::default()
    }

    pub fn strict(registry: Registry) -> Self {
        Self::with_profile(Profile::Strict(registry))
    }

    fn with_profile(profile: Profile) -> Self {
        Self {
            profile,
            ..Self::default()
        }
    }

    /// Loads the profile that configuration validation selected, reading its identity document
    /// once and adding the built-in test signer when the profile's choice settles to enabled.
    /// Validation owns the pairing rules and selects that choice; only the document's contents
    /// can fail here, and only whether it declares the signer can settle the choice.
    pub fn load(profile: &AuthProfile) -> Result<Self, IdentityError> {
        let (profile, test_signer, verification) = match profile {
            AuthProfile::Permissive => return Ok(Self::permissive()),
            AuthProfile::VerifyingPermissive {
                identities,
                verification,
            } => {
                let (keys, signer) =
                    Registry::load_signing_keys(identities.as_ref(), verification.test_signer)?;
                (Profile::VerifyingPermissive(keys), signer, verification)
            }
            AuthProfile::Strict {
                identities,
                verification,
            } => {
                let (registry, signer) =
                    Registry::load_principals(identities, verification.test_signer)?;
                (Profile::Strict(registry), signer, verification)
            }
        };
        Ok(Self {
            signed_host_port: signature::SignedHostPort::resolve(
                verification.signed_host_port,
                test_signer,
            ),
            test_signer,
            ..Self::with_profile(profile)
        })
    }

    /// Whether the built-in test signer exists, as loading settled it.
    pub fn test_signer(&self) -> TestSigner {
        self.test_signer
    }

    /// Whether V4 verification also accepts a canonical host without the request's port, after
    /// the default that the settled test signer selects.
    pub fn signed_host_port(&self) -> signature::SignedHostPort {
        self.signed_host_port
    }

    /// Permissive verification with an identity file's keys and the built-in test signer.
    #[cfg(test)]
    pub(crate) fn verifying_permissive(identity_file: &[u8]) -> Self {
        let mut keys = Registry::parse_for(identity_file, IdentityUse::SigningKeys)
            .unwrap()
            .signing_keys;
        keys.add(test_signer::ID, test_signer::public_key());
        Self {
            test_signer: TestSigner::Enabled,
            ..Self::with_profile(Profile::VerifyingPermissive(keys))
        }
    }

    fn registry(&self) -> Option<&Registry> {
        match &self.profile {
            Profile::Strict(registry) => Some(registry),
            Profile::Permissive | Profile::VerifyingPermissive(_) => None,
        }
    }

    /// Whether signed XML requests verify before they proceed.
    pub fn signed_urls(&self) -> SignedUrls {
        match self.profile {
            Profile::Permissive => SignedUrls::Ignore,
            Profile::VerifyingPermissive(_) | Profile::Strict(_) => SignedUrls::Verify,
        }
    }

    /// Selects whether V4 verification also accepts a signature whose canonical host omits the
    /// port that the request Host names; [`Auth::load`] sets it from the configuration.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_signed_host_port(
        mut self,
        signed_host_port: signature::SignedHostPort,
    ) -> Self {
        self.signed_host_port = signed_host_port;
        self
    }

    /// Reserve half the runtime's blocking slots for cryptography; other work shares the pool.
    pub fn with_verifier_budget(mut self, max_blocking_threads: usize) -> Self {
        self.verifiers = signature::Verifiers::for_blocking_threads(max_blocking_threads);
        self
    }

    /// Records `email` as the creator of objects written in permissive mode. Strict mode ignores
    /// it because its callers are registered principals.
    pub fn with_local_creator_email(mut self, email: &str) -> Self {
        self.local_creator_entity = (!email.is_empty()).then(|| format!("user-{email}"));
        self
    }

    pub(crate) fn local_creator_entity(&self) -> Option<&str> {
        self.local_creator_entity.as_deref()
    }

    /// Makes requests without credentials on the trusted listener act as the registered identity
    /// `id`, which may be the built-in test signer when it is enabled.
    pub fn trust_principal(&mut self, id: &str) -> Result<(), TrustedPrincipalError> {
        let Profile::Strict(registry) = &mut self.profile else {
            return Err(TrustedPrincipalError::RequiresStrict);
        };
        let index = registry
            .ids
            .get(id)
            .copied()
            .ok_or(TrustedPrincipalError::Unknown)?;
        registry.trusted = Some(index);
        Ok(())
    }

    pub fn mode(&self) -> AuthMode {
        match self.profile {
            Profile::Permissive | Profile::VerifyingPermissive(_) => AuthMode::Permissive,
            Profile::Strict(_) => AuthMode::Strict,
        }
    }

    /// The identities the identity file declares; the built-in test signer is not counted.
    pub fn identity_count(&self) -> Option<usize> {
        self.registry().map(|registry| registry.declared)
    }

    pub fn principals(&self) -> Option<&[Identity]> {
        self.registry()
            .map(|registry| registry.identities.as_slice())
    }

    /// Local simulation of the effective parent organization policy, loaded before readiness.
    pub fn public_access_prevention_projects(&self) -> &[String] {
        self.registry()
            .map_or(&[], |registry| &registry.public_access_prevention_projects)
    }

    /// Resolve the caller from request metadata only; never reads a body or the catalog.
    ///
    /// Returns `Ok(None)` in permissive mode, where no permission checks apply.
    #[cfg(test)]
    pub(crate) async fn authenticate(
        &self,
        headers: &HeaderMap,
        query: Option<&str>,
    ) -> Result<Option<Caller<'_>>, AuthFailure> {
        self.authenticate_on(ListenerRole::Public, headers, query)
            .await
    }

    /// [`Self::authenticate`] for a request that arrived on a `listener`.
    #[cfg(test)]
    pub(crate) async fn authenticate_on(
        &self,
        listener: ListenerRole,
        headers: &HeaderMap,
        query: Option<&str>,
    ) -> Result<Option<Caller<'_>>, AuthFailure> {
        match self
            .authenticate_request(headers, query, &crate::clock::SystemClock, listener)
            .await
        {
            Ok(caller) => Ok(caller),
            Err(AuthError::Failure(failure)) => Err(failure),
            Err(AuthError::Unavailable(error)) => panic!("credential verifier failed: {error}"),
        }
    }

    /// The one place a listener's role affects authentication: only a request that presents no
    /// credentials at all changes, so invalid credentials fail on every listener.
    pub(crate) async fn authenticate_request(
        &self,
        headers: &HeaderMap,
        query: Option<&str>,
        clock: &dyn crate::clock::Clock,
        listener: ListenerRole,
    ) -> Result<Option<Caller<'_>>, AuthError> {
        let Some(registry) = self.registry() else {
            return Ok(None);
        };
        let identity = match Self::presented(registry, headers, query, clock)? {
            Presented::Anonymous => return Ok(Some(registry.caller_without_credentials(listener))),
            Presented::Token(identity) => Some(identity),
            Presented::Jwt { candidate, warn } => {
                let identity = candidate.verify(&self.verifiers).await?;
                if identity.is_none() && warn {
                    warn_self_signed_jwt();
                }
                identity
            }
        };
        identity
            .map(|identity| Some(Caller::Principal(identity)))
            .ok_or_else(|| AuthFailure::InvalidCredentials.into())
    }

    fn presented<'a>(
        registry: &'a Registry,
        headers: &HeaderMap,
        query: Option<&str>,
        clock: &dyn crate::clock::Clock,
    ) -> Result<Presented<'a>, AuthFailure> {
        if query.is_some_and(has_credential_parameter) {
            return Err(AuthFailure::InvalidCredentials);
        }
        let mut values = headers.get_all(AUTHORIZATION).iter();
        let Some(value) = values.next() else {
            return Ok(Presented::Anonymous);
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
        if jwt_shaped(token) {
            // A registered token must never bypass verification of JWT-shaped credentials.
            let warn = looks_like_jwt(token);
            jwt::Candidate::parse(registry, token, clock)
                .map(|candidate| Presented::Jwt { candidate, warn })
                .ok_or_else(|| {
                    if warn {
                        warn_self_signed_jwt();
                    }
                    AuthFailure::InvalidCredentials
                })
        } else {
            // Randomized SipHash indexes local test tokens; HMAC verification uses ring's constant-time check.
            registry
                .tokens
                .get(token)
                .and_then(|index| registry.identities.get(*index))
                .map(Presented::Token)
                .ok_or(AuthFailure::InvalidCredentials)
        }
    }
}

fn warn_self_signed_jwt() {
    if !WARNED_SELF_SIGNED_JWT.swap(true, Ordering::Relaxed) {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Warn,
            "auth_self_signed_jwt_rejected",
            serde_json::json!({
                "message": "strict authentication rejected a self-signed JWT; register its issuer and RSA public_keys in the identity file, use RS256 with matching issuer/subject, devstorage.full_control scope, no audience and a valid lifetime of at most one hour; the permissive profile remains available for unauthenticated local clients",
            }),
        );
    }
}

fn jwt_shaped(token: &str) -> bool {
    token.split('.').count() == 3
}

/// A JWT is three base64url segments; its header is JSON, which encodes to a leading `eyJ`.
fn looks_like_jwt(token: &str) -> bool {
    let mut segments = token.split('.');
    segments
        .next()
        .is_some_and(|header| header.starts_with("eyJ"))
        && segments.next().is_some_and(|payload| !payload.is_empty())
        && segments
            .next()
            .is_some_and(|signature| !signature.is_empty())
        && segments.next().is_none()
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
    let mut bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    while let Some((&first, tail)) = bytes.split_first() {
        if first == b'%'
            && let Some(byte) = tail.first_chunk::<2>().and_then(crate::hex::byte)
        {
            decoded.push(byte);
            bytes = tail.get(2..).unwrap_or_default();
            continue;
        }
        decoded.push(if first == b'+' { b' ' } else { first });
        bytes = tail;
    }
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_fixtures::{PERMISSIVE, live, permissive_upload};
    use crate::test_support::TestDir;

    #[test]
    fn compact_grants_keep_project_membership_and_legacy_upload_snapshots() {
        let registry = Registry::parse(br#"{"identities":[{"id":"hmac","tokens":[],"project_roles":{"project":["roles/storage.hmacKeyAdmin"]}}]}"#).unwrap();
        let identity = &registry.identities[0];
        assert!(identity.has_project_grant("project"));
        assert!(!identity.allows(OBJECTS_GET, "project"));
        let restored = Identity::from_upload_snapshot(
            &serde_json::json!({"id":"legacy","projects":{"project":["storage.objects.create"]}}),
        )
        .unwrap();
        assert!(restored.allows(OBJECTS_CREATE, "project"));
        assert!(!restored.allows(OBJECTS_DELETE, "project"));
        assert!(!restored.has_explicit_storage_admin("project"));
        assert_eq!(
            restored.upload_snapshot(),
            serde_json::json!({"id":"legacy","projects":{"project":["storage.objects.create"]}})
        );
        assert!(Permission::parse_testable(BUCKETS_ENABLE_OBJECT_RETENTION.as_str()).is_none());
        assert_eq!(
            BUCKETS_ENABLE_OBJECT_RETENTION,
            Permission::BucketsEnableObjectRetention
        );
    }

    #[test]
    fn compact_grants_preserve_hmac_key_management_permissions() {
        let registry = Registry::parse(br#"{"identities":[{"id":"hmac","tokens":[],"project_roles":{"project":["roles/storage.hmacKeyAdmin"]}}]}"#).unwrap();
        let identity = &registry.identities[0];
        for permission in [
            Permission::HmacKeysCreate,
            Permission::HmacKeysDelete,
            Permission::HmacKeysGet,
            Permission::HmacKeysList,
            Permission::HmacKeysUpdate,
        ] {
            assert!(identity.allows(permission, "project"), "{permission}");
            assert!(Permission::parse_testable(permission.as_str()).is_none());
            let query = format!("permissions={permission}");
            let error = crate::json::test_permissions_query(Some(&query)).unwrap_err();
            assert_eq!(error.status, hyper::StatusCode::BAD_REQUEST);
            assert_eq!(error.reason, "badRequest");
            let object_uri = format!("/storage/v1/b/bucket/o/object/iam/testPermissions?{query}")
                .parse()
                .unwrap();
            assert!(crate::json::route_object(&hyper::Method::GET, &object_uri).is_none());
        }
    }

    #[test]
    fn jwt_detection_requires_three_segments_and_a_json_header() {
        assert!(looks_like_jwt("eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJ4In0.c2ln"));
        for token in [
            "admin-token",
            "ya29.opaque-oauth-access-token",
            "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJ4In0",
            "eyJhbGciOiJSUzI1NiJ9..c2ln",
            "eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJ4In0.c2ln.extra",
            "abc.def.ghi",
            "",
        ] {
            assert!(!looks_like_jwt(token), "{token}");
        }
    }

    #[test]
    fn pap_parent_fixture_rejects_malformed_and_repeated_projects() {
        for projects in [
            serde_json::json!(null),
            serde_json::json!(true),
            serde_json::json!({}),
            serde_json::json!([1]),
            serde_json::json!([""]),
            serde_json::json!(["has space"]),
            serde_json::json!(["p", "p"]),
        ] {
            let value = serde_json::json!({"identities":[{"id":"local","tokens":["token"]}],"public_access_prevention_projects":projects});
            assert!(
                Registry::parse(value.to_string().as_bytes())
                    .unwrap_err()
                    .to_string()
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
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let record = crate::json::parse_insert(
            br#"{"name":"pap-permissions"}"#,
            "project",
            std::time::UNIX_EPOCH,
        )
        .unwrap();
        let record = catalog.insert_bucket(record).await.unwrap();
        let identity = Identity {
            id: "local".into(),
            acl_entity: None,
            projects: BTreeMap::from([("project".into(), [BUCKETS_UPDATE].into_iter().collect())])
                .into(),
        };
        let access = BucketAccess::Policy {
            identity: Some(identity),
            permission: crate::auth::Permission::BucketsUpdate,
            project: Some("project".into()),
        };
        for replace in [false, true] {
            let update = BucketMetadataUpdate {
                public_access_prevention_enforced: Some(true),
                ..Default::default()
            };
            let result = if replace {
                catalog
                    .replace_bucket("pap-permissions", update, Default::default(), &access)
                    .await
            } else {
                catalog
                    .patch_bucket("pap-permissions", update, Default::default(), &access)
                    .await
            };
            assert_eq!(result, Err(CatalogError::Forbidden));
            assert_eq!(
                catalog
                    .get_bucket("pap-permissions", PERMISSIVE)
                    .await
                    .unwrap(),
                Some(record.clone())
            );
        }
        for enforced in [false, true] {
            catalog
                .patch_bucket(
                    "pap-permissions",
                    BucketMetadataUpdate {
                        public_access_prevention_enforced: Some(enforced),
                        ..Default::default()
                    },
                    Default::default(),
                    PERMISSIVE,
                )
                .await
                .unwrap();
            for replace in [false, true] {
                for value in [None, Some(enforced), Some(!enforced)] {
                    let before = catalog
                        .get_bucket("pap-permissions", PERMISSIVE)
                        .await
                        .unwrap()
                        .unwrap();
                    let update = BucketMetadataUpdate {
                        public_access_prevention_enforced: value,
                        labels: Some(Some(BTreeMap::from([(
                            "team".into(),
                            Some("storage".into()),
                        )]))),
                        ..Default::default()
                    };
                    let result = if replace {
                        catalog
                            .replace_bucket("pap-permissions", update, Default::default(), &access)
                            .await
                    } else {
                        catalog
                            .patch_bucket("pap-permissions", update, Default::default(), &access)
                            .await
                    };
                    let expected = value.unwrap_or(enforced);
                    if expected == enforced {
                        let after = result.unwrap();
                        assert_eq!(after.public_access_prevention_enforced, enforced);
                        assert_eq!(after.labels["team"], "storage");
                        assert_eq!(after.metageneration, before.metageneration + 1);
                    } else {
                        assert_eq!(result, Err(CatalogError::Forbidden));
                        assert_eq!(
                            catalog
                                .get_bucket("pap-permissions", PERMISSIVE)
                                .await
                                .unwrap(),
                            Some(before)
                        );
                    }
                }
            }
        }
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn resource_acl_echo_requires_read_permission_and_changes_require_set_permission() {
        use crate::{
            bucket::{BucketAccess, BucketMetadataUpdate, CatalogError},
            storage::Catalog,
        };
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let record = crate::json::parse_insert(
            br#"{"name":"acl-permissions"}"#,
            "project",
            std::time::UNIX_EPOCH,
        )
        .unwrap();
        catalog.insert_bucket(record).await.unwrap();
        for default_acl in [false, true] {
            for readable in [false, true] {
                let mut permissions = vec![BUCKETS_UPDATE];
                if readable {
                    permissions.push(BUCKETS_GET_IAM);
                }
                let access = BucketAccess::Policy {
                    identity: Some(Identity {
                        id: "local".into(),
                        acl_entity: None,
                        projects: BTreeMap::from([(
                            "project".into(),
                            permissions.into_iter().collect(),
                        )])
                        .into(),
                    }),
                    permission: Permission::BucketsUpdate,
                    project: Some("project".into()),
                };
                for replace in [false, true] {
                    for same in [false, true] {
                        let before = catalog
                            .get_bucket("acl-permissions", PERMISSIVE)
                            .await
                            .unwrap()
                            .unwrap();
                        let mut entries = crate::acl::project_private("project");
                        entries.reverse();
                        if !same {
                            entries[0].role = crate::acl::AclRole::Owner;
                        }
                        let mut update = BucketMetadataUpdate {
                            labels: Some(Some(BTreeMap::from([(
                                "changed".into(),
                                Some("yes".into()),
                            )]))),
                            ..Default::default()
                        };
                        if default_acl {
                            update.default_object_acl = Some(entries);
                        } else {
                            update.acl = Some(entries);
                        }
                        let conditions = Default::default();
                        let result = if replace {
                            catalog
                                .replace_bucket("acl-permissions", update, conditions, &access)
                                .await
                        } else {
                            catalog
                                .patch_bucket("acl-permissions", update, conditions, &access)
                                .await
                        };
                        if readable && same {
                            let after = result.unwrap();
                            assert_eq!(after.metageneration, before.metageneration + 1);
                            assert_eq!(after.labels["changed"], "yes");
                        } else {
                            assert_eq!(result, Err(CatalogError::Forbidden));
                            assert_eq!(
                                catalog
                                    .get_bucket("acl-permissions", PERMISSIVE)
                                    .await
                                    .unwrap()
                                    .unwrap(),
                                before
                            );
                        }
                    }
                }
            }
        }
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn object_resource_acl_echo_and_replacement_keep_read_and_write_permissions_separate() {
        use crate::{
            object::UploadChunk,
            server::{State, serve},
            storage::Catalog,
            test_support::TestDir,
        };
        use std::sync::Arc;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        async fn request(
            address: std::net::SocketAddr,
            method: &str,
            body: Value,
            token: &str,
        ) -> (u16, Value) {
            let body = body.to_string();
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream.write_all(format!("{method} /storage/v1/b/acl-permissions/o/object?projection=full HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.unwrap();
            let response = std::str::from_utf8(&bytes).unwrap();
            let (head, body) = response.split_once("\r\n\r\n").unwrap();
            (
                head.split_whitespace().nth(1).unwrap().parse().unwrap(),
                serde_json::from_str(body).unwrap(),
            )
        }

        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog
            .insert_bucket(
                crate::json::parse_insert(
                    br#"{"name":"acl-permissions"}"#,
                    "project",
                    std::time::UNIX_EPOCH,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let (sender, body) = tokio::sync::mpsc::channel(2);
        sender
            .send(Ok(UploadChunk::Data(bytes::Bytes::from_static(
                b"original",
            ))))
            .await
            .unwrap();
        sender.send(Ok(UploadChunk::End)).await.unwrap();
        drop(sender);
        catalog
            .put_object(permissive_upload("acl-permissions", "object", 100), body)
            .await
            .unwrap();
        let mut registry = Registry::parse(br#"{"identities":[{"id":"reader","tokens":["reader"]},{"id":"writer","tokens":["writer"]}]}"#).unwrap();
        Arc::make_mut(&mut registry.identities[0].projects).insert(
            "project".into(),
            [OBJECTS_UPDATE, OBJECTS_GET_IAM].into_iter().collect(),
        );
        Arc::make_mut(&mut registry.identities[1].projects).insert(
            "project".into(),
            [OBJECTS_UPDATE, OBJECTS_SET_IAM].into_iter().collect(),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(State {
            api_base_url: format!("http://{address}"),
            ..State::for_test(catalog.clone(), Auth::strict(registry))
        });
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let running = tokio::spawn(serve(
            listener,
            crate::config::Config {
                scheme: crate::config::Scheme::Http,
                ..Default::default()
            },
            state,
            async {
                stopped.await.unwrap();
                Ok(())
            },
        ));
        let target = live("acl-permissions", "object");
        for method in ["PATCH", "PUT"] {
            let before = catalog
                .get_object(target.clone(), Default::default(), PERMISSIVE)
                .await
                .unwrap();
            let mut acl = crate::acl::project_private("project");
            acl.reverse();
            let (status, response) = request(
                address,
                method,
                serde_json::json!({"acl":acl,"contentType":"text/plain"}),
                "reader",
            )
            .await;
            assert_eq!(status, 200, "{response}");
            assert_eq!(
                response["metageneration"],
                (before.metageneration + 1).to_string()
            );
            assert_eq!(response["acl"].as_array().unwrap().len(), 3);
            let unchanged = catalog
                .get_object(target.clone(), Default::default(), PERMISSIVE)
                .await
                .unwrap();
            let (status, response) = request(address, method, serde_json::json!({"acl":[{"entity":"allUsers","role":"READER"}],"contentType":"text/rejected"}), "reader").await;
            assert_eq!(status, 403, "{response}");
            assert_eq!(
                catalog
                    .get_object(target.clone(), Default::default(), PERMISSIVE)
                    .await
                    .unwrap(),
                unchanged
            );
        }
        let replacement = serde_json::json!({"acl":[{"entity":"user-new@example.test","role":"OWNER"}],"contentType":"text/replaced"});
        for method in ["PATCH", "PUT"] {
            let (status, response) = request(address, method, replacement.clone(), "writer").await;
            assert_eq!(status, 200, "{response}");
            assert_eq!(response["contentType"], "text/replaced");
            assert!(response.get("acl").is_none());
            let acl = catalog
                .read_acl(
                    crate::acl::AclTarget::Object {
                        bucket: "acl-permissions".into(),
                        name: "object".into(),
                        generation: None,
                    },
                    Default::default(),
                    PERMISSIVE,
                )
                .await
                .unwrap();
            let mut entries = acl.entries;
            entries.sort_by(|a, b| a.entity.cmp(&b.entity));
            assert_eq!(
                entries,
                vec![
                    crate::acl::AclEntry {
                        entity: "project-owners-0".into(),
                        role: crate::acl::AclRole::Owner
                    },
                    crate::acl::AclEntry {
                        entity: "user-new@example.test".into(),
                        role: crate::acl::AclRole::Owner
                    },
                ]
            );
        }
        stop.send(()).unwrap();
        running.await.unwrap().unwrap();
        catalog.close().await.unwrap();
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
        let project_roles: serde_json::Map<_, _> = roles::PREDEFINED_ROLES
            .iter()
            .zip(&projects)
            .map(|((role, _, _), project)| (project.clone(), serde_json::json!([role])))
            .collect();
        let registry = Registry::parse(
            serde_json::json!({ "identities": [{
                "id": "user@example.invalid",
                "tokens": ["token"],
                "project_roles": project_roles,
            }]})
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        let identity = &registry.identities[0];
        for (index, (_, _, permissions)) in roles::PREDEFINED_ROLES.iter().enumerate() {
            for permission in Permission::ALL {
                assert_eq!(
                    identity.allows(*permission, &projects[index]),
                    permissions.contains(&permission.as_str()),
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
        let has = |role: &str, permission: Permission| {
            roles::permissions(role)
                .unwrap()
                .contains(&permission.as_str())
        };
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

    #[test]
    fn sync_authority_requires_the_explicit_project_admin_role_and_is_not_in_upload_snapshots() {
        let registry=Registry::parse(br#"{"identities":[{"id":"admin","tokens":["admin-token"],"project_roles":{"owned":["roles/storage.admin"],"other":["roles/storage.objectAdmin","roles/storage.objectViewer"]}},{"id":"reader","tokens":["reader-token"],"project_roles":{"owned":["roles/storage.objectViewer"]}}]}"#).unwrap();
        let admin = &registry.identities[0];
        assert!(admin.has_explicit_storage_admin("owned"));
        assert!(!admin.has_explicit_storage_admin("other"));
        assert!(!admin.has_explicit_storage_admin("missing"));
        assert!(admin.allows(OBJECTS_DELETE, "owned") && admin.allows(OBJECTS_DELETE, "other"));
        assert!(!registry.identities[1].has_explicit_storage_admin("owned"));
        let restored = Identity::from_upload_snapshot(&admin.upload_snapshot()).unwrap();
        assert_eq!(restored.id(), admin.id());
        assert!(restored.allows(OBJECTS_CREATE, "owned"));
        assert!(!restored.has_explicit_storage_admin("owned"));
        let mut forged = admin.upload_snapshot();
        forged["projects"]["owned"] = serde_json::json!(["roles/storage.admin"]);
        assert!(Identity::from_upload_snapshot(&forged).is_err());
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

    #[tokio::test]
    async fn strict_caller_resolution() {
        let auth = strict();
        let id = |result: Result<Option<Caller<'_>>, AuthFailure>| match result {
            Ok(Some(Caller::Principal(identity))) => Ok(Some(identity.id().to_owned())),
            Ok(Some(Caller::Anonymous)) => Ok(None),
            Ok(None) => panic!("strict mode must resolve a caller"),
            Err(error) => Err(error),
        };
        assert_eq!(id(auth.authenticate(&headers(&[]), None).await), Ok(None));
        for value in [&b"Bearer tok-a"[..], b"bearer tok-b", b"BEARER   tok-a"] {
            assert_eq!(
                id(auth
                    .authenticate(&headers(&[value]), Some("project=p"))
                    .await),
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
                id(auth.authenticate(&headers(&[value]), None).await),
                Err(AuthFailure::InvalidCredentials),
                "{}",
                String::from_utf8_lossy(value)
            );
        }
        assert_eq!(
            id(auth
                .authenticate(&headers(&[b"Bearer tok-a", b"Bearer tok-a"]), None)
                .await),
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
            "X-Amz-Algorithm=AWS4-HMAC-SHA256",
            "x-amz-credential=local",
            "X-Amz-Date=20261031T000000Z",
            "X-Amz-Expires=60",
            "X-Amz-SignedHeaders=host",
            "X-Amz-Signature=00",
        ] {
            assert_eq!(
                id(auth
                    .authenticate(&headers(&[b"Bearer tok-a"]), Some(query))
                    .await),
                Err(AuthFailure::InvalidCredentials),
                "{query}"
            );
            assert_eq!(
                id(auth.authenticate(&headers(&[]), Some(query)).await),
                Err(AuthFailure::InvalidCredentials),
                "{query}"
            );
        }
        let permissive = Auth::permissive();
        assert!(matches!(
            permissive
                .authenticate(&headers(&[b"Basic garbage", b"x"]), Some("access_token=x"))
                .await,
            Ok(None)
        ));
    }

    #[tokio::test]
    async fn registered_principal_without_tokens_does_not_gain_bearer_or_jwt_authentication() {
        let registry = Registry::parse(
            br#"{"identities":[{"id":"hmac-only@example.test","tokens":[],"project_roles":{"local":["roles/storage.admin"]}}]}"#,
        )
        .unwrap();
        assert_eq!(registry.len(), 1);
        assert!(registry.tokens.is_empty());
        assert!(registry.signing_keys.is_empty());
        let auth = Auth::strict(registry);
        let anonymous = auth
            .authenticate(&headers(&[]), None)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(anonymous, Caller::Anonymous));
        assert_eq!(anonymous.principal(), Err(AuthFailure::LoginRequired));
        for credential in [
            &b"Bearer"[..],
            b"Bearer ",
            b"Bearer hmac-only@example.test",
            b"Bearer unknown",
            b"Bearer eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJobWFjLW9ubHlAZXhhbXBsZS50ZXN0In0.AA",
        ] {
            assert!(matches!(
                auth.authenticate(&headers(&[credential]), None).await,
                Err(AuthFailure::InvalidCredentials)
            ));
        }
        assert!(matches!(
            auth.authenticate(&headers(&[]), Some("X-Goog-Credential=unknown"))
                .await,
            Err(AuthFailure::InvalidCredentials)
        ));
    }

    #[tokio::test]
    async fn the_trusted_listener_changes_only_requests_without_credentials() {
        let id = |result: Result<Option<Caller<'_>>, AuthFailure>| match result {
            Ok(Some(Caller::Principal(identity))) => Ok(Some(identity.id().to_owned())),
            Ok(Some(Caller::Anonymous)) => Ok(None),
            Ok(None) => panic!("strict mode must resolve a caller"),
            Err(error) => Err(error),
        };
        let mut registry = Registry::parse(
            br#"{"identities":[{"id":"app@example.invalid","tokens":["app-token"],"project_roles":{"p":["roles/storage.objectAdmin"]}},{"id":"other@example.invalid","tokens":["other-token"]}]}"#,
        )
        .unwrap();
        registry.add_test_signer().unwrap();
        let mut auth = Auth::strict(registry);
        let trusted = ListenerRole::Trusted;
        // Until a principal is named the trusted listener grants nothing more than the public one.
        assert_eq!(
            id(auth.authenticate_on(trusted, &headers(&[]), None).await),
            Ok(None)
        );
        assert_eq!(
            auth.trust_principal("missing@example.invalid"),
            Err(TrustedPrincipalError::Unknown)
        );
        auth.trust_principal("app@example.invalid").unwrap();
        assert_eq!(
            id(auth.authenticate_on(trusted, &headers(&[]), None).await),
            Ok(Some("app@example.invalid".into()))
        );
        assert_eq!(
            id(auth.authenticate(&headers(&[]), None).await),
            Ok(None),
            "the public listener keeps anonymous requests anonymous"
        );
        assert_eq!(
            id(auth
                .authenticate_on(trusted, &headers(&[b"Bearer other-token"]), None)
                .await),
            Ok(Some("other@example.invalid".into()))
        );
        for (values, query) in [
            (&[&b"Bearer unknown-token"[..]][..], None),
            (&[b"Bearer"], None),
            (&[b"Basic YXBwOg=="], None),
            (&[b"Bearer app-token", b"Bearer app-token"], None),
            (&[], Some("access_token=app-token")),
            (&[], Some("X-Goog-Credential=unknown")),
        ] {
            assert_eq!(
                id(auth.authenticate_on(trusted, &headers(values), query).await),
                Err(AuthFailure::InvalidCredentials),
                "{values:?} {query:?}"
            );
        }
        auth.trust_principal(test_signer::ID).unwrap();
        assert_eq!(
            id(auth.authenticate_on(trusted, &headers(&[]), None).await),
            Ok(Some(test_signer::ID.into()))
        );
        assert_eq!(
            Auth::permissive().trust_principal("app@example.invalid"),
            Err(TrustedPrincipalError::RequiresStrict)
        );
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
        let long_token = "t".repeat(MAX_TOKEN_BYTES + 1);
        for (case, document) in [
            ("malformed", "{".to_owned()),
            ("not object", "[]".into()),
            ("missing identities", "{}".into()),
            ("unknown top", r#"{"identities":[],"extra":1}"#.into()),
            ("empty", r#"{"identities":[]}"#.into()),
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
                "reserved local creator id",
                valid(r#"{"id":"gcs-mimic:local-creator","tokens":["t"]}"#),
            ),
            (
                "reserved namespace",
                valid(r#"{"id":"gcs-mimic:other","tokens":["t"]}"#),
            ),
            (
                "duplicate id",
                valid(r#"{"id":"a","tokens":["t"]},{"id":"a","tokens":["u"]}"#),
            ),
            ("missing tokens", valid(r#"{"id":"a"}"#)),
            ("empty token", valid(r#"{"id":"a","tokens":[""]}"#)),
            ("space token", valid(r#"{"id":"a","tokens":["a b"]}"#)),
            ("non-ascii token", valid(r#"{"id":"a","tokens":["tö"]}"#)),
            (
                "JWT-shaped token",
                valid(r#"{"id":"a","tokens":["a.b.c"]}"#),
            ),
            ("empty JWT segments", valid(r#"{"id":"a","tokens":[".."]}"#)),
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
                "no roles",
                valid(r#"{"id":"a","tokens":["t"],"project_roles":{"p":[]}}"#),
            ),
            (
                "no public keys",
                valid(r#"{"id":"a","tokens":["t"],"public_keys":[]}"#),
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
            // Either setting supplies the same document, and the failure names the one used.
            let dir = TestDir::new();
            let path = dir.0.join("identities.json");
            std::fs::write(&path, &document).unwrap();
            for (source, setting) in [
                (DocumentSource::File(path), "identity_file: "),
                (DocumentSource::Inline(document), "identities: "),
            ] {
                let error = Registry::load_principals(&source, TestSignerChoice::WhenDeclared)
                    .unwrap_err()
                    .to_string();
                assert!(error.starts_with(setting), "{case}: {error}");
                assert!(
                    !error.contains("\"t\""),
                    "{case}: tokens must not be echoed"
                );
            }
        }
    }

    /// Everything the file can name resolves through an index, so counts past the former limits
    /// of 256 identities, 16 tokens, 64 projects and 32 roles load and authenticate.
    #[tokio::test]
    async fn a_registry_past_the_former_count_limits_loads_and_authenticates_its_last_entry() {
        const COUNT: usize = 1000;
        let roles: Vec<_> = roles::PREDEFINED_ROLES
            .iter()
            .map(|(role, _, _)| *role)
            .collect();
        let tokens: Vec<_> = (0..COUNT).map(|n| format!("extra-{n}")).collect();
        let projects: serde_json::Map<_, _> = (0..COUNT)
            .map(|n| (format!("project-{n}"), serde_json::json!(roles)))
            .collect();
        let mut identities: Vec<_> = (0..COUNT)
            .map(|n| serde_json::json!({"id": format!("u{n}"), "tokens": [format!("t{n}")]}))
            .collect();
        identities.push(serde_json::json!({
            "id": "last", "tokens": tokens, "project_roles": projects,
        }));
        let parked: Vec<_> = (0..COUNT).map(|n| format!("parked-{n}")).collect();
        let document = serde_json::json!({
            "identities": identities, "public_access_prevention_projects": parked,
        });
        let registry = Registry::parse(document.to_string().as_bytes()).unwrap();
        assert_eq!(registry.len(), COUNT + 1);
        assert_eq!(registry.public_access_prevention_projects.len(), COUNT);
        assert_eq!(registry.identity("u999").unwrap().id(), "u999");
        assert!(registry.identity("unknown").is_none());
        let auth = Auth::strict(registry);
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer extra-999".parse().unwrap());
        let caller = auth
            .authenticate_request(
                &headers,
                None,
                &crate::clock::SystemClock,
                ListenerRole::Public,
            )
            .await
            .unwrap()
            .unwrap();
        let identity = caller.principal().unwrap();
        assert_eq!(identity.id(), "last");
        assert!(identity.allows(OBJECTS_CREATE, "project-999"));
    }

    /// Stored upload options hold at most 64 KiB, so a principal's other projects must stay out.
    #[test]
    fn an_upload_snapshot_for_a_project_keeps_only_that_project() {
        let projects: serde_json::Map<_, _> = (0..5000)
            .map(|n| {
                (
                    format!("project-{n}"),
                    serde_json::json!(["roles/storage.objectAdmin"]),
                )
            })
            .collect();
        let document =
            serde_json::json!({"identities": [{"id":"a","tokens":[],"project_roles":projects}]});
        let registry = Registry::parse(document.to_string().as_bytes()).unwrap();
        let identity = registry.identity("a").unwrap();
        assert!(identity.upload_snapshot().to_string().len() > 64 * 1024);
        let snapshot = identity.upload_snapshot_for("project-7");
        let restored = Identity::from_upload_snapshot(&snapshot).unwrap();
        assert!(restored.allows(OBJECTS_CREATE, "project-7"));
        assert!(!restored.allows(OBJECTS_CREATE, "project-8"));
        assert_eq!(
            identity.upload_snapshot_for("unlisted")["projects"],
            serde_json::json!({})
        );
        let full = Identity::from_upload_snapshot(&identity.upload_snapshot()).unwrap();
        assert!(full.allows(OBJECTS_CREATE, "project-4999"));
    }

    /// An inline document is held to the limit a file is, and each failure names its setting.
    #[test]
    fn registry_file_read_is_bounded() {
        let dir = TestDir::new();
        let path = dir.0.join("identity.json");
        let mut document = r#"{"identities":[{"id":"a","tokens":["t"]}]}"#.to_owned();
        let limit = usize::try_from(MAX_REGISTRY_BYTES).unwrap();
        document.push_str(&" ".repeat(limit - document.len()));
        let load = |source: &DocumentSource| {
            Registry::load_principals(source, TestSignerChoice::WhenDeclared)
                .map(|(registry, _)| registry)
        };
        std::fs::write(&path, &document).unwrap();
        let file = DocumentSource::File(path.clone());
        assert_eq!(load(&file).unwrap().len(), 1);
        assert_eq!(
            load(&DocumentSource::Inline(document.clone()))
                .unwrap()
                .len(),
            1
        );
        document.push(' ');
        std::fs::write(&path, &document).unwrap();
        assert_eq!(
            load(&file).unwrap_err().to_string(),
            "identity_file exceeds 16 MiB"
        );
        assert_eq!(
            load(&DocumentSource::Inline(document))
                .unwrap_err()
                .to_string(),
            "identities exceeds 16 MiB"
        );
        std::fs::remove_file(&path).unwrap();
        let error = load(&file).unwrap_err();
        assert!(error.to_string().starts_with("identity_file: "));
        assert!(std::error::Error::source(&error).is_some());
    }
}
