use super::{
    convert,
    mask::{BucketField, UnappliedUpdatePath, update_paths},
    pb,
    status::{unsupported, unsupported_message},
};
use crate::{
    acl::{AclEntry, AclEntryInput, AclKind},
    bucket::{
        self, BucketMetadataUpdate, CatalogError, CorsRule, RetentionPolicyUpdate,
        autoclass::AutoclassUpdate,
    },
    metadata_error::MetadataError,
    object::MapPatch,
    storage::failure::wording,
};
use tonic::Status;

/// An UpdateBucket `update_mask` path, parsed once before it is applied. A subpath such as
/// `versioning.enabled` names the same setting as its parent, except the Autoclass and IAM
/// configuration subpaths, which change only part of their message.
#[derive(Clone, Debug, PartialEq, Eq)]
enum UpdatePath {
    /// `labels`: replaces the whole map.
    Labels,
    /// `labels.<key>`: sets one label, or removes it when the request does not carry the key.
    /// `labels.` names no key and is refused when the mask is parsed ([`EMPTY_LABEL_KEY`]).
    Label(String),
    StorageClass,
    Autoclass(AutoclassPath),
    Versioning,
    DefaultEventBasedHold,
    Cors,
    Lifecycle,
    RetentionPolicy,
    SoftDeletePolicy,
    /// `iam_config`: replaces public access prevention, and uniform bucket-level access when its
    /// message is present.
    IamConfig,
    PublicAccessPrevention,
    UniformBucketLevelAccess,
    Acl,
    DefaultObjectAcl,
}

/// The Autoclass settings an `autoclass` path changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutoclassPath {
    /// `autoclass`: both settings.
    All,
    /// `autoclass.enabled`.
    Enabled,
    /// `autoclass.terminal_storage_class`.
    TerminalStorageClass,
}

impl AutoclassPath {
    fn sets_enabled(self) -> bool {
        match self {
            Self::All | Self::Enabled => true,
            Self::TerminalStorageClass => false,
        }
    }

    fn sets_terminal_storage_class(self) -> bool {
        match self {
            Self::All | Self::TerminalStorageClass => true,
            Self::Enabled => false,
        }
    }
}

/// Cloud Storage's refusal of `labels.` in an update mask, an invalid label with an empty key,
/// whatever the request carries (recorded 2026-11-18, case grpc-probe-inferred-bucket-masks).
const EMPTY_LABEL_KEY: &str =
    "Invalid user label:  with value . Reason: Invalid field \"user_labels_list\"; key is empty.";

impl std::str::FromStr for UpdatePath {
    type Err = UnappliedUpdatePath;

    /// A path no case names is classified by the `Bucket` fields: `name`, `etag`, `project`
    /// and `location`, for one, change nothing ([`BucketField`]'s setting).
    fn from_str(path: &str) -> Result<Self, Self::Err> {
        Ok(match path {
            "labels" => Self::Labels,
            "storage_class" => Self::StorageClass,
            "autoclass" => Self::Autoclass(AutoclassPath::All),
            "autoclass.enabled" => Self::Autoclass(AutoclassPath::Enabled),
            "autoclass.terminal_storage_class" => {
                Self::Autoclass(AutoclassPath::TerminalStorageClass)
            }
            "versioning" | "versioning.enabled" => Self::Versioning,
            "default_event_based_hold" => Self::DefaultEventBasedHold,
            "cors" => Self::Cors,
            "lifecycle" | "lifecycle.rule" => Self::Lifecycle,
            "retention_policy" | "retention_policy.retention_duration" => Self::RetentionPolicy,
            "soft_delete_policy" | "soft_delete_policy.retention_duration" => {
                Self::SoftDeletePolicy
            }
            "iam_config" => Self::IamConfig,
            "iam_config.public_access_prevention" => Self::PublicAccessPrevention,
            "iam_config.uniform_bucket_level_access"
            | "iam_config.uniform_bucket_level_access.enabled" => Self::UniformBucketLevelAccess,
            "acl" => Self::Acl,
            "default_object_acl" => Self::DefaultObjectAcl,
            path => match path.strip_prefix("labels.") {
                Some("") => return Err(UnappliedUpdatePath::Refused(EMPTY_LABEL_KEY)),
                Some(key) => Self::Label(key.into()),
                None if names_no_member(path) => {
                    return Err(UnappliedUpdatePath::Unknown(path.into()));
                }
                None => return Err(UnappliedUpdatePath::of::<BucketField>(path)),
            },
        })
    }
}

