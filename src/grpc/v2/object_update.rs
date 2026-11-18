use super::{
    bucket_update::acl_entries,
    mask::{UnappliedUpdatePath, update_paths},
    object_io::ObjectField,
    pb,
};
use crate::{
    acl::{AclKind, AclRequest, PredefinedObjectAcl},
    clock::{RFC3339_MAX_MICROS, YEAR_ONE_MICROS},
    metadata_error::MetadataError,
    object::{
        MapPatch, ObjectError, ObjectMetadataUpdate, ObjectRecord, Patch, WriteMode,
        retention::{ObjectRetention, RetentionMode},
        unix_nanos::UnixNanos,
    },
};
use std::collections::HashMap;
use tonic::Status;

/// An UpdateObject `update_mask` path, parsed once before it is applied.
#[derive(Clone, Debug, PartialEq, Eq)]
enum UpdatePath {
    /// `metadata`: merges the request's entries, or clears the map when it has none.
    Metadata,
    /// `metadata.<key>`: sets one entry, or removes it when the request does not carry the
    /// key. `metadata.` names the empty key, which Cloud Storage updated (recorded 2026-11-17,
    /// case obj-gaps-empty-metadata-key); the pinned Go SDK sends it for that key.
    MetadataKey(String),
    ContentType,
    ContentEncoding,
    ContentDisposition,
    ContentLanguage,
    CacheControl,
    CustomTime,
    TemporaryHold,
    EventBasedHold,
    Acl,
    Retention,
}

/// The refusal of `storage_class` in an update mask: only a rewrite changes a storage class.
/// Recorded with NEARLINE (case grpc-probe-inferred-object-masks); that every other value is
/// refused the same way is inferred.
const STORAGE_CLASS_PATH: &str = "Client is not allowed to directly modify storage_class.";

/// The refusal of `kms_key` in an update mask. Recorded with the name of a key that does not
/// exist (case grpc-probe-inferred-object-masks); that an empty key or any other is refused the
/// same way is inferred.
const KMS_KEY_PATH: &str = "Cannot change the Cloud KMS key associated with a resource.";

impl std::str::FromStr for UpdatePath {
    type Err = UnappliedUpdatePath;

    fn from_str(path: &str) -> Result<Self, Self::Err> {
        Ok(match path {
            "metadata" => Self::Metadata,
            "content_type" => Self::ContentType,
            "content_encoding" => Self::ContentEncoding,
            "content_disposition" => Self::ContentDisposition,
            "content_language" => Self::ContentLanguage,
            "cache_control" => Self::CacheControl,
            "custom_time" => Self::CustomTime,
            "temporary_hold" => Self::TemporaryHold,
            "event_based_hold" => Self::EventBasedHold,
            "acl" => Self::Acl,
            "retention" => Self::Retention,
            "storage_class" => return Err(UnappliedUpdatePath::Refused(STORAGE_CLASS_PATH)),
            "kms_key" => return Err(UnappliedUpdatePath::Refused(KMS_KEY_PATH)),
            // Any other path is classified by the `Object` fields. The pinned Go SDK names the
            // output-only `create_time` in an ordinary empty update, which changes nothing.
            path => match path.strip_prefix("metadata.") {
                Some(key) => Self::MetadataKey(key.into()),
                None => return Err(UnappliedUpdatePath::of::<ObjectField>(path)),
            },
        })
    }
}

pub(super) fn from_proto(
    object: &pb::Object,
    paths: &[String],
) -> Result<ObjectMetadataUpdate, Status> {
    // `*` beside other paths replaces the editable metadata too: the request that named
    // content_language beside it was applied as `*` alone is (recorded in case
    // grpc-probe-inferred-object-masks). The other paths are not read, which is inferred.
    if paths.iter().any(|path| path == "*") {
        return replacement(object);
    }
    if paths.iter().any(|path| path == "metadata")
        && paths.iter().any(|path| path.starts_with("metadata."))
    {
        return Err(Status::invalid_argument(
            "Use either metadata or metadata.KEY in update_mask",
        ));
    }
    let mut update = ObjectMetadataUpdate::default();
    for path in update_paths(paths, "UpdateObject", "metadata or content_type") {
        apply_path(&mut update, object, path?)?;
    }
    validate(&update)?;
    Ok(update)
}

