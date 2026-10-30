//! Strict-profile permissions for object retention settings.
//!
//! objects.insert/patch/update additionally require storage.objects.setRetention when the
//! body includes `retention`, and overrideUnlockedRetention=true requires
//! storage.objects.overrideUnlockedRetention. The override permission is checked whenever the
//! flag is supplied with a retention change, before the catalog knows whether the change
//! actually locks, shortens or removes an Unlocked configuration.

use super::{BucketAccess, JsonError, State, auth};
use crate::object::{ObjectMetadataUpdate, retention::UnlockedOverride};

async fn require(
    state: &State,
    access: &BucketAccess,
    bucket: &str,
    permission: &str,
) -> Result<(), JsonError> {
    if let BucketAccess::Policy { identity, .. } = access {
        state
            .catalog
            .bucket_access(bucket, identity.as_ref(), permission)
            .await?;
    }
    Ok(())
}

pub(super) async fn authorize_upload(
    state: &State,
    access: &BucketAccess,
    bucket: &str,
    metadata: &ObjectMetadataUpdate,
) -> Result<(), JsonError> {
    if metadata.retention.is_some() {
        require(state, access, bucket, auth::OBJECTS_SET_RETENTION).await?;
    }
    Ok(())
}

pub(super) async fn authorize_update(
    state: &State,
    access: &BucketAccess,
    bucket: &str,
    update: &mut ObjectMetadataUpdate,
    retention_override: UnlockedOverride,
) -> Result<(), JsonError> {
    if update.retention.is_none() {
        return Ok(());
    }
    require(state, access, bucket, auth::OBJECTS_SET_RETENTION).await?;
    if retention_override == UnlockedOverride::Requested {
        require(
            state,
            access,
            bucket,
            auth::OBJECTS_OVERRIDE_UNLOCKED_RETENTION,
        )
        .await?;
    }
    update.override_unlocked_retention = retention_override;
    Ok(())
}
