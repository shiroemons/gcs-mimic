//! Read masks as each Storage v2 RPC interprets them. Protobuf gives an absent mask and one
//! without paths the same meaning, the RPC's default fields, and `*` names every field.

use super::{convert::Lifecycle, pb, status};
use prost_types::FieldMask;
use std::{collections::BTreeSet, str::FromStr};
use tonic::Status;

/// A read mask classified by how it uses `*`; each RPC applies its own rule to the classes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReadMask<'a> {
    /// Absent, or present without paths: the RPC's default fields.
    Default,
    /// Exactly one path, `*`.
    Wildcard,
    /// `*` beside other paths. Object reads take it as every field; bucket reads and
    /// BidiReadObject reject it.
    MixedWildcard(&'a [String]),
    /// Paths without `*`, at least one.
    Paths(&'a [String]),
}

impl<'a> From<&'a [String]> for ReadMask<'a> {
    fn from(paths: &'a [String]) -> Self {
        match paths {
            [] => Self::Default,
            [only] if only == "*" => Self::Wildcard,
            paths if paths.iter().any(|path| path == "*") => Self::MixedWildcard(paths),
            paths => Self::Paths(paths),
        }
    }
}

impl<'a> From<Option<&'a FieldMask>> for ReadMask<'a> {
    fn from(mask: Option<&'a FieldMask>) -> Self {
        mask.map_or(Self::Default, |mask| Self::from(mask.paths.as_slice()))
    }
}

impl<'a> ReadMask<'a> {
    /// The paths of a mask that selects fields, or `None` when it keeps every field it governs
    /// (an object read takes a wildcard among other paths as every field).
    pub(super) fn selective_paths(self) -> Option<&'a [String]> {
        match self {
            Self::Paths(paths) => Some(paths),
            Self::Default | Self::Wildcard | Self::MixedWildcard(_) => None,
        }
    }
}

/// A top-level field of a Storage v2 resource message, parsed from its proto name.
pub(super) trait ResourceField: FromStr + Copy {
    /// Names of the resource's fields that are no variant because no mask selects them.
    const UNSELECTABLE: &[&str] = &[];

    /// Who supplies the field's value. The only classification of a field: each resource
    /// answers with an exhaustive `match`, so a new field is classified where it is added.
    fn setting(self) -> FieldSetting;
}

/// Who supplies a resource field's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FieldSetting {
    /// A client sets it.
    Client,
    /// The proto marks it `OUTPUT_ONLY`: only the service sets it.
    OutputOnly,
    /// Names or addresses the resource (name, bucket): a request supplies it to select the
    /// resource, so an update that names it changes nothing.
    Identifier,
    /// The proto marks it `IMMUTABLE` (a bucket's project and location): a client sets it when
    /// it creates the resource, and an update that names it changes nothing.
    Immutable,
    /// The proto does not mark it `OUTPUT_ONLY`, but the service keeps its value (the generation,
    /// the etag, the retention expiry): a client value is accepted and ignored.
    ServiceKept,
}

impl FieldSetting {
    /// Whether only the service supplies the value, so a request's value is dropped from a
    /// resource the service creates (a copy destination). An identifier names the resource and
    /// an immutable field is set at creation, so a request keeps both.
    pub(super) fn service_supplied(self) -> bool {
        match self {
            Self::OutputOnly | Self::ServiceKept => true,
            Self::Client | Self::Identifier | Self::Immutable => false,
        }
    }
}

/// Whether the first segment of `path` names a field of `F`'s resource, whether or not a mask
/// selects it.
fn starts_with_field<F: ResourceField>(path: &str) -> bool {
    let name = path.split_once('.').map_or(path, |(name, _)| name);
    F::UNSELECTABLE.contains(&name) || name.parse::<F>().is_ok()
}

/// A path that is no top-level field gcs-mimic selects, nested paths included.
#[derive(Debug, thiserror::Error)]
#[error("path is no selectable top-level field")]
pub(super) struct UnselectableField;

// `macro_rules!` cannot capitalize an identifier, so each field names its variant.
macro_rules! bucket_fields {
    ($($field:ident => $variant:ident),+ $(,)?) => {
        /// A top-level `Bucket` field a read mask can select.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub(super) enum BucketField {
            $($variant),+
        }

        impl std::str::FromStr for BucketField {
            type Err = UnselectableField;

            fn from_str(path: &str) -> Result<Self, Self::Err> {
                match path {
                    $(stringify!($field) => Ok(Self::$variant),)+
                    _ => Err(UnselectableField),
                }
            }
        }

        impl BucketField {
            fn take(self, from: &mut pb::Bucket, to: &mut pb::Bucket) {
                match self {
                    $(Self::$variant => to.$field = std::mem::take(&mut from.$field),)+
                }
            }
        }
    };
}

