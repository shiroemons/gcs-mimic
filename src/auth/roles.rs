//! Pinned snapshot of Cloud Storage predefined roles, reduced to `storage.*` permissions.
//!
//! Source: https://docs.cloud.google.com/storage/docs/access-control/iam-roles
//! (checked 2026-10-22; page "Last updated 2026-10-21 UTC"). Non-storage permissions such as
//! `resourcemanager.projects.get` are omitted because no local route evaluates them. Basic roles
//! (`roles/owner`, `roles/editor`, `roles/viewer`) are intentionally absent: their storage
//! expansion is not part of the pinned storage role reference.
//!
//! Grant levels record each role's lowest grantable resource; managed-folder roles also apply
//! to buckets. Each permission list is sorted and unique so lookups can binary search.
//!
//! Stored bucket policies name roles from this table, so removing a role needs a migration of
//! the policies that hold it; a stored role outside the table reads as corruption.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GrantLevel {
    Project,
    Bucket,
    ManagedFolder,
}

/// Generates [`Role`] from one table row per role: its variant, name, lowest grant level and
/// `storage.*` permissions.
macro_rules! roles {
    ($((
        $variant:ident => $name:literal,
        $level:ident,
        [$($permission:literal),* $(,)?] $(,)?
    )),+ $(,)?) => {
        /// A pinned predefined Cloud Storage role. `Editor` and `Viewer` are
        /// `roles/storage.editor` and `roles/storage.viewer`, not the basic roles. Variants are
        /// declared in name order, which a unit test checks, so the derived order is the names'.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub(crate) enum Role {
            $($variant),+
        }

        impl Role {
            /// Every role, in name order.
            pub(crate) const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub(crate) fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }

            /// The lowest resource the role can be granted on.
            pub(crate) fn level(self) -> GrantLevel {
                match self {
                    $(Self::$variant => GrantLevel::$level),+
                }
            }

            /// Sorted and unique, so a lookup may binary search.
            pub(crate) fn permissions(self) -> &'static [&'static str] {
                match self {
                    $(Self::$variant => &[$($permission),*]),+
                }
            }
        }

        impl std::str::FromStr for Role {
            type Err = UnknownRole;

            fn from_str(name: &str) -> Result<Self, Self::Err> {
                match name {
                    $($name => Ok(Self::$variant),)+
                    _ => Err(UnknownRole),
                }
            }
        }
    };
}

