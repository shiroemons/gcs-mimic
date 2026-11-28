//! The permissions that roles grant and the project grants that hold them.

use super::*;
use std::fmt;

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

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("unsupported permission")]
pub(crate) struct UnsupportedPermission;

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
pub(super) struct PermissionSet(u32);

impl PermissionSet {
    pub(super) fn contains(self, permission: Permission) -> bool {
        self.0 & permission.bit() != 0
    }
    fn insert(&mut self, permission: Permission) {
        self.0 |= permission.bit();
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ProjectGrant {
    pub(super) permissions: PermissionSet,
    pub(super) any_storage_permission: bool,
}

impl ProjectGrant {
    pub(super) fn grant(&mut self, permission: Permission) {
        self.any_storage_permission = true;
        self.permissions.insert(permission);
    }

    /// Adds what the predefined `role` grants.
    pub(super) fn add_role(&mut self, role: Role) {
        let expanded = role.permissions();
        self.any_storage_permission |= !expanded.is_empty();
        for permission in Permission::ALL {
            if expanded.contains(&permission.as_str()) {
                self.grant(*permission);
            }
        }
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
