//! Actual ownership is separate from additional ACL OWNER grants.

use crate::{acl::AclEntry, auth::Identity};

/// Owner id of objects written by the permissive profile's local creator. The id is fixed, so a
/// changed creator address renames the entity without rewriting the owner kind. The whole
/// `gcs-mimic:` namespace is reserved: `Registry::parse` rejects it, so no strict identity can
/// claim these objects.
pub(crate) const LOCAL_CREATOR_ID: &str = "gcs-mimic:local-creator";
pub(crate) const RESERVED_ID_PREFIX: &str = "gcs-mimic:";

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(crate) enum ObjectOwner {
    Principal { id: String, entity: Option<String> },
    ProjectOwners { project: String },
}

impl ObjectOwner {
    pub(crate) fn entity(&self) -> Option<String> {
        match self {
            Self::Principal { entity, .. } => entity.clone(),
            Self::ProjectOwners { project } => Some(format!(
                "project-owners-{}",
                crate::acl::project_number(project)
            )),
        }
    }

    pub(crate) fn is_principal(&self, identity: Option<&Identity>) -> bool {
        // The local creator only labels permissive-era objects; it is never a caller, even if a
        // registry somehow held its id.
        matches!(self, Self::Principal { id, .. } if id != LOCAL_CREATOR_ID && identity.is_some_and(|identity| identity.id() == id))
    }

    pub(crate) fn preserve_grant(&self, entries: &mut Vec<AclEntry>) {
        let Some(entity) = self.entity() else { return };
        if let Some(entry) = entries.iter_mut().find(|entry| entry.entity == entity) {
            entry.role = "OWNER".into();
        } else {
            let grant = AclEntry {
                entity,
                role: "OWNER".into(),
            };
            // Cloud Storage lists the project owners team first on an object, ahead of the
            // other project teams (recorded 2026-10-31); any other owner follows them.
            match self {
                Self::ProjectOwners { .. } => entries.insert(0, grant),
                Self::Principal { .. } => entries.push(grant),
            }
        }
    }

    pub(crate) fn remove_implicit_grant(&self, entries: &mut Vec<AclEntry>) {
        // The project owners' grant keeps its place in the default ACL; Cloud Storage lists it
        // first, before the editors and viewers, while a grant re-added on read comes last.
        if matches!(self, Self::ProjectOwners { .. }) {
            return;
        }
        if let Some(entity) = self.entity() {
            entries.retain(|entry| entry.entity != entity);
        }
    }
}