/// Whether `path` names a member that a message field of the bucket does not have, as
/// `versioning.no_such_field` does. Cloud Storage refused that path and
/// `iam_config.no_such_field` as naming no field (recorded 2026-11-18, case
/// grpc-probe-inferred-bucket-masks); the other message fields' members were not asked, so
/// their unlisted members stay refused as unsupported.
fn names_no_member(path: &str) -> bool {
    let Some((field, member)) = path.split_once('.') else {
        return false;
    };
    let members: &[&str] = match field {
        "versioning" => &["enabled"],
        "iam_config" => &["uniform_bucket_level_access", "public_access_prevention"],
        _ => return false,
    };
    let member = member.split_once('.').map_or(member, |(member, _)| member);
    !members.contains(&member)
}

pub(super) fn from_proto(
    bucket: &pb::Bucket,
    paths: &[String],
) -> Result<BucketMetadataUpdate, Status> {
    let mut update = BucketMetadataUpdate::default();
    let remedy = "labels, versioning, lifecycle or iam_config";
    for path in update_paths(paths, "UpdateBucket", remedy) {
        let path = path?;
        update.touched = true;
        apply_path(&mut update, bucket, path)?;
    }
    Ok(update)
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other catalog error keeps the shared gRPC mapping"
)]
pub(super) fn input_error(error: CatalogError) -> Status {
    match error {
        // Names no field, so it keeps the shared mapping and its BadRequest.
        error @ CatalogError::Metadata(MetadataError::ClassLocation(_)) => error.into(),
        CatalogError::Metadata(reason) => {
            let message = reason.message(proto_field);
            if reason.unsupported() {
                unsupported_message(wording::unsupported_bucket_setting(&message))
            } else {
                Status::invalid_argument(message)
            }
        }
        // Both retention refusals keep INVALID_ARGUMENT with a message naming the proto field.
        CatalogError::RetentionPolicyLocked(period) => Status::invalid_argument(format!(
            "Bucket retention policy is locked and cannot be removed or shortened; keep retention_policy.retention_duration at least {period} seconds or extend it"
        )),
        CatalogError::RetentionPolicyNotSet => Status::invalid_argument(
            "No retention policy to lock; set retention_policy.retention_duration first, for example 3600 seconds",
        ),
        error => error.into(),
    }
}

pub(super) fn proto_field(field: &str) -> &str {
    match field {
        "contentType" => "content_type",
        "contentEncoding" => "content_encoding",
        "contentDisposition" => "content_disposition",
        "contentLanguage" => "content_language",
        "cacheControl" => "cache_control",
        "customTime" => "custom_time",
        "storageClass" => "storage_class",
        "terminalStorageClass" => "terminal_storage_class",
        "uniformBucketLevelAccess" => "uniform_bucket_level_access",
        "age" => "age_days",
        "createdBefore" => "created_before",
        "customTimeBefore" => "custom_time_before",
        "daysSinceCustomTime" => "days_since_custom_time",
        "daysSinceNoncurrentTime" => "days_since_noncurrent_time",
        "isLive" => "is_live",
        "matchesPrefix" => "matches_prefix",
        "matchesStorageClass" => "matches_storage_class",
        "matchesStorageClass for bucket location" => "matches_storage_class for bucket location",
        "matchesSuffix" => "matches_suffix",
        "noncurrentTimeBefore" => "noncurrent_time_before",
        "numNewerVersions" => "num_newer_versions",
        field => field,
    }
}

