//! The `Object` fields a GetObject, ReadObject or ListObjects read mask selects. A mask is parsed
//! once, with the request, so an invalid path is reported before anything is looked up; applying
//! a parsed mask cannot fail.

use crate::grpc::v2::{
    mask::{FieldSetting, ReadMask, ResourceField, UnselectableField},
    pb,
};

// `macro_rules!` cannot capitalize an identifier, so each field names its variant.
macro_rules! object_fields {
    ($($field:ident => $variant:ident),+ $(,)?) => {
        /// A top-level `Object` field.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(in crate::grpc::v2) enum ObjectField {
            $($variant),+
        }

        impl std::str::FromStr for ObjectField {
            type Err = UnselectableField;

            fn from_str(name: &str) -> Result<Self, Self::Err> {
                match name {
                    $(stringify!($field) => Ok(Self::$variant),)+
                    _ => Err(UnselectableField),
                }
            }
        }

        impl ObjectField {
            fn take(self, from: &mut pb::Object, to: &mut pb::Object) {
                match self {
                    $(Self::$variant => to.$field = std::mem::take(&mut from.$field),)+
                }
            }

            /// Resets every field of `object` whose value only the service supplies: the
            /// `OUTPUT_ONLY` fields and the ones the service keeps ([`FieldSetting`]). An
            /// identifier stays, since a request names the object with it.
            pub(in crate::grpc::v2) fn clear_service_set(object: &mut pb::Object) {
                $(
                    if matches!(
                        Self::$variant.setting(),
                        FieldSetting::OutputOnly | FieldSetting::ServiceKept
                    ) {
                        object.$field = Default::default();
                    }
                )+
            }
        }
    };
}

object_fields!(
    name => Name,
    bucket => Bucket,
    etag => Etag,
    generation => Generation,
    restore_token => RestoreToken,
    metageneration => Metageneration,
    storage_class => StorageClass,
    size => Size,
    content_encoding => ContentEncoding,
    content_disposition => ContentDisposition,
    cache_control => CacheControl,
    acl => Acl,
    content_language => ContentLanguage,
    delete_time => DeleteTime,
    finalize_time => FinalizeTime,
    content_type => ContentType,
    create_time => CreateTime,
    component_count => ComponentCount,
    checksums => Checksums,
    update_time => UpdateTime,
    kms_key => KmsKey,
    update_storage_class_time => UpdateStorageClassTime,
    temporary_hold => TemporaryHold,
    retention_expire_time => RetentionExpireTime,
    metadata => Metadata,
    contexts => Contexts,
    event_based_hold => EventBasedHold,
    owner => Owner,
    customer_encryption => CustomerEncryption,
    custom_time => CustomTime,
    soft_delete_time => SoftDeleteTime,
    hard_delete_time => HardDeleteTime,
    retention => Retention,
);

macro_rules! object_members {
    ($($field:ident . $member:ident => $variant:ident),+ $(,)?) => {
        /// A member of one of `Object`'s singular messages, which keeps that member alone.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(super) enum ObjectMember {
            $($variant),+
        }

        impl ObjectMember {
            fn named(field: &str, member: &str) -> Option<Self> {
                match (field, member) {
                    $((stringify!($field), stringify!($member)) => Some(Self::$variant),)+
                    _ => None,
                }
            }

            /// Copies the member into `to`. A message `from` no longer holds was taken whole
            /// by a path naming its field, or was never set.
            fn copy(self, from: &pb::Object, to: &mut pb::Object) {
                match self {
                    $(Self::$variant => {
                        if let Some(message) = &from.$field {
                            to.$field.get_or_insert_default().$member =
                                Clone::clone(&message.$member);
                        }
                    })+
                }
            }
        }
    };
}

