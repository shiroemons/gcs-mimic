use super::{convert, pb, status::unsupported};
use crate::{
    acl::{AclEntry, AclEntryInput, AclKind},
    bucket::{self, BucketMetadataUpdate, CorsRule, autoclass::AutoclassUpdate},
    json::JsonError,
    object::MapMode,
};
use std::collections::BTreeMap;
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

fn apply_path(update: &mut BucketMetadataUpdate, b: &pb::Bucket, path: &str) -> Result<(), Status> {
    match path {
        "labels" => {
            update.labels = Some(Some(
                b.labels
                    .iter()
                    .map(|(k, v)| (k.clone(), Some(v.clone())))
                    .collect(),
            ));
            update.labels_mode = MapMode::Replace;
        }
        p if let Some(key) = p.strip_prefix("labels.")
            && !key.is_empty() =>
        {
            update
                .labels
                .get_or_insert_with(|| Some(BTreeMap::new()))
                .get_or_insert_with(BTreeMap::new)
                .insert(key.into(), b.labels.get(key).cloned());
        }
        "storage_class" => {
            update.storage_class = Some(bucket::validate_update_storage_class(&b.storage_class)?)
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
            update.retention_period = Some(
                b.retention_policy
                    .as_ref()
                    .and_then(|policy| policy.retention_duration)
                    .map(|duration| bucket::retention_period(duration.seconds))
                    .transpose()?,
            );
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
            update.soft_delete_retention = Some(bucket::soft_delete_retention(duration.seconds)?);
        }
        "iam_config"
        | "iam_config.uniform_bucket_level_access"
        | "iam_config.uniform_bucket_level_access.enabled"
        | "iam_config.public_access_prevention" => apply_iam(update, b.iam_config.as_ref(), path)?,
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
    bucket::validate_cors(&rules)?;
    Ok(rules)
}

fn apply_iam(
    update: &mut BucketMetadataUpdate,
    config: Option<&pb::bucket::IamConfig>,
    path: &str,
) -> Result<(), Status> {
    if path == "iam_config" {
        if let Some(config) = config {
            if let Some(uniform) = &config.uniform_bucket_level_access {
                update.uniform_bucket_level_access_enabled = Some(uniform.enabled);
            }
            if !config.public_access_prevention.is_empty() {
                update.public_access_prevention_enforced = Some(bucket::public_access_prevention(
                    &config.public_access_prevention,
                )?);
            }
        }
    } else if path == "iam_config.public_access_prevention" {
        let value = config
            .map(|config| config.public_access_prevention.as_str())
            .filter(|value| !value.is_empty())
            .unwrap_or("inherited");
        update.public_access_prevention_enforced = Some(bucket::public_access_prevention(value)?);
    } else {
        update.uniform_bucket_level_access_enabled = Some(
            config
                .and_then(|config| config.uniform_bucket_level_access.as_ref())
                .is_some_and(|uniform| uniform.enabled),
        );
    }
    Ok(())
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
        for seconds in [-1, 1, 604799, 7776001] {
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
