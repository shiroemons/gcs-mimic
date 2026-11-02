use super::{ObjectUpload, RestoreAccess};
use crate::bucket::{BucketAccess, BucketRecord};
use crate::object::ObjectTarget;
use std::time::UNIX_EPOCH;

/// Spelled out at every call site so a test that skips authorization says so.
pub(crate) const PERMISSIVE: &BucketAccess = &BucketAccess::Permissive;

/// Names both permissive authorities so tests explicitly skip restore authorization.
pub(crate) const PERMISSIVE_RESTORE: RestoreAccess<'static> = RestoreAccess {
    restore: PERMISSIVE,
    create: PERMISSIVE,
};

/// The live version of bucket/name.
pub(crate) fn live(bucket: &str, name: &str) -> ObjectTarget {
    ObjectTarget::new(bucket, name, None)
}

/// A specific generation of bucket/name.
pub(crate) fn version(bucket: &str, name: &str, generation: i64) -> ObjectTarget {
    ObjectTarget::new(bucket, name, Some(generation))
}

/// A text/plain upload with no declared hashes or preconditions and permissive access.
pub(crate) fn permissive_upload<'a>(
    bucket: &'a str,
    name: &'a str,
    max_bytes: u64,
) -> ObjectUpload<'a> {
    ObjectUpload {
        bucket,
        name,
        content_type: "text/plain".into(),
        expected: Default::default(),
        conditions: Default::default(),
        max_bytes,
        metadata: Default::default(),
        access: BucketAccess::Permissive,
    }
}

pub(crate) fn bucket(name: &str, project: &str) -> BucketRecord {
    BucketRecord {
        name: name.into(),
        project: project.into(),
        location: "US".into(),
        location_type: "multi-region".into(),
        storage_class: "STANDARD".into(),
        autoclass: None,
        labels: Default::default(),
        versioning_enabled: false,
        versioning_configured: false,
        default_event_based_hold: false,
        public_access_prevention_enforced: false,
        uniform_bucket_level_access_locked_time: None,
        lifecycle: Vec::new(),
        retention_policy: None,
        soft_delete_policy: None,
        object_retention_enabled: false,
        cors: Vec::new(),
        metageneration: 1,
        generation: 0,
        time_created: UNIX_EPOCH,
        updated: UNIX_EPOCH,
    }
}