bucket_fields!(
    name => Name,
    bucket_id => BucketId,
    etag => Etag,
    project => Project,
    metageneration => Metageneration,
    location => Location,
    location_type => LocationType,
    storage_class => StorageClass,
    autoclass => Autoclass,
    labels => Labels,
    create_time => CreateTime,
    update_time => UpdateTime,
    default_event_based_hold => DefaultEventBasedHold,
    versioning => Versioning,
    cors => Cors,
    lifecycle => Lifecycle,
    retention_policy => RetentionPolicy,
    soft_delete_policy => SoftDeletePolicy,
    object_retention => ObjectRetention,
    iam_config => IamConfig,
    acl => Acl,
    default_object_acl => DefaultObjectAcl,
    owner => Owner,
    // Bucket creation and update reject these settings, so a stored bucket never has them and
    // selecting one returns it unset, as Cloud Storage answers for a bucket without the setting
    // and as a read without a mask already does. The Java SDK names every field in its default
    // mask, so refusing them would break every GetBucket.
    rpo => Rpo,
    website => Website,
    logging => Logging,
    encryption => Encryption,
    billing => Billing,
    custom_placement_config => CustomPlacementConfig,
    hierarchical_namespace => HierarchicalNamespace,
    ip_filter => IpFilter,
);

impl ResourceField for BucketField {
    /// `satisfies_pzs`, which no mask selects because the emulator has no value to answer with.
    const UNSELECTABLE: &[&str] = &["satisfies_pzs"];

    fn setting(self) -> FieldSetting {
        use BucketField as F;
        match self {
            F::BucketId
            | F::Metageneration
            | F::LocationType
            | F::CreateTime
            | F::UpdateTime
            | F::Owner => FieldSetting::OutputOnly,
            // UpdateBucket accepted each of these four in its update mask and changed nothing,
            // even with the request's etag or with the values the bucket already has (recorded
            // 2026-11-18, case grpc-probe-inferred-bucket-masks).
            F::Name => FieldSetting::Identifier,
            F::Project | F::Location => FieldSetting::Immutable,
            F::Etag => FieldSetting::ServiceKept,
            F::StorageClass
            | F::Autoclass
            | F::Labels
            | F::DefaultEventBasedHold
            | F::Versioning
            | F::Cors
            | F::Lifecycle
            | F::RetentionPolicy
            | F::SoftDeletePolicy
            | F::ObjectRetention
            | F::IamConfig
            | F::Acl
            | F::DefaultObjectAcl
            | F::Rpo
            | F::Website
            | F::Logging
            | F::Encryption
            | F::Billing
            | F::CustomPlacementConfig
            | F::HierarchicalNamespace
            | F::IpFilter => FieldSetting::Client,
        }
    }
}

/// Whether a read without a mask includes the ACL fields: GetBucket defaults to all fields,
/// ListBuckets to all fields except `acl`, `default_object_acl` and `owner`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AclDefault {
    Include,
    Exclude,
}

/// A GetBucket or ListBuckets read mask, validated before authorization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum BucketMask {
    /// No mask, or one without paths.
    Default,
    /// `*` alone.
    All,
    /// The listed fields, each once.
    Fields(BTreeSet<BucketField>),
}

impl BucketMask {
    pub(super) fn parse(mask: Option<&FieldMask>) -> Result<Self, Status> {
        match ReadMask::from(mask) {
            ReadMask::Default => Ok(Self::Default),
            ReadMask::Wildcard => Ok(Self::All),
            // Cloud Storage's wording, recorded by case grpc-unsupported-options.
            ReadMask::MixedWildcard(paths) => Err(Status::invalid_argument(format!(
                "Field masks may either contain only a single '*' field or specify specific fields, but not both. Got: '{}'.",
                paths.join(",")
            ))),
            ReadMask::Paths(paths) => paths
                .iter()
                .map(|path| {
                    path.parse().map_err(|UnselectableField| {
                        // A path under a `Bucket` field is not selected yet, whether or not
                        // the field has that member; only an unknown first segment is known
                        // to name nothing.
                        if starts_with_field::<BucketField>(path) {
                            status::unsupported(&format!("bucket read_mask {path}"))
                        } else {
                            unknown_field(path)
                        }
                    })
                })
                .collect::<Result<_, _>>()
                .map(Self::Fields),
        }
    }

    /// Whether the response carries `acl`, `default_object_acl` and `owner`.
    pub(super) fn includes_acl(&self, default: AclDefault) -> bool {
        match self {
            Self::Default => default == AclDefault::Include,
            Self::All => true,
            Self::Fields(fields) => [
                BucketField::Acl,
                BucketField::DefaultObjectAcl,
                BucketField::Owner,
            ]
            .iter()
            .any(|field| fields.contains(field)),
        }
    }