roles! {
    (
        Admin => "roles/storage.admin",
        Bucket,
        [
            "storage.anywhereCaches.create",
            "storage.anywhereCaches.disable",
            "storage.anywhereCaches.get",
            "storage.anywhereCaches.list",
            "storage.anywhereCaches.pause",
            "storage.anywhereCaches.resume",
            "storage.anywhereCaches.update",
            "storage.bucketOperations.cancel",
            "storage.bucketOperations.get",
            "storage.bucketOperations.list",
            "storage.buckets.create",
            "storage.buckets.createTagBinding",
            "storage.buckets.delete",
            "storage.buckets.deleteTagBinding",
            "storage.buckets.enableObjectRetention",
            "storage.buckets.get",
            "storage.buckets.getIamPolicy",
            "storage.buckets.getIpFilter",
            "storage.buckets.getObjectInsights",
            "storage.buckets.list",
            "storage.buckets.listEffectiveTags",
            "storage.buckets.listTagBindings",
            "storage.buckets.relocate",
            "storage.buckets.restore",
            "storage.buckets.setIamPolicy",
            "storage.buckets.setIpFilter",
            "storage.buckets.update",
            "storage.buckets.viewIntelligenceDetails",
            "storage.buckets.viewSecurityIntelligenceDetails",
            "storage.featureConfigs.create",
            "storage.featureConfigs.delete",
            "storage.featureConfigs.get",
            "storage.featureConfigs.list",
            "storage.featureConfigs.update",
            "storage.folders.create",
            "storage.folders.delete",
            "storage.folders.get",
            "storage.folders.list",
            "storage.folders.rename",
            "storage.intelligenceConfigs.get",
            "storage.intelligenceConfigs.update",
            "storage.managedFolders.create",
            "storage.managedFolders.delete",
            "storage.managedFolders.get",
            "storage.managedFolders.getIamPolicy",
            "storage.managedFolders.list",
            "storage.managedFolders.setIamPolicy",
            "storage.managedFolders.update",
            "storage.multipartUploads.abort",
            "storage.multipartUploads.create",
            "storage.multipartUploads.list",
            "storage.multipartUploads.listParts",
            "storage.objects.create",
            "storage.objects.createContext",
            "storage.objects.delete",
            "storage.objects.deleteContext",
            "storage.objects.get",
            "storage.objects.getIamPolicy",
            "storage.objects.list",
            "storage.objects.move",
            "storage.objects.overrideUnlockedRetention",
            "storage.objects.restore",
            "storage.objects.setIamPolicy",
            "storage.objects.setRetention",
            "storage.objects.update",
            "storage.objects.updateContext",
        ],
    ),
    (
        AnnotationGeneratorService => "roles/storage.annotationGeneratorService",
        ManagedFolder,
        [
            "storage.objects.createContext",
            "storage.objects.deleteContext",
            "storage.objects.get",
            "storage.objects.list",
            "storage.objects.update",
            "storage.objects.updateContext",
        ],
    ),
    (
        BucketViewer => "roles/storage.bucketViewer",
        Bucket,
        ["storage.buckets.get", "storage.buckets.list"],
    ),
    (
        Editor => "roles/storage.editor",
        ManagedFolder,
        [
            "storage.buckets.create",
            "storage.buckets.delete",
            "storage.buckets.list",
            "storage.buckets.listEffectiveTags",
            "storage.buckets.listTagBindings",
            "storage.buckets.viewIntelligenceDetails",
            "storage.buckets.viewSecurityIntelligenceDetails",
            "storage.featureConfigs.create",
            "storage.featureConfigs.delete",
            "storage.featureConfigs.get",
            "storage.featureConfigs.list",
            "storage.featureConfigs.update",
            "storage.folders.create",
            "storage.folders.delete",
            "storage.folders.get",
            "storage.folders.list",
            "storage.folders.rename",
            "storage.hmacKeys.create",
            "storage.hmacKeys.delete",
            "storage.hmacKeys.get",
            "storage.hmacKeys.list",
            "storage.hmacKeys.update",
            "storage.intelligenceConfigs.get",
        ],
    ),
    (
        ExpressModeServiceInput => "roles/storage.expressModeServiceInput",
        ManagedFolder,
        [
            "storage.objects.create",
            "storage.objects.delete",
            "storage.objects.list",
            "storage.objects.update",
        ],
    ),
    (
        ExpressModeServiceOutput => "roles/storage.expressModeServiceOutput",
        ManagedFolder,
        [
            "storage.objects.delete",
            "storage.objects.get",
            "storage.objects.list",
        ],
    ),
    (
        ExpressModeUserAccess => "roles/storage.expressModeUserAccess",
        Project,
        [
            "storage.buckets.get",
            "storage.buckets.list",
            "storage.multipartUploads.abort",
            "storage.multipartUploads.create",
            "storage.multipartUploads.list",
            "storage.multipartUploads.listParts",
            "storage.objects.create",
            "storage.objects.delete",
            "storage.objects.get",
            "storage.objects.list",
            "storage.objects.restore",
            "storage.objects.update",
        ],
    ),
    (
        FolderAdmin => "roles/storage.folderAdmin",
        ManagedFolder,
        [
            "storage.folders.create",
            "storage.folders.delete",
            "storage.folders.get",
            "storage.folders.list",
            "storage.folders.rename",
            "storage.managedFolders.create",
            "storage.managedFolders.delete",
            "storage.managedFolders.get",
            "storage.managedFolders.getIamPolicy",
            "storage.managedFolders.list",
            "storage.managedFolders.setIamPolicy",
            "storage.managedFolders.update",
            "storage.multipartUploads.abort",
            "storage.multipartUploads.create",
            "storage.multipartUploads.list",
            "storage.multipartUploads.listParts",
            "storage.objects.create",
            "storage.objects.createContext",
            "storage.objects.delete",
            "storage.objects.deleteContext",
            "storage.objects.get",
            "storage.objects.getIamPolicy",
            "storage.objects.list",
            "storage.objects.move",
            "storage.objects.overrideUnlockedRetention",
            "storage.objects.restore",
            "storage.objects.setIamPolicy",
            "storage.objects.setRetention",
            "storage.objects.update",
            "storage.objects.updateContext",
        ],
    ),
    (
        HmacKeyAdmin => "roles/storage.hmacKeyAdmin",
        Project,
        [
            "storage.hmacKeys.create",
            "storage.hmacKeys.delete",
            "storage.hmacKeys.get",
            "storage.hmacKeys.list",
            "storage.hmacKeys.update",
        ],
    ),
    (
        InsightsCollectorService => "roles/storage.insightsCollectorService",
        Bucket,
        ["storage.buckets.get", "storage.buckets.getObjectInsights"],
    ),
    (
        LegacyBucketOwner => "roles/storage.legacyBucketOwner",
        Bucket,
        [
            "storage.anywhereCaches.create",
            "storage.anywhereCaches.disable",
            "storage.anywhereCaches.get",
            "storage.anywhereCaches.list",
            "storage.anywhereCaches.pause",
            "storage.anywhereCaches.resume",
            "storage.anywhereCaches.update",
            "storage.bucketOperations.cancel",
            "storage.bucketOperations.get",
            "storage.bucketOperations.list",
            "storage.buckets.createTagBinding",
            "storage.buckets.deleteTagBinding",
            "storage.buckets.enableObjectRetention",
            "storage.buckets.get",
            "storage.buckets.getIamPolicy",
            "storage.buckets.getIpFilter",
            "storage.buckets.listEffectiveTags",
            "storage.buckets.listTagBindings",
            "storage.buckets.relocate",
            "storage.buckets.restore",
            "storage.buckets.setIamPolicy",
            "storage.buckets.setIpFilter",
            "storage.buckets.update",
            "storage.folders.create",
            "storage.folders.delete",
            "storage.folders.get",
            "storage.folders.list",
            "storage.folders.rename",
            "storage.managedFolders.create",
            "storage.managedFolders.delete",
            "storage.managedFolders.get",
            "storage.managedFolders.getIamPolicy",
            "storage.managedFolders.list",
            "storage.managedFolders.setIamPolicy",
            "storage.managedFolders.update",
            "storage.multipartUploads.abort",
            "storage.multipartUploads.create",
            "storage.multipartUploads.list",
            "storage.multipartUploads.listParts",
            "storage.objects.create",
            "storage.objects.createContext",
            "storage.objects.delete",
            "storage.objects.deleteContext",
            "storage.objects.list",
            "storage.objects.restore",
            "storage.objects.setRetention",
            "storage.objects.updateContext",
        ],
    ),
    (
        LegacyBucketReader => "roles/storage.legacyBucketReader",
        Bucket,
        [
            "storage.buckets.get",
            "storage.folders.get",
            "storage.folders.list",
            "storage.managedFolders.get",
            "storage.managedFolders.list",
            "storage.multipartUploads.list",
            "storage.objects.list",
        ],
    ),
    (
        LegacyBucketWriter => "roles/storage.legacyBucketWriter",
        Bucket,
        [
            "storage.buckets.get",
            "storage.folders.create",
            "storage.folders.delete",
            "storage.folders.get",
            "storage.folders.list",
            "storage.folders.rename",
            "storage.managedFolders.create",
            "storage.managedFolders.delete",
            "storage.managedFolders.get",
            "storage.managedFolders.list",
            "storage.managedFolders.update",
            "storage.multipartUploads.abort",
            "storage.multipartUploads.create",
            "storage.multipartUploads.list",
            "storage.multipartUploads.listParts",
            "storage.objects.create",
            "storage.objects.createContext",
            "storage.objects.delete",
            "storage.objects.list",
            "storage.objects.restore",
            "storage.objects.setRetention",
        ],
    ),
    (
        LegacyObjectOwner => "roles/storage.legacyObjectOwner",
        ManagedFolder,
        [
            "storage.objects.createContext",
            "storage.objects.deleteContext",
            "storage.objects.get",
            "storage.objects.getIamPolicy",
            "storage.objects.overrideUnlockedRetention",
            "storage.objects.setIamPolicy",
            "storage.objects.setRetention",
            "storage.objects.update",
            "storage.objects.updateContext",
        ],
    ),
    (
        LegacyObjectReader => "roles/storage.legacyObjectReader",
        ManagedFolder,
        ["storage.objects.get"],
    ),
    (
        ObjectAdmin => "roles/storage.objectAdmin",
        ManagedFolder,
        [
            "storage.folders.create",
            "storage.folders.delete",
            "storage.folders.get",
            "storage.folders.list",
            "storage.folders.rename",
            "storage.managedFolders.create",
            "storage.managedFolders.delete",
            "storage.managedFolders.get",
            "storage.managedFolders.list",
            "storage.managedFolders.update",
            "storage.multipartUploads.abort",
            "storage.multipartUploads.create",
            "storage.multipartUploads.list",
            "storage.multipartUploads.listParts",
            "storage.objects.create",
            "storage.objects.createContext",
            "storage.objects.delete",
            "storage.objects.deleteContext",
            "storage.objects.get",
            "storage.objects.getIamPolicy",
            "storage.objects.list",
            "storage.objects.move",
            "storage.objects.overrideUnlockedRetention",
            "storage.objects.restore",
            "storage.objects.setIamPolicy",
            "storage.objects.setRetention",
            "storage.objects.update",
            "storage.objects.updateContext",
        ],
    ),
    (
        ObjectCreator => "roles/storage.objectCreator",
        ManagedFolder,
        [
            "storage.folders.create",
            "storage.managedFolders.create",
            "storage.multipartUploads.abort",
            "storage.multipartUploads.create",
            "storage.multipartUploads.listParts",
            "storage.objects.create",
            "storage.objects.createContext",
        ],
    ),
    (
        ObjectUser => "roles/storage.objectUser",
        ManagedFolder,
        [
            "storage.folders.create",
            "storage.folders.delete",
            "storage.folders.get",
            "storage.folders.list",
            "storage.folders.rename",
            "storage.managedFolders.create",
            "storage.managedFolders.delete",
            "storage.managedFolders.get",
            "storage.managedFolders.list",
            "storage.managedFolders.update",
            "storage.multipartUploads.abort",
            "storage.multipartUploads.create",
            "storage.multipartUploads.list",
            "storage.multipartUploads.listParts",
            "storage.objects.create",
            "storage.objects.createContext",
            "storage.objects.delete",
            "storage.objects.deleteContext",
            "storage.objects.get",
            "storage.objects.list",
            "storage.objects.move",
            "storage.objects.restore",
            "storage.objects.update",
            "storage.objects.updateContext",
        ],
    ),
    (
        ObjectViewer => "roles/storage.objectViewer",
        ManagedFolder,
        [
            "storage.folders.get",
            "storage.folders.list",
            "storage.managedFolders.get",
            "storage.managedFolders.list",
            "storage.objects.get",
            "storage.objects.list",
        ],
    ),
    (
        Viewer => "roles/storage.viewer",
        ManagedFolder,
        [
            "storage.buckets.list",
            "storage.buckets.listEffectiveTags",
            "storage.buckets.listTagBindings",
            "storage.buckets.viewIntelligenceDetails",
            "storage.buckets.viewSecurityIntelligenceDetails",
            "storage.featureConfigs.get",
            "storage.featureConfigs.list",
            "storage.folders.get",
            "storage.folders.list",
            "storage.hmacKeys.get",
            "storage.hmacKeys.list",
            "storage.intelligenceConfigs.get",
        ],
    ),
}

impl Role {
    pub(crate) fn grants(self, permission: super::Permission) -> bool {
        self.permissions()
            .binary_search(&permission.as_str())
            .is_ok()
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A role name outside the pinned snapshot, including the basic roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a pinned predefined Cloud Storage role")]
pub(crate) struct UnknownRole;

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let error = UnknownRole;
    assert_eq!(
        error.to_string(),
        "not a pinned predefined Cloud Storage role"
    );
    assert!(std::error::Error::source(&error).is_none());
}
