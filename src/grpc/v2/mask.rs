//! Read masks as each Storage v2 RPC interprets them. Protobuf gives an absent mask and one
//! without paths the same meaning, the RPC's default fields, and `*` names every field.

use super::{convert::Lifecycle, pb, status};
use prost_types::FieldMask;
use std::collections::BTreeSet;
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

// `macro_rules!` cannot capitalize an identifier, so each field names its variant.
macro_rules! bucket_fields {
    ($($field:ident => $variant:ident),+ $(,)?) => {
        /// A top-level `Bucket` field a read mask can select.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub(super) enum BucketField {
            $($variant),+
        }

        impl std::str::FromStr for BucketField {
            type Err = UnselectableBucketPath;

            fn from_str(path: &str) -> Result<Self, Self::Err> {
                match path {
                    $(stringify!($field) => Ok(Self::$variant),)+
                    _ => Err(UnselectableBucketPath),
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

/// A path that is no top-level `Bucket` field gcs-mimic selects (nested paths included).
#[derive(Debug)]
pub(super) struct UnselectableBucketPath;

impl std::fmt::Display for UnselectableBucketPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("path is no selectable top-level bucket field")
    }
}

impl std::error::Error for UnselectableBucketPath {}

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
                    path.parse().map_err(|UnselectableBucketPath| {
                        status::unsupported(&format!("bucket read_mask {path}"))
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