    /// Whether the resource is built with its lifecycle rules.
    pub(super) fn lifecycle(&self) -> Lifecycle {
        match self {
            Self::Default | Self::All => Lifecycle::Include,
            Self::Fields(fields) if fields.contains(&BucketField::Lifecycle) => Lifecycle::Include,
            Self::Fields(_) => Lifecycle::Omit,
        }
    }

    /// Keeps the selected fields; every other field takes its default.
    pub(super) fn apply(&self, mut bucket: pb::Bucket) -> pb::Bucket {
        let Self::Fields(fields) = self else {
            return bucket;
        };
        let mut selected = pb::Bucket::default();
        for field in fields {
            field.take(&mut bucket, &mut selected);
        }
        selected
    }
}

/// The paths of an update mask. Cloud Storage refuses an absent mask and one without paths
/// alike (recorded 2026-11-18 on UpdateBucket and UpdateObject).
pub(super) fn mask(mask: Option<&prost_types::FieldMask>) -> Result<&[String], Status> {
    mask.filter(|m| !m.paths.is_empty())
        .map(|m| m.paths.as_slice())
        .ok_or_else(|| {
            status::invalid_with_reason(
                "Invalid update_mask. Requests should specify at least one update field.",
                "GRPC_MISSING_UPDATE_MASK_FIELDS",
            )
        })
}

/// An `update_mask` path an update RPC does not apply.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum UnappliedUpdatePath {
    /// A field named without a member path whose value no update changes: every
    /// [`FieldSetting`] but [`FieldSetting::Client`]. It is accepted and changes nothing.
    ///
    /// Recorded for output-only fields: Cloud Storage accepted `create_time` and `update_time` on
    /// UpdateBucket and UpdateObject and applied nothing from them (2026-11-18; UpdateObject still
    /// advanced the metageneration), and UpdateObject took every other `OUTPUT_ONLY` field the
    /// same way (case grpc-probe-inferred-object-masks), as UpdateBucket took `bucket_id`,
    /// `metageneration` and `owner` (case grpc-probe-inferred-bucket-masks; its `location_type`
    /// answer is the declared difference of that case).
    /// Recorded for the others: UpdateObject took `etag`, `generation`, `retention_expire_time`,
    /// `name` and `bucket` (the same case), and UpdateBucket took `name`, `etag`, `project` and
    /// `location` (case grpc-probe-inferred-bucket-masks).
    Ignored,
    /// A path the RPC recognizes and refuses whatever value the request carries, answered
    /// INVALID_ARGUMENT with a BadRequest that repeats this message.
    Refused(&'static str),
    /// A field of the resource, or a path under one, that gcs-mimic does not update, and `*`
    /// beside other paths.
    Unsupported(String),
    /// A path whose first segment names no field of the resource.
    Unknown(String),
}

impl UnappliedUpdatePath {
    /// Classifies a path none of the RPC's update paths takes by the fields of `F`'s resource.
    pub(super) fn of<F: ResourceField>(path: &str) -> Self {
        match path.parse::<F>().ok().map(F::setting) {
            Some(
                FieldSetting::OutputOnly
                | FieldSetting::Identifier
                | FieldSetting::Immutable
                | FieldSetting::ServiceKept,
            ) => Self::Ignored,
            Some(FieldSetting::Client) | None if path == "*" || starts_with_field::<F>(path) => {
                Self::Unsupported(path.into())
            }
            Some(FieldSetting::Client) | None => Self::Unknown(path.into()),
        }
    }
}

/// The update paths `P` an update mask names, parsed in mask order as they are taken, so the
/// first failing path names the error whether it is refused or its value is invalid. A field no
/// update changes is skipped ([`UnappliedUpdatePath::Ignored`]). Every other unapplied path is
/// refused: one the RPC refuses whatever the request carries in its own words, one under a field
/// gcs-mimic does not update naming `rpc` and the paths it does update (`remedy`), and one that
/// names no field as Cloud Storage words it.
pub(super) fn update_paths<'a, P>(
    paths: &'a [String],
    rpc: &'a str,
    remedy: &'a str,
) -> impl Iterator<Item = Result<P, Status>> + 'a
where
    P: FromStr<Err = UnappliedUpdatePath>,
{
    paths.iter().filter_map(move |path| match path.parse() {
        Ok(path) => Some(Ok(path)),
        Err(UnappliedUpdatePath::Ignored) => None,
        Err(UnappliedUpdatePath::Refused(message)) => Some(Err(status::invalid_field(message))),
        Err(UnappliedUpdatePath::Unsupported(path)) => Some(Err(status::unsupported(&format!(
            "{rpc} update_mask {path}; use {remedy}"
        )))),
        Err(UnappliedUpdatePath::Unknown(path)) => Some(Err(unknown_field(&path))),
    })
}

