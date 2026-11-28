//! Registered local principals and the caller a request acts as.

use super::*;

/// A registered local principal with its project-scoped permissions expanded at load time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Identity {
    pub(super) id: String,
    pub(super) acl_entity: Option<crate::acl::Entity>,
    /// Shared because requests clone the caller, and a principal may hold any number of projects.
    pub(super) projects: Arc<BTreeMap<String, ProjectGrant>>,
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

    pub(crate) fn id(&self) -> &str {
        &self.id
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

/// The resolved strict-mode caller.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Caller<'a> {
    Anonymous,
    Principal(&'a Identity),
}

impl<'a> Caller<'a> {
    pub(crate) fn principal(self) -> Result<&'a Identity, AuthFailure> {
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
            Err(AuthFailure::Denied(Denied::new(
                Some(identity),
                permission,
                denied::Resource::project(project),
            )))
        }
    }
}
