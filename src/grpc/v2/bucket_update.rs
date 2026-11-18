use super::{convert, pb, status::unsupported};
use crate::{
    acl::{AclEntry, AclEntryInput, AclKind},
    bucket::{
        self, BucketMetadataUpdate, CatalogError, CorsRule, RetentionPolicyUpdate,
        autoclass::AutoclassUpdate,
    },
    json::JsonError,
    metadata_error::MetadataError,
    object::MapPatch,
};
use tonic::Status;

/// An UpdateBucket `update_mask` path, parsed once before it is applied. A subpath such as
/// `versioning.enabled` names the same setting as its parent, except the Autoclass and IAM
/// configuration subpaths, which change only part of their message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum UpdatePath {
    /// `labels`: replaces the whole map.
    Labels,
    /// `labels.<key>`: sets one label, or removes it when the request does not carry the key.
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
pub(super) enum AutoclassPath {
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

impl std::str::FromStr for UpdatePath {
    type Err = UnsupportedUpdatePath;

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
                Some(key) if !key.is_empty() => Self::Label(key.into()),
                _ => return Err(UnsupportedUpdatePath(path.into())),
            },
        })
    }
}

/// An `update_mask` path UpdateBucket does not apply; it names the path the client sent.
#[derive(Debug)]
pub(super) struct UnsupportedUpdatePath(String);

impl std::fmt::Display for UnsupportedUpdatePath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "UpdateBucket update_mask {}; use labels, versioning, lifecycle or iam_config",
            self.0
        )
    }
}

impl std::error::Error for UnsupportedUpdatePath {}

impl From<UnsupportedUpdatePath> for Status {
    fn from(error: UnsupportedUpdatePath) -> Self {
        unsupported(&error.to_string())
    }
}

pub(super) fn from_proto(
    bucket: &pb::Bucket,
    paths: &[String],
) -> Result<BucketMetadataUpdate, Status> {
    let mut update = BucketMetadataUpdate {
        touched: !paths.is_empty(),
        ..Default::default()
    };
    // Each path is parsed and applied before the next one is read, so the first failing path in
    // mask order names the error, whether it is refused or its value is invalid.
    for path in paths {
        apply_path(&mut update, bucket, path.parse()?)?;
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
                JsonError::unsupported(&message).into()
            } else {
                Status::invalid_argument(message)
            }
        }
        error @ CatalogError::RetentionPolicyLocked(period) => {
            let message = format!(
                "Bucket retention policy is locked and cannot be removed or shortened; keep retention_policy.retention_duration at least {period} seconds or extend it"
            );
            let mut error = JsonError::from(error);
            error.message = message;
            error.into()
        }
        error @ CatalogError::RetentionPolicyNotSet => {
            let mut error = JsonError::from(error);
            error.message = "No retention policy to lock; set retention_policy.retention_duration first, for example 3600 seconds".into();
            error.into()
        }
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
/// duration, the proto states no rounding for it, so a fractional one is refused rather than
/// silently shortened.
pub(super) fn soft_delete_seconds(duration: &prost_types::Duration) -> Result<i64, Status> {
    if duration.nanos != 0 {
        return Err(unsupported(
            "fractional soft delete duration; use whole seconds",
        ));
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
    .map_err(|error| Status::from(JsonError::from(error)))
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
        // Aliases build the same update; the autoclass subpaths are tested by value elsewhere.
        for (short, long) in [
            ("versioning", "versioning.enabled"),
            ("lifecycle", "lifecycle.rule"),
            ("retention_policy", "retention_policy.retention_duration"),
            (
                "soft_delete_policy",
                "soft_delete_policy.retention_duration",
            ),
            (
                "iam_config.uniform_bucket_level_access",
                "iam_config.uniform_bucket_level_access.enabled",
            ),
        ] {
            assert_eq!(
                format!("{:?}", from_proto(&bucket, &[short.into()]).unwrap()),
                format!("{:?}", from_proto(&bucket, &[long.into()]).unwrap()),
                "{short}"
            );
        }
        let error = from_proto(&bucket, &["labels.".into()]).unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unimplemented);
        assert_eq!(
            error.message(),
            "gcs-mimic does not support gRPC UpdateBucket update_mask labels.; use labels, versioning, lifecycle or iam_config; use the JSON API instead. Set STORAGE_EMULATOR_HOST to the server's api_base_url reported in its ready event."
        );
        for path in [
            "",
            " labels",
            "Labels",
            "labels ",
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
            "name",
            "bucket_id",
            "etag",
            "project",
            "metageneration",
            "location",
            "location_type",
            "create_time",
            "update_time",
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
        // Paths apply in mask order, so the first failing path names the error.
        let invalid_class = pb::Bucket {
            storage_class: "RAPID".into(),
            ..Default::default()
        };
        let first =
            from_proto(&invalid_class, &["storage_class".into(), "name".into()]).unwrap_err();
        assert!(first.message().contains("storage_class"), "{first}");
        assert!(!first.message().contains("update_mask"), "{first}");
        let first =
            from_proto(&invalid_class, &["name".into(), "storage_class".into()]).unwrap_err();
        assert!(first.message().contains("update_mask name"), "{first}");
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
        let error = "labels.".parse::<UpdatePath>().unwrap_err();
        assert_eq!(
            error.to_string(),
            "UpdateBucket update_mask labels.; use labels, versioning, lifecycle or iam_config"
        );
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
            let json = JsonError::from(bucket::validate_update_storage_class(value).unwrap_err());
            assert_eq!(json.message, error.message());
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
                    .map(|retention| retention.seconds()),
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
        assert_eq!(
            from_proto(&config, &["soft_delete_policy".into()])
                .unwrap_err()
                .code(),
            tonic::Code::Unimplemented,
            "a fractional duration must not round to a different retention"
        );
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