/// Cloud Storage's answer to a mask path that names no field of the resource (recorded
/// 2026-11-18 on GetBucket, ListBuckets, GetObject, UpdateBucket and UpdateObject).
pub(super) fn unknown_field(path: &str) -> Status {
    Status::invalid_argument(format!(
        "There is no field for this resource named '{path}'."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grpc::test_support::assert_unsupported;

    fn paths(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|&path| path.to_owned()).collect()
    }

    #[test]
    fn read_masks_classify_the_wildcard() {
        assert_eq!(ReadMask::from(None), ReadMask::Default);
        let empty = FieldMask::default();
        assert_eq!(ReadMask::from(Some(&empty)), ReadMask::Default);
        let only = paths(&["*"]);
        assert_eq!(ReadMask::from(only.as_slice()), ReadMask::Wildcard);
        let twice = paths(&["*", "*"]);
        assert_eq!(
            ReadMask::from(twice.as_slice()),
            ReadMask::MixedWildcard(&twice)
        );
        let mixed = paths(&["*", "name"]);
        assert_eq!(
            ReadMask::from(mixed.as_slice()),
            ReadMask::MixedWildcard(&mixed)
        );
        let named = paths(&["name"]);
        assert_eq!(ReadMask::from(named.as_slice()), ReadMask::Paths(&named));
        assert_eq!(
            ReadMask::Paths(&named).selective_paths(),
            Some(named.as_slice())
        );
        for keeps_all in [
            ReadMask::Default,
            ReadMask::Wildcard,
            ReadMask::MixedWildcard(&mixed),
        ] {
            assert_eq!(keeps_all.selective_paths(), None);
        }
    }

    fn bucket_mask(paths: &[&str]) -> Result<BucketMask, Status> {
        BucketMask::parse(Some(&FieldMask {
            paths: self::paths(paths),
        }))
    }

    #[test]
    fn bucket_masks_reject_mixed_wildcards_and_nested_paths() {
        assert_eq!(BucketMask::parse(None).unwrap(), BucketMask::Default);
        assert_eq!(bucket_mask(&[]).unwrap(), BucketMask::Default);
        assert_eq!(bucket_mask(&["*"]).unwrap(), BucketMask::All);
        assert_eq!(
            bucket_mask(&["name", "name", "acl"]).unwrap(),
            BucketMask::Fields(BTreeSet::from([BucketField::Name, BucketField::Acl]))
        );
        let mixed = bucket_mask(&["*", "name"]).unwrap_err();
        assert_eq!(mixed.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            mixed.message(),
            "Field masks may either contain only a single '*' field or specify specific fields, but not both. Got: '*,name'."
        );
        assert_unsupported(
            bucket_mask(&["name", "versioning.enabled"]),
            "bucket read_mask versioning.enabled",
        );
    }

    #[test]
    fn bucket_masks_select_acl_lifecycle_and_fields() {
        assert!(BucketMask::Default.includes_acl(AclDefault::Include));
        assert!(!BucketMask::Default.includes_acl(AclDefault::Exclude));
        assert!(BucketMask::All.includes_acl(AclDefault::Exclude));
        for field in ["acl", "default_object_acl", "owner"] {
            assert!(
                bucket_mask(&["name", field])
                    .unwrap()
                    .includes_acl(AclDefault::Exclude)
            );
        }
        assert!(
            !bucket_mask(&["name"])
                .unwrap()
                .includes_acl(AclDefault::Include)
        );

        assert_eq!(BucketMask::Default.lifecycle(), Lifecycle::Include);
        assert_eq!(BucketMask::All.lifecycle(), Lifecycle::Include);
        assert_eq!(
            bucket_mask(&["lifecycle"]).unwrap().lifecycle(),
            Lifecycle::Include
        );
        assert_eq!(bucket_mask(&["name"]).unwrap().lifecycle(), Lifecycle::Omit);

        let bucket = pb::Bucket {
            name: "projects/_/buckets/b".into(),
            bucket_id: "b".into(),
            metageneration: 3,
            ..Default::default()
        };
        assert_eq!(BucketMask::Default.apply(bucket.clone()), bucket);
        assert_eq!(BucketMask::All.apply(bucket.clone()), bucket);
        assert_eq!(
            bucket_mask(&["bucket_id", "metageneration"])
                .unwrap()
                .apply(bucket),
            pb::Bucket {
                bucket_id: "b".into(),
                metageneration: 3,
                ..Default::default()
            }
        );
    }
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let error = UnselectableField;
    assert_eq!(error.to_string(), "path is no selectable top-level field");
    assert!(std::error::Error::source(&error).is_none());
}
