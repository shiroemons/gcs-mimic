//! Which permissions a bucket `testIamPermissions` request may name. The JSON API and Storage
//! v2 gRPC check the same list; each words a refusal in its own protocol.

use super::{Permission, roles::Role};
use std::fmt;

/// A permission a bucket permission test cannot answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UntestablePermission {
    /// A permission a predefined role grants but no supported operation checks.
    Unsupported(String),
    /// Text that names no Cloud Storage permission.
    Unknown(String),
    /// A project-scoped permission, which a bucket cannot hold.
    ProjectScoped,
}

impl fmt::Display for UntestablePermission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(permission) => write!(
                formatter,
                "permission {permission}; use a supported operation permission such as storage.objects.get"
            ),
            Self::Unknown(permission) => write!(
                formatter,
                "{permission} is not a valid Google Cloud Storage permission."
            ),
            Self::ProjectScoped => formatter.write_str("Invalid argument."),
        }
    }
}

impl std::error::Error for UntestablePermission {}

/// The permission `permission` names, when a bucket permission test may ask for it.
pub(crate) fn testable_permission(permission: &str) -> Result<Permission, UntestablePermission> {
    let Ok(parsed) = Permission::try_from(permission) else {
        if Role::ALL
            .iter()
            .any(|role| role.permissions().contains(&permission))
        {
            return Err(UntestablePermission::Unsupported(permission.to_owned()));
        }
        return Err(UntestablePermission::Unknown(permission.to_owned()));
    };
    if matches!(parsed, Permission::BucketsCreate | Permission::BucketsList) {
        return Err(UntestablePermission::ProjectScoped);
    }
    Ok(parsed)
}
