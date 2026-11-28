//! The local principal registry that the identity document declares.

use super::*;

/// The file is trusted and read once at startup, and every lookup goes through an index, so its
/// size is all that needs a bound: it is held in memory while it parses. Configuration reads the
/// document and holds either form to this limit.
pub(crate) const MAX_REGISTRY_BYTES: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_REGISTRY_MIB: u64 = MAX_REGISTRY_BYTES / (1024 * 1024);
/// The longest email address; object owners and ACL entities store the id with little to spare.
pub(super) const MAX_ID_BYTES: usize = 254;
/// A registered token arrives in the same header as a JWT, so the same bound fits both.
pub(super) const MAX_TOKEN_BYTES: usize = jwt::MAX_TOKEN_BYTES;

/// What an identity file supplies, which depends on the profile that loads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IdentityUse {
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

/// Local principal registry loaded from the strict-mode identity document.
#[derive(Debug)]
pub(crate) struct Registry {
    pub(super) identities: Vec<Identity>,
    /// Identities the file declares; the ready event counts these, not the built-in signer.
    pub(super) declared: usize,
    /// Authentication resolves a signer per request, so it must not walk the identities.
    pub(super) ids: HashMap<String, usize>,
    pub(super) tokens: HashMap<String, usize>,
    pub(super) signing_keys: SigningKeys,
    pub(super) verified_signatures: jwt::VerifiedSignatures,
    pub(super) public_access_prevention_projects: Vec<String>,
    /// The identity that requests without credentials on the trusted listener act as.
    pub(super) trusted: Option<usize>,
}

impl Registry {
    /// The projects the identity file names: those its identities hold roles in and those it
    /// lists for public access prevention.
    pub(super) fn project_ids(&self) -> impl Iterator<Item = &str> {
        self.identities
            .iter()
            .flat_map(|identity| identity.projects.keys())
            .chain(&self.public_access_prevention_projects)
            .map(String::as_str)
    }

    /// Strict mode's principals from the identity document's bytes, with the built-in test signer
    /// when `signer` settles to enabled.
    pub(super) fn load_principals(
        document: Option<&[u8]>,
        signer: TestSignerChoice,
        trusted: &TrustedCaller,
        projects: &BTreeSet<String>,
    ) -> Result<(Self, TestSigner), RegistryError> {
        let mut registry = match document {
            Some(bytes) => Self::parse(bytes)?,
            None => Self::without_document(),
        };
        let signer = signer.settle(|| registry.declares(test_signer::ID));
        match signer {
            TestSigner::Enabled => registry.add_test_signer(projects)?,
            TestSigner::Disabled => {}
        }
        registry.trust(trusted, projects)?;
        Ok((registry, signer))
    }

