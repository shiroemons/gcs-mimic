//! Strict-profile permissions for object retention settings.
//!
//! objects.insert/patch/update additionally require storage.objects.setRetention when the
//! body includes `retention`, and overrideUnlockedRetention=true requires
//! storage.objects.overrideUnlockedRetention. The override permission is checked whenever the
//! flag is supplied with a retention change, before the catalog knows whether the change
//! actually locks, shortens or removes an Unlocked configuration. A copy, rewrite or compose
//! that names a retention for its destination needs storage.objects.setRetention as an upload
//! does.

use super::{ApiError, BucketAccess, Catalog, auth};
use crate::object::{ObjectMetadataUpdate, Patch, retention::UnlockedOverride};

async fn require(
    catalog: &Catalog,
    access: &BucketAccess,
    bucket: &str,
    permission: auth::Permission,
) -> Result<(), ApiError> {
    if let BucketAccess::Policy { identity, .. } = access {
        catalog
            .bucket_access(bucket, identity.as_ref(), permission)
            .await?;
    }
    Ok(())
}

pub(super) async fn authorize_upload(
    catalog: &Catalog,
    access: &BucketAccess,
    bucket: &str,
    metadata: &ObjectMetadataUpdate,
) -> Result<(), ApiError> {
    if metadata.retention != Patch::Keep {
        require(catalog, access, bucket, auth::OBJECTS_SET_RETENTION).await?;
    }
    Ok(())
}

pub(crate) async fn authorize_update(
    catalog: &Catalog,
    access: &BucketAccess,
    bucket: &str,
    update: &mut ObjectMetadataUpdate,
    retention_override: UnlockedOverride,
) -> Result<(), ApiError> {
    if update.retention == Patch::Keep {
        return Ok(());
    }
    require(catalog, access, bucket, auth::OBJECTS_SET_RETENTION).await?;
    if retention_override == UnlockedOverride::Requested {
        require(
            catalog,
            access,
            bucket,
            auth::OBJECTS_OVERRIDE_UNLOCKED_RETENTION,
        )
        .await?;
    }
    update.override_unlocked_retention = retention_override;
    Ok(())
}