fn apply_path(
    update: &mut BucketMetadataUpdate,
    b: &pb::Bucket,
    path: UpdatePath,
) -> Result<(), Status> {
    match path {
        UpdatePath::Labels => {
            update.labels = MapPatch::Replace(
                b.labels
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            );
        }
        UpdatePath::Label(key) => {
            let value = b.labels.get(&key).cloned();
            // A `labels` path replaces the map, which cannot carry the removal of a key missing
            // from it. The answer is the catalog's for an invalid label, given before
            // authorization like the other update-mask errors.
            if value.is_none() && matches!(update.labels, MapPatch::Replace(_)) {
                return Err(input_error(CatalogError::InvalidMetadata(
                    "Invalid bucket label".into(),
                )));
            }
            update.labels.merge_entry(key, value);
        }
        UpdatePath::StorageClass => {
            update.storage_class =
                Some(bucket::validate_update_storage_class(&b.storage_class).map_err(input_error)?)
        }
        UpdatePath::Autoclass(path) => {
            apply_autoclass(update, b.autoclass.as_ref(), path)?;
        }
        UpdatePath::Versioning => {
            update.versioning = Some(bucket::VersioningUpdate::Set(bucket::Versioning::from(
                Some(b.versioning.as_ref().is_some_and(|v| v.enabled)),
            )));
        }
        UpdatePath::DefaultEventBasedHold => {
            update.default_event_based_hold = Some(b.default_event_based_hold)
        }
        UpdatePath::Cors => {
            update.cors = Some(cors_rules(&b.cors)?);
        }
        UpdatePath::Lifecycle => {
            update.lifecycle = Some(convert::lifecycle_rules(b.lifecycle.as_ref())?)
        }
        UpdatePath::RetentionPolicy => {
            if b.retention_policy
                .as_ref()
                .is_some_and(|policy| policy.is_locked)
            {
                return Err(unsupported(
                    "retention_policy.is_locked; use LockBucketRetentionPolicy",
                ));
            }
            let period = b
                .retention_policy
                .as_ref()
                .and_then(|policy| policy.retention_duration.as_ref())
                .map(|duration| {
                    bucket::RetentionPeriod::try_from(retention_seconds(duration))
                        .map_err(input_error)
                })
                .transpose()?;
            update.retention_policy = match period {
                Some(period) => RetentionPolicyUpdate::Set(period),
                None => RetentionPolicyUpdate::Remove,
            };
        }
        UpdatePath::SoftDeletePolicy => {
            let duration = b
                .soft_delete_policy
                .as_ref()
                .and_then(|policy| policy.retention_duration)
                .unwrap_or_default();
            update.soft_delete_retention = Some(
                bucket::SoftDeleteRetention::try_from(soft_delete_seconds(&duration)?)
                    .map_err(input_error)?,
            );
        }
        // Under the parent path Cloud Storage treats the two settings differently (recorded
        // 2026-11-13): public access prevention is replaced, so an `iam_config` that leaves it
        // out (empty, or uniform access alone, as the Go client sends) resets it to `inherited`,
        // while uniform access changes only when its message is present.
        UpdatePath::IamConfig => {
            apply_public_access_prevention(update, b.iam_config.as_ref())?;
            if b.iam_config
                .as_ref()
                .is_some_and(|config| config.uniform_bucket_level_access.is_some())
            {
                apply_uniform_access(update, b.iam_config.as_ref());
            }
        }
        UpdatePath::PublicAccessPrevention => {
            apply_public_access_prevention(update, b.iam_config.as_ref())?;
        }
        UpdatePath::UniformBucketLevelAccess => {
            apply_uniform_access(update, b.iam_config.as_ref());
        }
        UpdatePath::Acl => {
            update.acl = Some(acl_entries(
                AclKind::Bucket,
                b.acl.iter().map(|entry| (&entry.entity, &entry.role)),
            )?)
        }
        UpdatePath::DefaultObjectAcl => {
            update.default_object_acl = Some(acl_entries(
                AclKind::DefaultObject,
                b.default_object_acl
                    .iter()
                    .map(|entry| (&entry.entity, &entry.role)),
            )?)
        }
    }
    Ok(())
}

/// The whole seconds of a retention duration: the proto rounds any `nanos` down to the nearest
/// second (storage.proto lines 2429-2433). Normalizing first carries nanos that reach a whole
/// second into the seconds and gives a positive duration nonnegative nanos, so its seconds are
/// the value rounded down even when a client sent nanos of the other sign. A duration that
/// normalizes to zero or below is no valid period, however its seconds round.
pub(super) fn retention_seconds(duration: &prost_types::Duration) -> i64 {
    duration.normalized().seconds
}

/// The whole seconds of a soft delete duration, shared with CreateBucket. Unlike a retention
/// duration, a fractional one is refused: Cloud Storage answers UpdateBucket with this status
/// and a `BadRequest` that repeats it (recorded 2026-11-18).
pub(super) fn soft_delete_seconds(duration: &prost_types::Duration) -> Result<i64, Status> {
    if duration.nanos != 0 {
        return Err(super::status::invalid_field("Invalid argument."));
    }
    Ok(duration.seconds)
}