    /// The keys permissive verification accepts: an optional identity document's and the
    /// built-in test signer's when `signer` settles to enabled.
    pub(super) fn load_signing_keys(
        document: Option<&[u8]>,
        signer: TestSignerChoice,
    ) -> Result<(SigningKeys, TestSigner), RegistryError> {
        let mut keys = match document {
            Some(bytes) => Self::parse_for(bytes, IdentityUse::SigningKeys)?.signing_keys,
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

    /// A strict registry without an identity document, where only the built-in identities that
    /// loading adds will exist.
    fn without_document() -> Self {
        Self {
            identities: Vec::new(),
            declared: 0,
            ids: HashMap::new(),
            tokens: HashMap::new(),
            signing_keys: SigningKeys::default(),
            verified_signatures: jwt::VerifiedSignatures::default(),
            public_access_prevention_projects: Vec::new(),
            trusted: None,
        }
    }

    pub(super) fn parse_for(bytes: &[u8], usage: IdentityUse) -> Result<Self, RegistryError> {
        let value: Value = serde_json::from_slice(bytes).map_err(RegistryError::Syntax)?;
        let top = value
            .as_object()
            .ok_or_else(|| RegistryError::document("top level must be a JSON object"))?;
        if let Some(key) = top.keys().find(|key| {
            !matches!(
                key.as_str(),
                "identities" | "public_access_prevention_projects"
            )
        }) {
            return Err(RegistryError::document(format!("unknown field: {key}")));
        }
        let pap_projects = top.get("public_access_prevention_projects");
        if usage == IdentityUse::SigningKeys && pap_projects.is_some() {
            return Err(RegistryError::document(strict_only(
                "public_access_prevention_projects",
            )));
        }
        let public_access_prevention_projects = parse_pap_projects(pap_projects)?;
        let entries = top
            .get("identities")
            .ok_or_else(|| RegistryError::document("missing required field: identities"))?
            .as_array()
            .ok_or_else(|| RegistryError::document("identities must be an array"))?;
        if entries.is_empty() {
            return Err(RegistryError::document(
                "identities must contain at least one entry",
            ));
        }
        let mut registry = Self {
            identities: Vec::with_capacity(entries.len()),
            declared: entries.len(),
            ids: HashMap::with_capacity(entries.len()),
            tokens: HashMap::new(),
            signing_keys: SigningKeys::default(),
            verified_signatures: jwt::VerifiedSignatures::default(),
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
        let entry = entry
            .as_object()
            .ok_or_else(|| RegistryError::entry(index, "must be a JSON object"))?;
        if let Some(key) = entry.keys().find(|key| {
            !matches!(
                key.as_str(),
                "id" | "tokens" | "project_roles" | "public_keys" | "acl_entity"
            )
        }) {
            return Err(RegistryError::entry(index, format!("unknown field: {key}")));
        }
        if let Some(field) = usage.rejected_field(entry) {
            return Err(RegistryError::entry(index, strict_only(field)));
        }
        let id = entry
            .get("id")
            .ok_or_else(|| RegistryError::entry(index, "missing required field: id"))?
            .as_str()
            .ok_or_else(|| RegistryError::entry(index, "id must be a string"))?;
        if !valid_principal_id(id) {
            return Err(RegistryError::entry(
                index,
                format!(
                    "id must be 1..={MAX_ID_BYTES} bytes of printable ASCII without whitespace"
                ),
            ));
        }
        if id.starts_with(crate::object::owner::RESERVED_ID_PREFIX) {
            return Err(RegistryError::entry(
                index,
                format!(
                    "id must not start with {}; that namespace is reserved for emulator-internal principals",
                    crate::object::owner::RESERVED_ID_PREFIX
                ),
            ));
        }
        if self.ids.insert(id.to_owned(), index).is_some() {
            return Err(RegistryError::entry(index, format!("duplicate id: {id}")));
        }
        match (entry.get("public_keys"), usage) {
            (Some(keys), _) => self
                .signing_keys
                .insert(id, parse_public_keys(keys, index)?),
            (None, IdentityUse::SigningKeys) => {
                return Err(RegistryError::entry(
                    index,
                    "missing required field: public_keys; with auth_mode=permissive an identity file supplies only signing keys",
                ));
            }
            (None, IdentityUse::Principals) => {}
        }
        let tokens: &[Value] = match (entry.get("tokens"), usage) {
            (Some(tokens), _) => tokens
                .as_array()
                .ok_or_else(|| RegistryError::entry(index, "tokens must be an array"))?,
            (None, IdentityUse::SigningKeys) => &[],
            (None, IdentityUse::Principals) => {
                return Err(RegistryError::entry(
                    index,
                    "missing required field: tokens",
                ));
            }
        };
        self.add_tokens(tokens, index)?;
        let permissions = match entry.get("project_roles") {
            Some(projects) => parse_project_roles(projects, index)?,
            None => BTreeMap::new(),
        };
        self.identities.push(Identity {
            id: id.to_owned(),
            acl_entity: acl_identity::parse_alias(entry.get("acl_entity"), index)?,
            projects: permissions.into(),
        });
        Ok(())
    }

    fn add_tokens(&mut self, tokens: &[Value], index: usize) -> Result<(), RegistryError> {
        for token in tokens {
            let token = token
                .as_str()
                .filter(|token| visible_ascii(token, MAX_TOKEN_BYTES))
                .ok_or_else(|| {
                    RegistryError::entry(
                        index,
                        format!("each token must be 1..={MAX_TOKEN_BYTES} bytes of visible ASCII"),
                    )
                })?;
            // Never echo token values into startup errors or logs.
            if jwt_shaped(token) {
                return Err(RegistryError::entry(
                    index,
                    "tokens must not be JWT-shaped (three dot-separated segments)",
                ));
            }
            if self.tokens.insert(token.to_owned(), index).is_some() {
                return Err(RegistryError::entry(index, "duplicate token"));
            }
        }
        Ok(())
    }

    /// Adds the built-in test signer's public key. An entry the document declares with its ID
    /// keeps its tokens, project roles and ACL entity; otherwise the signer holds
    /// `roles/storage.objectUser` in `projects`, and elsewhere only bucket or object grants
    /// authorize it.
    pub(super) fn add_test_signer(
        &mut self,
        projects: &BTreeSet<String>,
    ) -> Result<(), RegistryError> {
        self.add_built_in(BuiltIn::TestSigner, projects)?;
        self.signing_keys
            .add(test_signer::ID, test_signer::public_key());
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.identities.len()
    }

    /// Who a request that presents no credentials acts as on `listener`.
    pub(super) fn caller_without_credentials(&self, listener: ListenerRole) -> Caller<'_> {
        let trusted = match listener {
            ListenerRole::Public => None,
            ListenerRole::Trusted => self.trusted.and_then(|index| self.identities.get(index)),
        };
        // A trusted listener without a principal stays anonymous, so a missing setting can only
        // take access away.
        trusted.map_or(Caller::Anonymous, Caller::Principal)
    }

    pub(super) fn identity(&self, id: &str) -> Option<&Identity> {
        self.identities.get(*self.ids.get(id)?)
    }
}

fn parse_pap_projects(value: Option<&Value>) -> Result<Vec<String>, RegistryError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let projects = value.as_array().ok_or_else(|| {
        RegistryError::document(
            "public_access_prevention_projects must be an array of project identifiers",
        )
    })?;
    let mut seen = BTreeSet::new();
    let mut public_access_prevention_projects = Vec::with_capacity(projects.len());
    for project in projects {
        let project = project.as_str().ok_or_else(|| {
            RegistryError::document("public_access_prevention_projects entries must be strings")
        })?;
        validate_project_id(project).map_err(|_| {
            RegistryError::document("invalid public_access_prevention_projects entry")
        })?;
        if !seen.insert(project) {
            return Err(RegistryError::document(
                "duplicate public_access_prevention_projects entry",
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
    let keys = keys
        .as_array()
        .filter(|keys| !keys.is_empty())
        .ok_or_else(|| {
            RegistryError::entry(
                index,
                "public_keys must be a nonempty array of inline public PEM strings",
            )
        })?;
    keys.iter()
        .map(|key| {
            signature::PublicKey::parse(key.as_str().ok_or_else(|| {
                RegistryError::entry(index, "public_keys entries must be strings")
            })?)
            .map_err(|defect| RegistryError::PublicKey {
                entry: index,
                defect,
            })
        })
        .collect()
}

/// Expands an identity's `project_roles` into the permissions granted per project.
fn parse_project_roles(
    projects: &Value,
    index: usize,
) -> Result<BTreeMap<String, ProjectGrant>, RegistryError> {
    let mut permissions = BTreeMap::new();
    let projects = projects
        .as_object()
        .ok_or_else(|| RegistryError::entry(index, "project_roles must be a JSON object"))?;
    for (project, role_values) in projects {
        validate_project_id(project).map_err(|defect| RegistryError::Project {
            entry: index,
            project: project.clone(),
            defect,
        })?;
        let role_values = role_values
            .as_array()
            .filter(|roles| !roles.is_empty())
            .ok_or_else(|| {
                RegistryError::entry(
                    index,
                    format!("project {project:?}: roles must be a nonempty array of role names"),
                )
            })?;
        let mut names = BTreeSet::new();
        let granted: &mut ProjectGrant = permissions.entry(project.clone()).or_default();
        for role in role_values {
            let role = role.as_str().ok_or_else(|| {
                RegistryError::entry(
                    index,
                    format!("project {project:?}: role names must be strings"),
                )
            })?;
            if !names.insert(role) {
                return Err(RegistryError::entry(
                    index,
                    format!("project {project:?}: duplicate role {role}"),
                ));
            }
            let role = role.parse::<Role>().map_err(|_| {
                RegistryError::entry(index, format!(
                    "project {project:?}: unsupported role {role} (only pinned predefined Cloud Storage roles are available)"
                ))
            })?;
            granted.add_role(role);
        }
    }
    Ok(permissions)
}

pub(super) fn visible_ascii(value: &str, max: usize) -> bool {
    (1..=max).contains(&value.len()) && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

pub(crate) fn valid_principal_id(value: &str) -> bool {
    visible_ascii(value, MAX_ID_BYTES)
}