fn apply_path(
    update: &mut ObjectMetadataUpdate,
    object: &pb::Object,
    path: UpdatePath,
) -> Result<(), Status> {
    match path {
        UpdatePath::Metadata => update.metadata = masked_metadata(&object.metadata),
        UpdatePath::MetadataKey(key) => {
            let value = object.metadata.get(&key).cloned();
            update.metadata.merge_entry(key, value);
        }
        UpdatePath::ContentType => update.content_type = Patch::Set(object.content_type.clone()),
        // A proto3 string cannot tell empty from unset: an empty masked field clears it.
        UpdatePath::ContentEncoding => {
            update.content_encoding = Patch::nullable(set(&object.content_encoding));
        }
        UpdatePath::ContentDisposition => {
            update.content_disposition = Patch::nullable(set(&object.content_disposition));
        }
        UpdatePath::ContentLanguage => {
            update.content_language = Patch::nullable(set(&object.content_language));
        }
        UpdatePath::CacheControl => {
            update.cache_control = Patch::nullable(set(&object.cache_control));
        }
        UpdatePath::CustomTime => {
            update.custom_time = Patch::nullable(object.custom_time.map(|time| time.to_string()));
        }
        UpdatePath::TemporaryHold => update.temporary_hold = Some(object.temporary_hold),
        UpdatePath::EventBasedHold => {
            update.event_based_hold = Some(object.event_based_hold.unwrap_or(false));
        }
        UpdatePath::Acl => update.acl = acl_request(object)?,
        UpdatePath::Retention => {
            update.retention = Patch::nullable(retention(object.retention.as_ref())?);
        }
    }
    Ok(())
}

/// The ACL an `Object` in a request lists, as a JSON resource's `acl` array is taken.
fn acl_request(object: &pb::Object) -> Result<AclRequest, Status> {
    Ok(AclRequest::Entries(acl_entries(
        AclKind::Object,
        object.acl.iter().map(|entry| (&entry.entity, &entry.role)),
    )?))
}

/// The change a `*` mask makes: the request's object replaces the editable metadata, as a JSON
/// PUT does. Cloud Storage cleared the content language, the cache control and the custom
/// metadata of an object for a `*` update that carried only a content type, and left its ACL
/// (recorded 2026-11-18). An ACL or a retention the request does carry is applied as its own
/// path applies it; one it leaves out is kept, which for the retention is not recorded. The
/// update is a [`WriteMode::Replace`], so `ObjectMetadataUpdate::apply_to_existing` also
/// releases each hold the request does not set, as gcs-mimic's JSON PUT does: Cloud Storage
/// released a temporary hold and an event-based hold that a `*` update with only a content type
/// left out (recorded in case grpc-probe-inferred-object-masks).
fn replacement(object: &pb::Object) -> Result<ObjectMetadataUpdate, Status> {
    let mut update = for_write(object);
    if !object.acl.is_empty() {
        update.acl = acl_request(object)?;
    }
    if let Some(retention) = retention(object.retention.as_ref())? {
        update.retention = Patch::Set(retention);
    }
    validate(&update)?;
    Ok(update)
}

/// The change a bare `metadata` path makes. Cloud Storage merged the request's entries into the
/// stored map, keeping the keys it left out, and cleared the map for a request with none
/// (recorded 2026-11-17, case obj-gaps-empty-metadata-key), as a JSON PATCH does. The pinned Go
/// SDK relies on the second to delete every key.
fn masked_metadata(entries: &HashMap<String, String>) -> MapPatch {
    if entries.is_empty() {
        return MapPatch::Clear;
    }
    MapPatch::Merge(
        entries
            .iter()
            .map(|(key, value)| (key.clone(), Some(value.clone())))
            .collect(),
    )
}

pub(super) fn validate(update: &ObjectMetadataUpdate) -> Result<(), Status> {
    update.validate().map_err(input_error)
}

