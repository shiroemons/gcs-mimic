use crate::bucket::{BucketAccess, BucketRecord};
use crate::object::ObjectTarget;
use std::time::UNIX_EPOCH;

/// Spelled out at every call site so a test that skips authorization says so.
pub(crate) const PERMISSIVE: &BucketAccess = &BucketAccess::Permissive;

pub(crate) fn live(bucket: &str, name: &str) -> ObjectTarget {
    ObjectTarget::new(bucket, name, None)
}

pub(crate) fn version(bucket: &str, name: &str, generation: i64) -> ObjectTarget {
    ObjectTarget::new(bucket, name, Some(generation))
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