object_members!(
    // The Java SDK lists with both digests.
    checksums.crc32c => ChecksumsCrc32c,
    checksums.md5_hash => ChecksumsMd5Hash,
    owner.entity => OwnerEntity,
    owner.entity_id => OwnerEntityId,
    customer_encryption.encryption_algorithm => CustomerEncryptionAlgorithm,
    customer_encryption.key_sha256_bytes => CustomerEncryptionKeySha256Bytes,
    retention.mode => RetentionMode,
    retention.retain_until_time => RetentionRetainUntilTime,
    // A part of a timestamp is selected as a message member: `create_time.seconds` answered a
    // timestamp of whole seconds (recorded in case grpc-probe-inferred-object-masks). `nanos`
    // and the other timestamp fields follow by inference.
    delete_time.seconds => DeleteTimeSeconds,
    delete_time.nanos => DeleteTimeNanos,
    finalize_time.seconds => FinalizeTimeSeconds,
    finalize_time.nanos => FinalizeTimeNanos,
    create_time.seconds => CreateTimeSeconds,
    create_time.nanos => CreateTimeNanos,
    update_time.seconds => UpdateTimeSeconds,
    update_time.nanos => UpdateTimeNanos,
    update_storage_class_time.seconds => UpdateStorageClassTimeSeconds,
    update_storage_class_time.nanos => UpdateStorageClassTimeNanos,
    retention_expire_time.seconds => RetentionExpireTimeSeconds,
    retention_expire_time.nanos => RetentionExpireTimeNanos,
    custom_time.seconds => CustomTimeSeconds,
    custom_time.nanos => CustomTimeNanos,
    soft_delete_time.seconds => SoftDeleteTimeSeconds,
    soft_delete_time.nanos => SoftDeleteTimeNanos,
    hard_delete_time.seconds => HardDeleteTimeSeconds,
    hard_delete_time.nanos => HardDeleteTimeNanos,
    // No object holds contexts, since every write path refuses them, so the map is empty, as
    // Cloud Storage answered for an object without contexts (recorded in the same case).
    contexts.custom => ContextsCustom,
);

impl ResourceField for ObjectField {
    fn output_only(self) -> bool {
        self.setting() == FieldSetting::OutputOnly
    }

    fn setting(self) -> FieldSetting {
        use ObjectField as F;
        match self {
            F::RestoreToken
            | F::Metageneration
            | F::Size
            | F::DeleteTime
            | F::FinalizeTime
            | F::CreateTime
            | F::ComponentCount
            | F::Checksums
            | F::UpdateTime
            | F::UpdateStorageClassTime
            | F::Owner
            | F::SoftDeleteTime
            | F::HardDeleteTime => FieldSetting::OutputOnly,
            F::Name | F::Bucket => FieldSetting::Identifier,
            // Not marked OUTPUT_ONLY, yet a client value changes nothing. UpdateObject took
            // `etag`, `generation` and `retention_expire_time` in its update mask, and
            // ComposeObject and RewriteObject took an etag in the destination, without
            // changing anything (recorded in cases grpc-probe-inferred-object-masks and
            // grpc-probe-inferred-destinations). The generation is refused by RewriteObject
            // alone. A destination that carries a `retention_expire_time` is dropped the same
            // way by inference.
            F::Etag | F::Generation | F::RetentionExpireTime => FieldSetting::ServiceKept,
            F::StorageClass
            | F::ContentEncoding
            | F::ContentDisposition
            | F::CacheControl
            | F::Acl
            | F::ContentLanguage
            | F::ContentType
            | F::KmsKey
            | F::TemporaryHold
            | F::Metadata
            | F::Contexts
            | F::EventBasedHold
            | F::CustomerEncryption
            | F::CustomTime
            | F::Retention => FieldSetting::Client,
        }
    }
}

/// Whether `member` is a real member of `field` that gcs-mimic does not select on its own: the
/// parts of the retention's timestamp. Cloud Storage's answer to them is not recorded.
///
/// Every other path that is not a listed member names no field: a scalar has no members, and
/// under a repeated field, a map or a member Cloud Storage named no field for `acl.role`
/// (recorded 2026-11-18) and for `metadata.somekey`, `name.x`, `owner.entity.x`, an empty path,
/// a trailing dot and a leading dot (recorded in case grpc-probe-inferred-object-masks), all on
/// GetObject.
fn unprojected_member(field: ObjectField, member: &str) -> bool {
    field == ObjectField::Retention
        && matches!(
            member,
            "retain_until_time.seconds" | "retain_until_time.nanos"
        )
}

