//! Identities that gcs-mimic supplies itself: the built-in test signer, and the application
//! identity that the trusted listener acts as when `trusted_principal` names none.
//!
//! An identity document entry with a built-in's ID replaces it, so an explicit declaration
//! always wins over the implicit grants.

use super::{
    GrantSource, Identity, ProjectGrant, Registry, RegistryError, acl_identity, test_signer,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// The trusted listener's application identity. The reserved `.invalid` domain can never name a
/// real Google account, and the ID differs from the test signer and from the permissive creator
/// `local-user@gcs-mimic.invalid`.
pub const TRUSTED_APP_ID: &str = "trusted-app@gcs-mimic.invalid";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BuiltIn {
    TestSigner,
    /// Holds no token and no key, so only the trusted listener can act as it.
    TrustedApp,
}

impl BuiltIn {
    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::TestSigner => test_signer::ID,
            Self::TrustedApp => TRUSTED_APP_ID,
        }
    }

    /// The predefined role the identity holds in each implicit project when the identity
    /// document does not declare it. The application's own servers manage their buckets, as a
    /// deployed service account with `roles/storage.admin` would. The test signer's key is
    /// public, so it gets the least role that covers signed GET and PUT, overwrites and what
    /// Rails also does (delete, list and metadata updates): `objectUser` administers no IAM
    /// policy or ACL.
    pub(crate) fn role(self) -> Option<&'static str> {
        match self {
            Self::TestSigner => Some("roles/storage.objectUser"),
            Self::TrustedApp => Some("roles/storage.admin"),
        }
    }

    /// The fix when another identity's ID or alias names this built-in.
    fn collision(self) -> String {
        let id = self.id();
        let remedy = match self {
            Self::TestSigner => "set test_signer=disabled",
            Self::TrustedApp => "name another identity with trusted_principal",
        };
        format!(
            "another identity's id or acl_entity names the built-in identity {id}; declare {id} itself or {remedy}"
        )
    }

    /// The implicit grants: the role in every project, or none without a role. They carry the
    /// role's permissions but no explicit `roles/storage.admin`, so they never open sync controls,
    /// which the identity document alone can grant.
    fn grants(self, projects: &BTreeSet<String>) -> BTreeMap<String, ProjectGrant> {
        let Some(role) = self.role() else {
            return BTreeMap::new();
        };
        // The roles above are in the pinned snapshot, which a unit test proves; a role missing
        // from it would grant nothing rather than something else.
        projects
            .iter()
            .filter_map(|project| {
                let mut grant = ProjectGrant::default();
                grant
                    .add_role(role, GrantSource::Implicit)
                    .map(|()| (project.clone(), grant))
            })
            .collect()
    }
}

impl Registry {
    /// Adds `built_in` with its implicit grants in `projects`, unless the identity document
    /// declares its ID, in which case the declared entry keeps its tokens, project roles and ACL
    /// entity. Returns the identity's index.
    pub(super) fn add_built_in(
        &mut self,
        built_in: BuiltIn,
        projects: &BTreeSet<String>,
    ) -> Result<usize, RegistryError> {
        let id = built_in.id();
        if let Some(index) = self.ids.get(id) {
            return Ok(*index);
        }
        let index = self.identities.len();
        self.ids.insert(id.to_owned(), index);
        self.identities.push(Identity {
            id: id.to_owned(),
            acl_entity: None,
            projects: Arc::new(built_in.grants(projects)),
        });
        // The document's own aliases were already validated, so a collision involves this one.
        acl_identity::validate_aliases(&self.identities).map_err(|_| RegistryError::Invalid {
            entry: None,
            reason: built_in.collision(),
        })?;
        Ok(index)
    }

    /// Makes requests without credentials on the trusted listener act as the built-in
    /// application identity, with `roles/storage.admin` in `projects` unless it is declared.
    pub(super) fn trust_built_in_app(
        &mut self,
        projects: &BTreeSet<String>,
    ) -> Result<(), RegistryError> {
        self.trusted = Some(self.add_built_in(BuiltIn::TrustedApp, projects)?);
        Ok(())
    }

    /// Whether the identity document declares `id`, as opposed to gcs-mimic adding it.
    pub(super) fn declares(&self, id: &str) -> bool {
        self.ids.get(id).is_some_and(|index| *index < self.declared)
    }
}

#[cfg(test)]
mod tests;
