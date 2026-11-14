use super::{convert, pb, status::unsupported};
use crate::{
    acl::{AclEntry, AclEntryInput, AclKind},
    bucket::{
        self, BucketMetadataUpdate, CatalogError, CorsRule, RetentionPolicyUpdate,
        autoclass::AutoclassUpdate,
    },
    json::JsonError,
    object::MapPatch,
};
use tonic::Status;

pub(super) fn from_proto(
    bucket: &pb::Bucket,
    paths: &[String],
) -> Result<BucketMetadataUpdate, Status> {
    let mut update = BucketMetadataUpdate {
        touched: !paths.is_empty(),
        ..Default::default()
    };
    for path in paths {
        apply_path(&mut update, bucket, path)?;
    }
    Ok(update)
}

pub(super) fn input_error(error: CatalogError) -> Status {
    match &error {
        CatalogError::Metadata(reason) => {
            let message = reason.message(proto_field);
            return if reason.unsupported() {
                JsonError::unsupported(&message).into()
            } else {
                Status::invalid_argument(message)
            };
        }
        CatalogError::RetentionPolicyLocked(period) => {
            let message = format!(
                "Bucket retention policy is locked and cannot be removed or shortened; keep retention_policy.retention_duration at least {period} seconds or extend it"
            );
            let mut error = JsonError::from(error);
            error.message = message;
            return error.into();
        }
        CatalogError::RetentionPolicyNotSet => {
            let mut error = JsonError::from(error);
            error.message = "No retention policy to lock; set retention_policy.retention_duration first, for example 3600 seconds".into();
            return error.into();
        }
        _ => {}
    }
    error.into()
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

fn apply_path(update: &mut BucketMetadataUpdate, b: &pb::Bucket, path: &str) -> Result<(), Status> {
    match path {
        "labels" => {
            update.labels = MapPatch::Replace(
                b.labels
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            );
        }
        p if let Some(key) = p.strip_prefix("labels.")
            && !key.is_empty() =>
        {
            let value = b.labels.get(key).cloned();
            // A `labels` path replaces the map, which cannot carry the removal of a key missing
            // from it. The answer is the catalog's for an invalid label, given before
            // authorization like the other update-mask errors.
            if value.is_none() && matches!(update.labels, MapPatch::Replace(_)) {
                return Err(input_error(CatalogError::InvalidMetadata(
                    "Invalid bucket label".into(),
                )));
            }
            update.labels.merge_entry(key.into(), value);
        }
        "storage_class" => {
            update.storage_class =
                Some(bucket::validate_update_storage_class(&b.storage_class).map_err(input_error)?)
        }
        "autoclass" | "autoclass.enabled" | "autoclass.terminal_storage_class" => {
            apply_autoclass(update, b.autoclass.as_ref(), path)?;
        }
        "versioning" | "versioning.enabled" => {
            update.versioning_enabled = Some(b.versioning.as_ref().is_some_and(|v| v.enabled));
            update.versioning_configured = Some(true);
        }
        "default_event_based_hold" => {
            update.default_event_based_hold = Some(b.default_event_based_hold)
        }
        "cors" => {
            update.cors = Some(cors_rules(&b.cors)?);
        }
        "lifecycle" | "lifecycle.rule" => {
            update.lifecycle = Some(convert::lifecycle_rules(b.lifecycle.as_ref())?)
        }
        "retention_policy" | "retention_policy.retention_duration" => {
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
                .and_then(|policy| policy.retention_duration)
                .map(|duration| bucket::retention_period(duration.seconds).map_err(input_error))
                .transpose()?;
            update.retention_policy = match period {
                Some(period) => RetentionPolicyUpdate::Set(period),
                None => RetentionPolicyUpdate::Remove,
            };
        }
        "soft_delete_policy" | "soft_delete_policy.retention_duration" => {
            let duration = b
                .soft_delete_policy
                .as_ref()
                .and_then(|policy| policy.retention_duration)
                .unwrap_or_default();
            if duration.nanos != 0 {
                return Err(unsupported(
                    "fractional soft delete duration; use whole seconds",
                ));
            }
            update.soft_delete_retention =
                Some(bucket::SoftDeleteRetention::try_from(duration.seconds).map_err(input_error)?);
        }
        // Under the parent path Cloud Storage treats the two settings differently (recorded
        // 2026-11-13): public access prevention is replaced, so an `iam_config` that leaves it
        // out (empty, or uniform access alone, as the Go client sends) resets it to `inherited`,
        // while uniform access changes only when its message is present.
        "iam_config" => {
            apply_public_access_prevention(update, b.iam_config.as_ref())?;
            if b.iam_config
                .as_ref()
                .is_some_and(|config| config.uniform_bucket_level_access.is_some())
            {
                apply_uniform_access(update, b.iam_config.as_ref());
            }
        }
        "iam_config.public_access_prevention" => {
            apply_public_access_prevention(update, b.iam_config.as_ref())?;
        }
        "iam_config.uniform_bucket_level_access"
        | "iam_config.uniform_bucket_level_access.enabled" => {
            apply_uniform_access(update, b.iam_config.as_ref());
        }
        "acl" => {
            update.acl = Some(acl_entries(
                AclKind::Bucket,
                b.acl.iter().map(|entry| (&entry.entity, &entry.role)),
            )?)
        }
        "default_object_acl" => {
            update.default_object_acl = Some(acl_entries(
                AclKind::DefaultObject,
                b.default_object_acl
                    .iter()
                    .map(|entry| (&entry.entity, &entry.role)),
            )?)
        }
        _ => {
            return Err(unsupported(&format!(
                "UpdateBucket update_mask {path}; use labels, versioning, lifecycle or iam_config"
            )));
        }
    }
    Ok(())
}

fn apply_autoclass(
    update: &mut BucketMetadataUpdate,
    config: Option<&pb::bucket::Autoclass>,
    path: &str,
) -> Result<(), Status> {
    let next = update
        .autoclass
        .get_or_insert_with(AutoclassUpdate::default);
    if path != "autoclass.terminal_storage_class" {
        next.enabled = Some(config.is_some_and(|config| config.enabled));
    }
    if path != "autoclass.enabled"
        && let Some(class) = config.and_then(|config| config.terminal_storage_class.as_ref())
    {
        bucket::autoclass::validate_terminal_storage_class(class).map_err(|_| {
            Status::invalid_argument("autoclass.terminal_storage_class must be NEARLINE or ARCHIVE")
        })?;
        next.terminal_storage_class = Some(class.clone());
    }
    Ok(())
}

pub(super) fn cors_rules(cors: &[pb::bucket::Cors]) -> Result<Vec<CorsRule>, Status> {
    let rules = cors
        .iter()
        .map(|rule| CorsRule {
            origin: rule.origin.clone(),
            method: rule.method.clone(),
            response_header: rule.response_header.clone(),
            max_age_seconds: Some(rule.max_age_seconds),
        })
        .collect::<Vec<_>>();
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
    update.public_access_prevention_enforced =
        Some(bucket::public_access_prevention(value).map_err(input_error)?);
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