pub(super) fn input_error(error: ObjectError) -> Status {
    // A class the bucket's location cannot hold names no field, so it keeps the shared mapping
    // and its BadRequest.
    if let ObjectError::Metadata(metadata) = &error
        && !matches!(metadata, MetadataError::ClassLocation(_))
    {
        let message = metadata.message(super::bucket_update::proto_field);
        return if metadata.unsupported() {
            super::status::unsupported(&format!("unsupported metadata change: {message}"))
        } else {
            Status::invalid_argument(message)
        };
    }
    super::object_io::object_status(error)
}

pub(super) fn retention(
    value: Option<&pb::object::Retention>,
) -> Result<Option<ObjectRetention>, Status> {
    let Some(value) = value else { return Ok(None) };
    let mode = match pb::object::retention::Mode::try_from(value.mode) {
        Ok(pb::object::retention::Mode::Unlocked) => RetentionMode::Unlocked,
        Ok(pb::object::retention::Mode::Locked) => RetentionMode::Locked,
        _ => {
            return Err(Status::invalid_argument(
                "retention.mode must be UNLOCKED or LOCKED",
            ));
        }
    };
    let time = value.retain_until_time.ok_or_else(|| {
        Status::invalid_argument("retention.retain_until_time is required with retention.mode")
    })?;
    let invalid = || {
        Status::invalid_argument("retention.retain_until_time must be a valid protobuf timestamp")
    };
    if !(YEAR_ONE_MICROS / 1_000_000..=RFC3339_MAX_MICROS / 1_000_000).contains(&time.seconds) {
        return Err(invalid());
    }
    let nanos = u32::try_from(time.nanos).map_err(|_| invalid())?;
    let retain_until = UnixNanos::new(time.seconds, nanos).ok_or_else(invalid)?;
    Ok(Some(ObjectRetention { mode, retain_until }))
}

/// The editable metadata a request's object sets, as a [`WriteMode::Replace`] update: for a new
/// generation (WriteObject, ComposeObject, a RewriteObject destination) and, under a `*` update
/// mask, for an existing one ([`replacement`]).
pub(super) fn for_write(object: &pb::Object) -> ObjectMetadataUpdate {
    // An empty proto3 string leaves the field unnamed (`Patch::Keep`) rather than cleared, and
    // upload replay fingerprints keep the bytes they had for it. Keeping is correct for both
    // uses only because the field starts unset: a new generation has no value yet, and a
    // `WriteMode::Replace` update clears an existing generation's content fields before it
    // applies (`ObjectMetadataUpdate::apply` in `src/object.rs`).
    let named = |value: &str| set(value).map_or(Patch::Keep, Patch::Set);
    ObjectMetadataUpdate {
        write: WriteMode::Replace,
        content_type: named(&object.content_type),
        content_encoding: named(&object.content_encoding),
        content_disposition: named(&object.content_disposition),
        content_language: named(&object.content_language),
        cache_control: named(&object.cache_control),
        // A merge of every value, not a replacement: there are no entries to keep, since a new
        // generation has none and `WriteMode::Replace` clears an existing one's first, and a
        // replacement would add the map-mode extension to the upload replay fingerprint.
        metadata: MapPatch::Merge(
            object
                .metadata
                .iter()
                .map(|(key, value)| (key.clone(), Some(value.clone())))
                .collect(),
        ),
        temporary_hold: object.temporary_hold.then_some(true),
        event_based_hold: object.event_based_hold,
        custom_time: object
            .custom_time
            .map_or(Patch::Keep, |time| Patch::Set(time.to_string())),
        ..Default::default()
    }
}

pub(super) fn inherited(source: &ObjectRecord) -> ObjectMetadataUpdate {
    ObjectMetadataUpdate {
        storage_class: Some(source.storage_class),
        ..ObjectMetadataUpdate::inheriting(source)
    }
}

/// A canned ACL field of a Storage v2 request, in the JSON API's spelling; empty is absent.
pub(super) fn canned_acl(field: &str, value: &str) -> Result<Option<PredefinedObjectAcl>, Status> {
    if value.is_empty() {
        return Ok(None);
    }
    value.parse().map(Some).map_err(|_| {
        Status::invalid_argument(format!(
            "{field} {value:?} must be authenticatedRead, bucketOwnerFullControl, bucketOwnerRead, private, projectPrivate or publicRead"
        ))
    })
}

