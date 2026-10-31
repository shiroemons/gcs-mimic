//! Actual ownership is separate from additional ACL OWNER grants.

use crate::{acl::AclEntry, auth::Identity};

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
        matches!(self, Self::Principal { id, .. } if identity.is_some_and(|identity| identity.id() == id))
    }

    pub(crate) fn preserve_grant(&self, entries: &mut Vec<AclEntry>) {
        let Some(entity) = self.entity() else { return };
        if let Some(entry) = entries.iter_mut().find(|entry| entry.entity == entity) {
            entry.role = "OWNER".into();
        } else {
            entries.push(AclEntry {
                entity,
                role: "OWNER".into(),
            });
        }
    }

    pub(crate) fn remove_implicit_grant(&self, entries: &mut Vec<AclEntry>) {
        if let Some(entity) = self.entity() {
            entries.retain(|entry| entry.entity != entity);
        }
    }
}