/// One path of a read mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ObjectPath {
    Field(ObjectField),
    Member(ObjectMember),
}

/// A read mask path that selects nothing.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum UnselectablePath {
    /// The path names no field of the resource: an unknown name, a member its message does not
    /// have, or anything under a scalar, a repeated field or a map. On GetObject Cloud Storage
    /// answered so for `no_such_field`, `owner.no_such_field` and `acl.role` (recorded
    /// 2026-11-18) and for a map key, a path under a scalar or a member, and an empty, trailing
    /// or leading dot path (recorded in case grpc-probe-inferred-object-masks).
    Unknown(String),
    /// A member that exists but that gcs-mimic does not select apart from its field.
    Unsupported(String),
}

impl std::fmt::Display for UnselectablePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown(path) => write!(f, "read mask path {path} names no object field"),
            Self::Unsupported(path) => write!(f, "read mask path {path} is not selectable"),
        }
    }
}

impl std::error::Error for UnselectablePath {}

impl std::str::FromStr for ObjectPath {
    type Err = UnselectablePath;

    fn from_str(path: &str) -> Result<Self, Self::Err> {
        let (name, member) = match path.split_once('.') {
            Some((name, member)) => (name, Some(member)),
            None => (path, None),
        };
        let Ok(field) = name.parse::<ObjectField>() else {
            return Err(UnselectablePath::Unknown(path.into()));
        };
        let Some(member) = member else {
            return Ok(Self::Field(field));
        };
        if let Some(member) = ObjectMember::named(name, member) {
            return Ok(Self::Member(member));
        }
        Err(if unprojected_member(field, member) {
            UnselectablePath::Unsupported(path.into())
        } else {
            UnselectablePath::Unknown(path.into())
        })
    }
}

/// A parsed read mask over `Object`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ObjectMask {
    /// An absent or empty mask, or one that names `*`: every field the RPC returns.
    Everything,
    /// The named fields and members; everything else takes its default.
    Paths(Vec<ObjectPath>),
}

impl TryFrom<ReadMask<'_>> for ObjectMask {
    type Error = UnselectablePath;

    fn try_from(mask: ReadMask<'_>) -> Result<Self, Self::Error> {
        let Some(paths) = mask.selective_paths() else {
            return Ok(Self::Everything);
        };
        paths
            .iter()
            .map(|path| path.parse())
            .collect::<Result<_, _>>()
            .map(Self::Paths)
    }
}