fn set(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    #[test]
    fn overlapping_metadata_masks_are_actionable_in_both_orders() {
        for paths in [
            ["metadata", "metadata.missing"],
            ["metadata.missing", "metadata"],
        ] {
            let error = super::from_proto(&super::pb::Object::default(), &paths.map(String::from))
                .unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            assert_eq!(
                error.message(),
                "Use either metadata or metadata.KEY in update_mask"
            );
        }
    }
    use super::*;
    use prost::Message;

    #[test]
    fn update_paths_separate_output_only_unsupported_and_unknown_paths() {
        assert_eq!("metadata".parse(), Ok(UpdatePath::Metadata));
        assert_eq!(
            "metadata.a.b".parse(),
            Ok(UpdatePath::MetadataKey("a.b".into()))
        );
        assert_eq!("metadata.".parse(), Ok(UpdatePath::MetadataKey("".into())));
        // Recorded 2026-11-18 for `create_time` and `update_time`, and in case
        // grpc-probe-inferred-object-masks for the other fields the proto marks OUTPUT_ONLY and
        // for the fields that are not OUTPUT_ONLY but are accepted and ignored: identifiers and
        // the values the service keeps.
        for path in [
            "restore_token",
            "metageneration",
            "size",
            "delete_time",
            "finalize_time",
            "create_time",
            "component_count",
            "checksums",
            "update_time",
            "update_storage_class_time",
            "owner",
            "soft_delete_time",
            "hard_delete_time",
            "name",
            "bucket",
            "etag",
            "generation",
            "retention_expire_time",
        ] {
            assert_eq!(
                path.parse::<UpdatePath>(),
                Err(UnappliedUpdatePath::Ignored),
                "{path}"
            );
        }
        // `*` never reaches the path parser: `from_proto` takes it as a replacement first.
        for (path, unapplied) in [
            (
                "storage_class",
                UnappliedUpdatePath::Refused(STORAGE_CLASS_PATH),
            ),
            ("kms_key", UnappliedUpdatePath::Refused(KMS_KEY_PATH)),
            (
                "retention.mode",
                UnappliedUpdatePath::Unsupported("retention.mode".into()),
            ),
            (
                "owner.entity",
                UnappliedUpdatePath::Unsupported("owner.entity".into()),
            ),
            (
                "checksums.crc32c",
                UnappliedUpdatePath::Unsupported("checksums.crc32c".into()),
            ),
            (
                "retention_expire_time.seconds",
                UnappliedUpdatePath::Unsupported("retention_expire_time.seconds".into()),
            ),
            // Recorded 2026-11-18.
            (
                "no_such_field",
                UnappliedUpdatePath::Unknown("no_such_field".into()),
            ),
            (
                "no_such_field.entity",
                UnappliedUpdatePath::Unknown("no_such_field.entity".into()),
            ),
            ("", UnappliedUpdatePath::Unknown(String::new())),
        ] {
            assert_eq!(path.parse::<UpdatePath>(), Err(unapplied), "{path}");
        }
    }

    /// Recorded 2026-11-18: `*` with only a content type cleared the other content fields and
    /// the custom metadata.
    #[test]
    fn a_wildcard_mask_replaces_the_editable_metadata() {
        let object = pb::Object {
            content_type: "text/x-wildcard".into(),
            ..Default::default()
        };
        let update = from_proto(&object, &["*".into()]).unwrap();
        assert_eq!(update.write, WriteMode::Replace);
        assert_eq!(update.content_type, Patch::Set("text/x-wildcard".into()));
        assert_eq!(update.acl, AclRequest::Omitted);
        assert_eq!(update.retention, Patch::Keep);
        let listed = pb::Object {
            acl: vec![pb::ObjectAccessControl {
                entity: "allUsers".into(),
                role: "READER".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let update = from_proto(&listed, &["*".into()]).unwrap();
        assert_eq!(update.acl.entries().map(<[_]>::len), Some(1));
    }

    /// `*` beside another path is a replacement, and the other paths are not read: a path that
    /// alone is refused, or that names no field, changes nothing beside it. Not recorded;
    /// inferred from the recorded `*` beside `content_language` (case
    /// grpc-probe-inferred-object-masks), whose answer reading the paths would not change.
    #[test]
    fn a_wildcard_beside_another_path_does_not_read_that_path() {
        let object = pb::Object {
            content_type: "text/plain".into(),
            storage_class: "NEARLINE".into(),
            kms_key: "projects/p/locations/l/keyRings/r/cryptoKeys/missing".into(),
            ..Default::default()
        };
        for other in ["storage_class", "kms_key", "no_such_field", "labels.x"] {
            let update = from_proto(&object, &["*".into(), other.into()])
                .unwrap_or_else(|error| panic!("{other}: {error}"));
            assert_eq!(update.write, WriteMode::Replace, "{other}");
            assert_eq!(
                update.content_type,
                Patch::Set("text/plain".into()),
                "{other}"
            );
            assert_eq!(update.storage_class, None, "{other}");
        }
        // Alone, each of those paths is refused.
        for path in ["storage_class", "kms_key", "no_such_field"] {
            assert_eq!(
                from_proto(&object, &[path.into()]).unwrap_err().code(),
                tonic::Code::InvalidArgument,
                "{path}"
            );
        }
    }

    #[test]
    fn object_updates_preserve_map_modes_and_explicit_resets() {
        let object = pb::Object {
            metadata: [("keep".into(), "value".into())].into(),
            ..Default::default()
        };
        let update = from_proto(
            &object,
            &[
                "metadata".into(),
                "content_language".into(),
                "retention".into(),
            ],
        )
        .unwrap();
        assert_eq!(
            update.metadata,
            MapPatch::Merge([("keep".into(), Some("value".into()))].into())
        );
        assert_eq!(update.content_language, Patch::Clear);
        assert_eq!(update.retention, Patch::Clear);
        let update = from_proto(&object, &["metadata.missing".into()]).unwrap();
        assert_eq!(
            update.metadata,
            MapPatch::Merge([("missing".into(), None)].into())
        );
    }

    #[test]
    fn a_metadata_mask_merges_entries_clears_on_none_and_names_the_empty_key() {
        let cleared = from_proto(&pb::Object::default(), &["metadata".into()]).unwrap();
        assert_eq!(cleared.metadata, MapPatch::Clear);
        let object = pb::Object {
            metadata: [(String::new(), "empty".into())].into(),
            ..Default::default()
        };
        for path in ["metadata", "metadata."] {
            let update = from_proto(&object, &[path.into()]).unwrap();
            assert_eq!(
                update.metadata,
                MapPatch::Merge([(String::new(), Some("empty".into()))].into()),
                "{path}"
            );
        }
    }

    #[test]
    fn object_validation_uses_proto_field_names_and_keeps_error_details() {
        let object = pb::Object {
            content_type: "invalid\r\nvalue".into(),
            ..Default::default()
        };
        let error = from_proto(&object, &["content_type".into()]).unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(error.message().contains("content_type"));
        assert!(!error.message().contains("contentType"));
        let object = pb::Object {
            content_type: "text/日本語".into(),
            ..Default::default()
        };
        let error = from_proto(&object, &["content_type".into()]).unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unimplemented);
        let details = crate::grpc::generated::google::rpc::Status::decode(error.details()).unwrap();
        assert_eq!(
            details.details[0].type_url,
            "type.googleapis.com/google.rpc.ErrorInfo"
        );
    }

    #[test]
    fn retention_validates_timestamp_bounds_and_keeps_nanoseconds() {
        let value = |seconds, nanos| pb::object::Retention {
            mode: pb::object::retention::Mode::Unlocked as i32,
            retain_until_time: Some(prost_types::Timestamp { seconds, nanos }),
        };
        for (seconds, nanos) in [
            (0, -1),
            (0, 1_000_000_000),
            (-62_135_596_801, 0),
            (253_402_300_800, 0),
        ] {
            let error = retention(Some(&value(seconds, nanos))).unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            assert!(error.message().contains("retain_until_time"));
        }
        let result = retention(Some(&value(2_000_000_000, 123_456_789)))
            .unwrap()
            .unwrap();
        assert_eq!(result.retain_until.subsec_nanos(), 123_456_789);
    }
}