fn apply_autoclass(
    update: &mut BucketMetadataUpdate,
    config: Option<&pb::bucket::Autoclass>,
    path: AutoclassPath,
) -> Result<(), Status> {
    let next = update
        .autoclass
        .get_or_insert_with(AutoclassUpdate::default);
    if path.sets_enabled() {
        next.enabled = Some(config.is_some_and(|config| config.enabled));
    }
    if path.sets_terminal_storage_class()
        && let Some(class) = config.and_then(|config| config.terminal_storage_class.as_ref())
    {
        next.terminal_storage_class = Some(class.parse().map_err(|_| {
            Status::invalid_argument("autoclass.terminal_storage_class must be NEARLINE or ARCHIVE")
        })?);
    }
    Ok(())
}

pub(super) fn cors_rules(cors: &[pb::bucket::Cors]) -> Result<Vec<CorsRule>, Status> {
    let rules = cors.iter().map(CorsRule::from).collect::<Vec<_>>();
    bucket::validate_cors(&rules).map_err(input_error)?;
    Ok(rules)
}

/// Sets public access prevention from `config`; an absent or empty value means `inherited`.
fn apply_public_access_prevention(
    update: &mut BucketMetadataUpdate,
    config: Option<&pb::bucket::IamConfig>,
) -> Result<(), Status> {
    let value = config
        .map(|config| config.public_access_prevention.as_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("inherited");
    update.public_access_prevention = Some(
        value
            .parse::<bucket::PublicAccessPrevention>()
            .map_err(input_error)?,
    );
    Ok(())
}

/// Sets uniform bucket-level access from `config`; an absent message means disabled.
fn apply_uniform_access(update: &mut BucketMetadataUpdate, config: Option<&pb::bucket::IamConfig>) {
    update.uniform_bucket_level_access_enabled = Some(
        config
            .and_then(|config| config.uniform_bucket_level_access.as_ref())
            .is_some_and(|uniform| uniform.enabled)
            .into(),
    );
}

pub(super) fn acl_entries<'a>(
    kind: AclKind,
    entries: impl Iterator<Item = (&'a String, &'a String)>,
) -> Result<Vec<AclEntry>, Status> {
    crate::acl::parse_entries(
        kind,
        entries
            .map(|(entity, role)| AclEntryInput {
                entity: entity.clone(),
                role: role.clone(),
            })
            .collect(),
    )
    .map_err(|error| Status::invalid_argument(wording::acl_refusal(&error)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `labels` path replaces the map, so a later `labels.KEY` path can add or change a key it
    /// carries but cannot remove one; an earlier one is overwritten by the replacement.
    #[test]
    fn label_masks_combine_a_replacement_with_key_paths_by_their_order() {
        let bucket = pb::Bucket {
            labels: [("present".into(), "value".into())].into(),
            ..Default::default()
        };
        let replaced = MapPatch::Replace([("present".into(), "value".into())].into());
        let paths = |paths: [&str; 2]| paths.map(String::from);
        let error = from_proto(&bucket, &paths(["labels", "labels.missing"])).unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(error.message(), "Invalid bucket label");
        for order in [["labels.missing", "labels"], ["labels", "labels.present"]] {
            let update = from_proto(&bucket, &paths(order)).unwrap();
            assert_eq!(update.labels, replaced, "{order:?}");
        }
    }

    /// The names of the settings an update changes, read by destructuring so that a new
    /// setting must be listed here.
    fn changed(update: &BucketMetadataUpdate) -> Vec<&'static str> {
        let BucketMetadataUpdate {
            acl,
            default_object_acl,
            cors,
            lifecycle,
            retention_policy,
            soft_delete_retention,
            object_retention_enabled,
            labels,
            storage_class,
            autoclass,
            versioning,
            default_event_based_hold,
            public_access_prevention,
            uniform_bucket_level_access_enabled,
            rpo,
            touched: _,
        } = update;
        [
            ("acl", acl.is_some()),
            ("default_object_acl", default_object_acl.is_some()),
            ("cors", cors.is_some()),
            ("lifecycle", lifecycle.is_some()),
            (
                "retention_policy",
                *retention_policy != RetentionPolicyUpdate::default(),
            ),
            ("soft_delete_retention", soft_delete_retention.is_some()),
            (
                "object_retention_enabled",
                object_retention_enabled.is_some(),
            ),
            ("labels", *labels != MapPatch::default()),
            ("storage_class", storage_class.is_some()),
            ("autoclass", autoclass.is_some()),
            ("versioning", versioning.is_some()),
            (
                "default_event_based_hold",
                default_event_based_hold.is_some(),
            ),
            (
                "public_access_prevention",
                public_access_prevention.is_some(),
            ),
            (
                "uniform_bucket_level_access_enabled",
                uniform_bucket_level_access_enabled.is_some(),
            ),
            ("rpo", rpo.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect()
    }

    /// A bucket every accepted path can apply.
    fn configured_bucket() -> pb::Bucket {
        pb::Bucket {
            labels: [("team".into(), "dev".into())].into(),
            storage_class: "NEARLINE".into(),
            autoclass: Some(pb::bucket::Autoclass {
                enabled: true,
                terminal_storage_class: Some("ARCHIVE".into()),
                ..Default::default()
            }),
            versioning: Some(pb::bucket::Versioning { enabled: true }),
            default_event_based_hold: true,
            retention_policy: Some(pb::bucket::RetentionPolicy {
                retention_duration: Some(prost_types::Duration {
                    seconds: 3600,
                    nanos: 0,
                }),
                ..Default::default()
            }),
            soft_delete_policy: Some(pb::bucket::SoftDeletePolicy {
                retention_duration: Some(prost_types::Duration {
                    seconds: 604_800,
                    nanos: 0,
                }),
                ..Default::default()
            }),
            iam_config: Some(pb::bucket::IamConfig {
                public_access_prevention: "enforced".into(),
                uniform_bucket_level_access: Some(
                    pb::bucket::iam_config::UniformBucketLevelAccess {
                        enabled: true,
                        ..Default::default()
                    },
                ),
            }),
            acl: vec![pb::BucketAccessControl {
                entity: "allAuthenticatedUsers".into(),
                role: "READER".into(),
                ..Default::default()
            }],
            default_object_acl: vec![pb::ObjectAccessControl {
                entity: "allAuthenticatedUsers".into(),
                role: "READER".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    /// Each accepted update_mask path, the settings it changes, and the paths that are its
    /// aliases; every other path is refused with UNIMPLEMENTED naming it, in mask order.
    #[test]
    fn update_mask_paths_select_their_settings_and_refuse_the_rest() {
        let bucket = configured_bucket();
        let accepted: [(&[&str], &[&str]); 15] = [
            (&["labels", "labels.team", "labels.absent"], &["labels"]),
            (&["storage_class"], &["storage_class"]),
            (
                &[
                    "autoclass",
                    "autoclass.enabled",
                    "autoclass.terminal_storage_class",
                ],
                &["autoclass"],
            ),
            (&["versioning", "versioning.enabled"], &["versioning"]),
            (&["default_event_based_hold"], &["default_event_based_hold"]),
            (&["cors"], &["cors"]),
            (&["lifecycle", "lifecycle.rule"], &["lifecycle"]),
            (
                &["retention_policy", "retention_policy.retention_duration"],
                &["retention_policy"],
            ),
            (
                &[
                    "soft_delete_policy",
                    "soft_delete_policy.retention_duration",
                ],
                &["soft_delete_retention"],
            ),
            (
                &["iam_config"],
                &[
                    "public_access_prevention",
                    "uniform_bucket_level_access_enabled",
                ],
            ),
            (
                &["iam_config.public_access_prevention"],
                &["public_access_prevention"],
            ),
            (
                &[
                    "iam_config.uniform_bucket_level_access",
                    "iam_config.uniform_bucket_level_access.enabled",
                ],
                &["uniform_bucket_level_access_enabled"],
            ),
            (&["acl"], &["acl"]),
            (&["default_object_acl"], &["default_object_acl"]),
            // A key path may name any label key, dots included.
            (&["labels.a.b"], &["labels"]),
        ];
        for (paths, settings) in accepted {
            for path in paths {
                let update = from_proto(&bucket, &[(*path).into()]).unwrap();
                assert_eq!(changed(&update), settings, "{path}");
                assert!(update.touched, "{path}");
            }
        }
        // Aliases set their one setting, checked above, to the same value; the autoclass
        // subpaths are tested by value elsewhere.
        let update = |path: &str| from_proto(&bucket, &[path.into()]).unwrap();
        assert_eq!(
            update("versioning").versioning,
            update("versioning.enabled").versioning
        );
        assert_eq!(
            update("lifecycle").lifecycle,
            update("lifecycle.rule").lifecycle
        );
        assert_eq!(
            update("retention_policy").retention_policy,
            update("retention_policy.retention_duration").retention_policy
        );
        assert_eq!(
            update("soft_delete_policy").soft_delete_retention,
            update("soft_delete_policy.retention_duration").soft_delete_retention
        );
        assert_eq!(
            update("iam_config.uniform_bucket_level_access").uniform_bucket_level_access_enabled,
            update("iam_config.uniform_bucket_level_access.enabled")
                .uniform_bucket_level_access_enabled
        );
        // A key path takes the rest of the path as one key, dots included.
        let dotted = pb::Bucket {
            labels: [("a.b".into(), "c".into())].into(),
            ..Default::default()
        };
        assert_eq!(
            from_proto(&dotted, &["labels.a.b".into()]).unwrap().labels,
            MapPatch::Merge([("a.b".into(), Some("c".into()))].into())
        );
        // Recorded 2026-11-18 (case grpc-probe-inferred-bucket-masks): an empty label key is an
        // invalid label, answered with a BadRequest whose violation repeats the message, and the
        // first failing path in mask order names the error.
        crate::grpc::test_support::assert_bad_request(
            from_proto(&bucket, &["labels.".into()]),
            "Invalid user label:  with value . Reason: Invalid field \"user_labels_list\"; key is empty.",
        );
        crate::grpc::test_support::assert_bad_request(
            from_proto(&bucket, &["labels.team".into(), "labels.".into()]),
            EMPTY_LABEL_KEY,
        );
        let first = from_proto(&bucket, &["no_such_field".into(), "labels.".into()]).unwrap_err();
        assert_eq!(
            first.message(),
            "There is no field for this resource named 'no_such_field'."
        );
        for path in [
            "storage_class.name",
            "autoclass.toggle_time",
            "autoclass.terminal_storage_class_update_time",
            "versioning.enabled.value",
            "lifecycle.rule.action",
            "retention_policy.is_locked",
            "retention_policy.effective_time",
            "soft_delete_policy.effective_time",
            "iam_config.uniform_bucket_level_access.lock_time",
            "acl.role",
            "default_object_acl.role",
            "object_retention",
            "object_retention.enabled",
        ] {
            let error = from_proto(&bucket, &[path.into()]).unwrap_err();
            assert_eq!(error.code(), tonic::Code::Unimplemented, "{path}");
            assert_eq!(
                error.message(),
                unsupported(&format!(
                    "UpdateBucket update_mask {path}; use labels, versioning, lifecycle or iam_config"
                ))
                .message(),
                "{path}"
            );
            assert!(!error.details().is_empty(), "{path}");
        }
        // A path whose first segment is no bucket field names nothing, exact spelling included.
        for path in ["", " labels", "Labels", "labels ", "no_such_field"] {
            let error = from_proto(&bucket, &[path.into()]).unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument, "{path}");
            assert_eq!(
                error.message(),
                format!("There is no field for this resource named '{path}'."),
            );
        }
        // The fields the service alone sets are accepted and change nothing (recorded
        // 2026-11-18 for create_time and update_time, 2026-11-18 for the rest), and so are the
        // identifier, the etag and the settings fixed at creation (recorded 2026-11-18, case
        // grpc-probe-inferred-bucket-masks), which leave the update untouched.
        for path in [
            "bucket_id",
            "metageneration",
            "location_type",
            "create_time",
            "update_time",
            "owner",
            "name",
            "etag",
            "project",
            "location",
        ] {
            let update = from_proto(&bucket, &[path.into()]).unwrap();
            assert!(update.is_empty(), "{path}");
            assert!(!update.touched, "{path}");
        }
        // A message field's member it does not have names no field, as a top-level path does
        // (recorded 2026-11-18, case grpc-probe-inferred-bucket-masks).
        for path in ["versioning.no_such_field", "iam_config.no_such_field"] {
            let error = from_proto(&bucket, &[path.into()]).unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument, "{path}");
            assert_eq!(
                error.message(),
                format!("There is no field for this resource named '{path}'."),
            );
            assert!(error.details().is_empty(), "{path}");
        }
        // Paths apply in mask order, so the first failing path names the error.
        let invalid_class = pb::Bucket {
            storage_class: "RAPID".into(),
            ..Default::default()
        };
        let first =
            from_proto(&invalid_class, &["storage_class".into(), "name".into()]).unwrap_err();
        assert!(first.message().contains("storage_class"), "{first}");
        assert!(!first.message().contains("update_mask"), "{first}");
        let first = from_proto(
            &invalid_class,
            &["object_retention".into(), "storage_class".into()],
        )
        .unwrap_err();
        assert!(
            first.message().contains("update_mask object_retention"),
            "{first}"
        );
    }

    #[test]
    fn update_paths_parse_once_into_the_setting_they_name() {
        for (path, parsed) in [
            ("labels", UpdatePath::Labels),
            ("labels.team", UpdatePath::Label("team".into())),
            ("labels.a.b", UpdatePath::Label("a.b".into())),
            ("versioning.enabled", UpdatePath::Versioning),
            (
                "autoclass.terminal_storage_class",
                UpdatePath::Autoclass(AutoclassPath::TerminalStorageClass),
            ),
            (
                "iam_config.uniform_bucket_level_access.enabled",
                UpdatePath::UniformBucketLevelAccess,
            ),
        ] {
            assert_eq!(path.parse::<UpdatePath>().unwrap(), parsed, "{path}");
        }
        for (path, unapplied) in [
            (
                "website",
                UnappliedUpdatePath::Unsupported("website".into()),
            ),
            ("*", UnappliedUpdatePath::Unsupported("*".into())),
            (
                "versioning.no_such_field",
                UnappliedUpdatePath::Unknown("versioning.no_such_field".into()),
            ),
            (
                "iam_config.no_such_field",
                UnappliedUpdatePath::Unknown("iam_config.no_such_field".into()),
            ),
            (
                "iam_config.uniform_bucket_level_access.lock_time",
                UnappliedUpdatePath::Unsupported(
                    "iam_config.uniform_bucket_level_access.lock_time".into(),
                ),
            ),
            ("labels.", UnappliedUpdatePath::Refused(EMPTY_LABEL_KEY)),
            // Output-only fields, the identifier, the etag and the settings fixed at creation.
            ("create_time", UnappliedUpdatePath::Ignored),
            ("owner", UnappliedUpdatePath::Ignored),
            ("name", UnappliedUpdatePath::Ignored),
            ("etag", UnappliedUpdatePath::Ignored),
            ("project", UnappliedUpdatePath::Ignored),
            ("location", UnappliedUpdatePath::Ignored),
            // An output-only field named with a member path is refused like any other path
            // under a field, and so is a field no mask selects.
            (
                "owner.entity",
                UnappliedUpdatePath::Unsupported("owner.entity".into()),
            ),
            (
                "satisfies_pzs",
                UnappliedUpdatePath::Unsupported("satisfies_pzs".into()),
            ),
            (
                "satisfies_pzs.x",
                UnappliedUpdatePath::Unsupported("satisfies_pzs.x".into()),
            ),
            (
                "no_such_field",
                UnappliedUpdatePath::Unknown("no_such_field".into()),
            ),
            (
                "no_such_field.enabled",
                UnappliedUpdatePath::Unknown("no_such_field.enabled".into()),
            ),
        ] {
            assert_eq!(path.parse::<UpdatePath>().unwrap_err(), unapplied, "{path}");
        }
    }

    /// `names_no_member` lists the members of `versioning` and `iam_config` by hand, since prost
    /// has no reflection. Destructuring each message without `..` makes a member the proto adds
    /// a compile error here until it is listed below and in the table.
    #[test]
    fn names_no_member_lists_every_member_the_proto_declares() {
        let pb::bucket::Versioning { enabled: _ } = pb::bucket::Versioning::default();
        let pb::bucket::IamConfig {
            uniform_bucket_level_access: _,
            public_access_prevention: _,
        } = pb::bucket::IamConfig::default();
        for member in [
            "versioning.enabled",
            "iam_config.uniform_bucket_level_access",
            "iam_config.public_access_prevention",
        ] {
            assert!(!names_no_member(member), "{member}");
            assert!(!names_no_member(&format!("{member}.x")), "{member}");
        }
        for path in ["versioning.no_such_field", "iam_config.enabled"] {
            assert!(names_no_member(path), "{path}");
        }
        // Only these two messages are listed; another field's members are not judged here.
        assert!(!names_no_member("autoclass.no_such_field"));
    }

    /// Cloud Storage accepts an output-only path and changes nothing, and refuses a path that
    /// names no bucket field (recorded 2026-11-18).
    #[test]
    fn output_only_paths_change_nothing_and_unknown_paths_are_invalid() {
        let bucket = pb::Bucket::default();
        let update = from_proto(&bucket, &["create_time".into(), "update_time".into()]).unwrap();
        assert!(update.is_empty());
        let update = from_proto(&bucket, &["create_time".into(), "labels".into()]).unwrap();
        assert!(update.touched);
        let unknown = from_proto(&bucket, &["no_such_field".into()]).unwrap_err();
        assert_eq!(unknown.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            unknown.message(),
            "There is no field for this resource named 'no_such_field'."
        );
        assert!(unknown.details().is_empty());
    }

    #[test]
    fn invalid_storage_classes_keep_client_values_verbatim() {
        for value in [
            "storageClass",
            "isLive",
            "storageClass.isLive",
            "terminalStorageClass",
        ] {
            let bucket = pb::Bucket {
                storage_class: value.into(),
                ..Default::default()
            };
            let error = from_proto(&bucket, &["storage_class".into()]).unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            assert_eq!(
                error.message(),
                format!("Invalid storage class \"{value}\"")
            );
            let json =
                crate::ApiError::from(bucket::validate_update_storage_class(value).unwrap_err());
            assert_eq!(json.message(), error.message());
        }
    }

    #[test]
    fn bucket_update_errors_name_proto_fields() {
        let bucket = pb::Bucket {
            storage_class: "RAPID".into(),
            ..Default::default()
        };
        let error = from_proto(&bucket, &["storage_class".into()]).unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unimplemented);
        assert!(error.message().contains("storage_class"), "{error}");
        assert!(!error.message().contains("storageClass"));
        assert!(!error.details().is_empty());
        for input in [
            CatalogError::RetentionPolicyLocked(60),
            CatalogError::RetentionPolicyNotSet,
            CatalogError::Metadata(crate::metadata_error::MetadataError::AutoclassDisabled),
        ] {
            let error = input_error(input);
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            assert!(!error.message().contains("retentionPolicy"));
            assert!(!error.message().contains("retentionPeriod"));
            assert!(!error.message().contains("terminalStorageClass"));
            assert!(!error.message().contains('{'));
        }
        assert_eq!(
            input_error(CatalogError::NotEmpty).code(),
            tonic::Code::FailedPrecondition
        );
    }

    #[test]
    fn bucket_updates_validate_proto_fields_without_json_bodies() {
        let config = pb::Bucket {
            autoclass: Some(pb::bucket::Autoclass {
                enabled: true,
                terminal_storage_class: Some("COLDLINE".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let error = from_proto(&config, &["autoclass".into()]).unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(error.message().contains("terminal_storage_class"));
        assert!(!error.message().contains("terminalStorageClass"));
        for seconds in [-1, 0, 3_155_760_000] {
            let config = pb::Bucket {
                retention_policy: Some(pb::bucket::RetentionPolicy {
                    retention_duration: Some(prost_types::Duration { seconds, nanos: 0 }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let error = from_proto(&config, &["retention_policy".into()]).unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            assert!(!error.message().contains("retentionPeriod"));
        }
        for seconds in [-1, 7_776_001, 3_155_760_000, i64::MAX] {
            let config = pb::Bucket {
                soft_delete_policy: Some(pb::bucket::SoftDeletePolicy {
                    retention_duration: Some(prost_types::Duration { seconds, nanos: 0 }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            assert_eq!(
                from_proto(&config, &["soft_delete_policy".into()])
                    .unwrap_err()
                    .code(),
                tonic::Code::InvalidArgument
            );
        }
        for seconds in [0, 1, 5, 604_799, 7_776_000] {
            let config = pb::Bucket {
                soft_delete_policy: Some(pb::bucket::SoftDeletePolicy {
                    retention_duration: Some(prost_types::Duration { seconds, nanos: 0 }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let update = from_proto(&config, &["soft_delete_policy".into()]).unwrap();
            assert_eq!(
                update
                    .soft_delete_retention
                    .map(crate::bucket::SoftDeleteRetention::seconds),
                u64::try_from(seconds).ok()
            );
        }
        let config = pb::Bucket {
            soft_delete_policy: Some(pb::bucket::SoftDeletePolicy {
                retention_duration: Some(prost_types::Duration {
                    seconds: 5,
                    nanos: 500_000_000,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        // Cloud Storage's answer, with a BadRequest that repeats it (recorded 2026-11-18).
        let fractional = from_proto(&config, &["soft_delete_policy".into()]).unwrap_err();
        assert_eq!(
            fractional.code(),
            tonic::Code::InvalidArgument,
            "a fractional duration must not round to a different retention"
        );
        assert_eq!(fractional.message(), "Invalid argument.");
        assert!(!fractional.details().is_empty());
        let config = pb::Bucket {
            cors: vec![pb::bucket::Cors {
                origin: vec!["invalid origin".into()],
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            from_proto(&config, &["cors".into()]).unwrap_err().code(),
            tonic::Code::InvalidArgument
        );
    }
}