impl ObjectMask {
    /// Keeps what the mask names. ACL and owner are filled beforehand, and only for a path
    /// that names the whole field: Cloud Storage answered `owner.entity` without an owner
    /// (recorded 2026-11-18). Fields the local object cannot hold (KMS key, contexts, the
    /// restore token of a live object) stay empty.
    pub(super) fn apply(&self, mut object: pb::Object) -> pb::Object {
        let Self::Paths(paths) = self else {
            return object;
        };
        let mut selected = pb::Object::default();
        for path in paths {
            match path {
                ObjectPath::Field(field) => field.take(&mut object, &mut selected),
                ObjectPath::Member(_) => {}
            }
        }
        for path in paths {
            match path {
                ObjectPath::Member(member) => member.copy(&object, &mut selected),
                ObjectPath::Field(_) => {}
            }
        }
        selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(paths: &[&str]) -> Result<ObjectMask, UnselectablePath> {
        let paths: Vec<String> = paths.iter().map(|&path| path.to_owned()).collect();
        ObjectMask::try_from(ReadMask::from(paths.as_slice()))
    }

    fn object() -> pb::Object {
        pb::Object {
            name: "object".into(),
            size: 5,
            checksums: Some(pb::ObjectChecksums {
                crc32c: Some(7),
                md5_hash: vec![1; 16],
            }),
            owner: Some(pb::Owner {
                entity: "user-a".into(),
                entity_id: "id".into(),
            }),
            retention: Some(pb::object::Retention {
                mode: pb::object::retention::Mode::Unlocked as i32,
                retain_until_time: Some(prost_types::Timestamp {
                    seconds: 9,
                    nanos: 1,
                }),
            }),
            metadata: [("k".into(), "v".into())].into(),
            ..Default::default()
        }
    }

    #[test]
    fn masks_without_selective_paths_keep_every_field() {
        for paths in [&[][..], &["*"], &["*", "name"]] {
            let mask = parse(paths).unwrap();
            assert_eq!(mask, ObjectMask::Everything);
            assert_eq!(mask.apply(object()), object());
        }
    }

    #[test]
    fn a_member_path_keeps_that_member_alone() {
        let selected = parse(&["name", "checksums.crc32c", "owner.entity"])
            .unwrap()
            .apply(object());
        assert_eq!(
            selected,
            pb::Object {
                name: "object".into(),
                checksums: Some(pb::ObjectChecksums {
                    crc32c: Some(7),
                    md5_hash: Default::default(),
                }),
                owner: Some(pb::Owner {
                    entity: "user-a".into(),
                    entity_id: String::new(),
                }),
                ..Default::default()
            }
        );
        let retention = parse(&["retention.retain_until_time"])
            .unwrap()
            .apply(object())
            .retention
            .unwrap();
        assert_eq!(retention.mode, 0);
        assert_eq!(retention.retain_until_time.unwrap().seconds, 9);
    }

    #[test]
    fn a_field_path_keeps_the_whole_message_beside_its_member_paths() {
        for paths in [
            &["owner", "owner.entity"][..],
            &["owner.entity", "owner"],
            &["owner.entity", "owner.entity_id"],
        ] {
            let selected = parse(paths).unwrap().apply(object());
            assert_eq!(selected.owner, object().owner, "{paths:?}");
        }
    }

    /// A message the object does not carry stays absent, which is how GetObject answers
    /// `owner.entity`: the owner is filled only for a path naming the whole field.
    #[test]
    fn a_member_of_an_absent_message_selects_nothing() {
        let mut ownerless = object();
        ownerless.owner = None;
        let selected = parse(&["owner.entity"]).unwrap().apply(ownerless);
        assert_eq!(selected, pb::Object::default());
    }

    #[test]
    fn paths_that_name_no_field_are_unknown() {
        for path in [
            // Recorded 2026-11-18 on GetObject.
            "no_such_field",
            "owner.no_such_field",
            "acl.role",
            // Recorded in case grpc-probe-inferred-object-masks: a map key, a path under a
            // member or a scalar, and an empty segment.
            "owner.entity.x",
            "name.x",
            "metadata.somekey",
            "checksums.",
            "",
            ".name",
            // Not recorded: inferred for a path under an unknown name, a map of contexts or a
            // part of a timestamp.
            "no_such_field.entity",
            "contexts.custom.key",
            "create_time.seconds.x",
        ] {
            assert_eq!(
                parse(&["name", path]),
                Err(UnselectablePath::Unknown(path.into())),
                "{path}"
            );
        }
    }

    /// `create_time.seconds` answered a timestamp of whole seconds, and `contexts.custom` an
    /// empty message, in case grpc-probe-inferred-object-masks.
    #[test]
    fn a_part_of_a_timestamp_and_the_context_map_are_selected_as_members() {
        let mut stamped = object();
        stamped.create_time = Some(prost_types::Timestamp {
            seconds: 5,
            nanos: 6,
        });
        let selected = parse(&[
            "create_time.seconds",
            "custom_time.nanos",
            "contexts.custom",
        ])
        .unwrap()
        .apply(stamped);
        assert_eq!(
            selected,
            pb::Object {
                create_time: Some(prost_types::Timestamp {
                    seconds: 5,
                    nanos: 0,
                }),
                ..Default::default()
            }
        );
    }

    #[test]
    fn real_members_that_are_not_selected_alone_are_unsupported() {
        for path in [
            "retention.retain_until_time.seconds",
            "retention.retain_until_time.nanos",
        ] {
            assert_eq!(
                parse(&[path]),
                Err(UnselectablePath::Unsupported(path.into())),
                "{path}"
            );
        }
        assert_eq!(
            parse(&["create_time.no_such_field"]),
            Err(UnselectablePath::Unknown(
                "create_time.no_such_field".into()
            ))
        );
    }
}
