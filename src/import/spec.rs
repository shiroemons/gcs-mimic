//! Typed publication inputs that the scanner parses once per eligible source entry.

use super::codec::ObjectAttrs;
use crate::{
    acl::AclEntry,
    bucket::StorageClass,
    object::{
        ExpectedHashes, MapPatch, ObjectMetadataUpdate, Patch, generation::Generation,
        retention::ObjectRetention,
    },
    storage::admin::{GenerationCondition, Publication, PublishMode},
};
use std::time::SystemTime;

/// The checksums a source object declares; publication fails unless its body matches both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SourceHashes {
    pub(super) md5: [u8; 16],
    pub(super) crc32c: u32,
}

impl From<SourceHashes> for ExpectedHashes {
    fn from(hashes: SourceHashes) -> Self {
        Self {
            md5: Some(hashes.md5),
            crc32c: Some(hashes.crc32c),
        }
    }
}

/// What publication and resume verification need from one source object, parsed once. The
/// source times stay in the record: converting them to this platform's clock can still fail
/// (Windows holds no instant before 1601), and that failure belongs to the import, not the scan.
#[derive(Debug)]
pub(super) struct ObjectSpec {
    pub(super) generation: Generation,
    pub(super) hashes: SourceHashes,
    pub(super) storage_class: StorageClass,
    pub(super) acl: Option<Vec<AclEntry>>,
    pub(super) retention: Option<ObjectRetention>,
    /// `None` when the source leaves it unset.
    pub(super) custom_time: Option<String>,
}

impl ObjectSpec {
    /// The publication request. It takes the attributes so their strings move into the request.
    pub(super) fn into_publication(self, attrs: ObjectAttrs, cap: u64) -> Publication {
        // A source attribute is a plain string, so an empty one means the source left it unset;
        // storing it would add an empty field that a JSON upload stores only when asked to.
        let named = |value: String| Patch::nullable((!value.is_empty()).then_some(value));
        Publication {
            content_type: attrs.content_type,
            metadata: ObjectMetadataUpdate {
                retention: Patch::nullable(self.retention),
                acl: self.acl.into(),
                storage_class: Some(self.storage_class),
                custom_time: Patch::nullable(self.custom_time),
                content_encoding: named(attrs.content_encoding),
                content_disposition: named(attrs.content_disposition),
                content_language: named(attrs.content_language),
                cache_control: named(attrs.cache_control),
                metadata: attrs.metadata.map_or(MapPatch::Clear, |entries| {
                    MapPatch::Merge(
                        entries
                            .into_iter()
                            .map(|(key, value)| (key, Some(value)))
                            .collect(),
                    )
                }),
                event_based_hold: None,
                ..Default::default()
            },
            expected: self.hashes.into(),
            condition: Some(GenerationCondition::Absent),
            max_bytes: cap,
            mode: PublishMode::Replace,
        }
    }
}

/// What publication and resume verification need from one source bucket, parsed once.
#[derive(Debug)]
pub(super) struct BucketSpec {
    /// The creation time a memory snapshot records; a filesystem source has none.
    pub(super) created: Option<SystemTime>,
    pub(super) acl: Option<Vec<AclEntry>>,
}
