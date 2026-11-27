//! Which permissions a bucket `testIamPermissions` request may name. The JSON API and Storage
//! v2 gRPC check the same list; each words a refusal in its own protocol.

use super::{Permission, roles::Role};

/// A permission a bucket permission test cannot answer.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum UntestablePermission {
    /// A permission a predefined role grants but no supported operation checks.
    #[error("permission {0}; use a supported operation permission such as storage.objects.get")]
    Unsupported(String),
    /// Text that names no Cloud Storage permission.
    #[error("{0} is not a valid Google Cloud Storage permission.")]
    Unknown(String),
    /// A project-scoped permission, which a bucket cannot hold.
    #[error("Invalid argument.")]
    ProjectScoped,
}

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

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let error = UntestablePermission::Unknown("read".into());
    assert_eq!(
        error.to_string(),
        "read is not a valid Google Cloud Storage permission."
    );
    assert!(std::error::Error::source(&error).is_none());
}
