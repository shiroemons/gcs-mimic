use super::pb;
use crate::{
    acl::{AclEntry, AclKind},
    bucket::{BucketRecord, CatalogError},
    json::acl::AclOwner,
    object::{ObjectError, ObjectRecord, retention::RetentionMode, unix_nanos::UnixNanos},
};
use prost_types::{Duration, Timestamp};
use serde_json::Value;

/// A retention time as a protobuf timestamp, straight from its seconds and nanoseconds so no
/// platform clock resolution is involved.
fn unix_nanos_timestamp(time: UnixNanos) -> Timestamp {
    Timestamp {
        seconds: time.secs(),
        nanos: i32::try_from(time.subsec_nanos()).unwrap_or_default(),
    }
}

/// Converts stored JSON lifecycle rules, which the shared validator has already accepted.
fn lifecycle(rules: &[Value]) -> Option<pb::bucket::Lifecycle> {
    use pb::bucket::lifecycle::{
        Rule,
        rule::{Action, Condition},
    };
    if rules.is_empty() {
        return None;
    }
    let date = |v: &Value| -> Option<crate::grpc::generated::google::r#type::Date> {
        let mut parts = v.as_str()?.split('-');
        Some(crate::grpc::generated::google::r#type::Date {
            year: parts.next()?.parse().ok()?,
            month: parts.next()?.parse().ok()?,
            day: parts.next()?.parse().ok()?,
        })
    };
    let strings = |v: &Value| {
        v.as_array()
            .map(|v| {
                v.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let days = |v: &Value| v.as_i64().and_then(|v| i32::try_from(v).ok());
    Some(pb::bucket::Lifecycle {
        rule: rules
            .iter()
            .map(|r| {
                let c = &r["condition"];
                Rule {
                    action: Some(Action {
                        r#type: r["action"]["type"].as_str().unwrap_or_default().into(),
                        storage_class: r["action"]["storageClass"]
                            .as_str()
                            .unwrap_or_default()
                            .into(),
                    }),
                    condition: Some(Condition {
                        age_days: days(&c["age"]),
                        created_before: date(&c["createdBefore"]),
                        is_live: c["isLive"].as_bool(),
                        num_newer_versions: days(&c["numNewerVersions"]),
                        matches_storage_class: strings(&c["matchesStorageClass"]),
                        days_since_custom_time: days(&c["daysSinceCustomTime"]),
                        custom_time_before: date(&c["customTimeBefore"]),
                        days_since_noncurrent_time: days(&c["daysSinceNoncurrentTime"]),
                        noncurrent_time_before: date(&c["noncurrentTimeBefore"]),
                        matches_prefix: strings(&c["matchesPrefix"]),
                        matches_suffix: strings(&c["matchesSuffix"]),
                    }),
                }
            })
            .collect(),
    })
}

macro_rules! access_control {
    ($name:ident, $type:path) => {
        /// Converts an entry rendered by the JSON ACL code, so both transports derive the same
        /// ids, etags and entity fields.
        pub(crate) fn $name(value: &Value) -> $type {
            let text = |field: &str| value[field].as_str().unwrap_or_default().to_owned();
            $type {
                role: text("role"),
                id: text("id"),
                entity: text("entity"),
                entity_alt: String::new(),
                entity_id: text("entityId"),
                etag: text("etag"),
                email: text("email"),
                domain: text("domain"),
                project_team: value.get("projectTeam").map(|team| pb::ProjectTeam {
                    project_number: team["projectNumber"].as_str().unwrap_or_default().into(),
                    team: team["team"].as_str().unwrap_or_default().into(),
                }),
            }
        }
    };
}

access_control!(bucket_access_control, pb::BucketAccessControl);
access_control!(object_access_control, pb::ObjectAccessControl);

pub(crate) fn bucket(record: &BucketRecord) -> Result<pb::Bucket, tonic::Status> {
    let retention_policy = record
        .retention_policy
        .as_ref()
        .map(|policy| {
            Ok::<_, CatalogError>(pb::bucket::RetentionPolicy {
                effective_time: Some(policy.effective_time.into()),
                is_locked: policy.is_locked,
                retention_duration: Some(Duration {
                    seconds: crate::grpc::convert::signed(
                        policy.retention_period,
                        "retention_period",
                    )
                    .map_err(crate::storage::failure::unavailable)?,
                    nanos: 0,
                }),
            })
        })
        .transpose()?;
    let soft_delete_policy = record
        .soft_delete_policy
        .as_ref()
        .map(|policy| {
            Ok::<_, CatalogError>(pb::bucket::SoftDeletePolicy {
                effective_time: Some(policy.effective_time.into()),
                retention_duration: Some(Duration {
                    seconds: crate::grpc::convert::signed(
                        policy.retention_duration_seconds,
                        "retention_duration_seconds",
                    )
                    .map_err(crate::storage::failure::unavailable)?,
                    nanos: 0,
                }),
            })
        })
        .transpose()?;
    Ok(pb::Bucket {
        name: format!("projects/_/buckets/{}", record.name),
        bucket_id: record.name.clone(),
        etag: crate::json::bucket_etag(record),
        lifecycle: lifecycle(&record.lifecycle),
        // Cloud Storage returns the project number here, and the Java SDK parses it as a number.
        // The local registry has no project numbers, so this matches JSON `projectNumber`.
        project: format!("projects/{}", crate::acl::project_number(&record.project)),
        metageneration: record.metageneration,
        location: record.location.clone(),
        location_type: record.location_type.clone(),
        storage_class: record.storage_class.clone(),
        autoclass: record
            .autoclass
            .as_ref()
            .map(|config| pb::bucket::Autoclass {
                enabled: config.enabled,
                toggle_time: Some(config.toggle_time.into()),
                terminal_storage_class: config.terminal_storage_class.clone(),
                terminal_storage_class_update_time: config
                    .terminal_storage_class_update_time
                    .map(Into::into),
            }),
        labels: record.labels.clone().into_iter().collect(),
        create_time: Some(record.time_created.into()),
        update_time: Some(record.updated.into()),
        default_event_based_hold: record.default_event_based_hold,
        // Cloud Storage omits the message for a bucket whose versioning is currently off, as JSON does.
        versioning: record
            .versioning_enabled
            .then_some(pb::bucket::Versioning { enabled: true }),
        cors: record
            .cors
            .iter()
            .map(|rule| pb::bucket::Cors {
                origin: rule.origin.clone(),
                method: rule.method.clone(),
                response_header: rule.response_header.clone(),
                max_age_seconds: rule.max_age_seconds.unwrap_or_default(),
            })
            .collect(),
        retention_policy,
        soft_delete_policy,
        object_retention: record
            .object_retention_enabled
            .then_some(pb::bucket::ObjectRetention { enabled: true }),
        iam_config: Some(pb::bucket::IamConfig {
            public_access_prevention: if record.public_access_prevention_enforced {
                "enforced"
            } else {
                "inherited"
            }
            .into(),
            uniform_bucket_level_access: Some(pb::bucket::iam_config::UniformBucketLevelAccess {
                enabled: record.uniform_bucket_level_access_locked_time.is_some(),
                lock_time: record
                    .uniform_bucket_level_access_locked_time
                    .map(Into::into),
            }),
        }),
        ..Default::default()
    })
}

fn millis(time: std::time::SystemTime) -> Timestamp {
    unix_nanos_timestamp(UnixNanos::from(time).floor_to_millis())
}

const INVALID_CUSTOM_TIME: &str =
    "Stored customTime is invalid; repair the catalog or restore a valid backup";

#[derive(Debug)]
pub(crate) enum ObjectConversionError {
    InvalidCustomTime(ObjectError),
    Storage(ObjectError),
}

impl std::fmt::Display for ObjectConversionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCustomTime(_) => formatter.write_str(INVALID_CUSTOM_TIME),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ObjectConversionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidCustomTime(error) | Self::Storage(error) => Some(error),
        }
    }
}

impl From<ObjectError> for ObjectConversionError {
    fn from(error: ObjectError) -> Self {
        Self::Storage(error)
    }
}

impl From<ObjectConversionError> for tonic::Status {
    fn from(error: ObjectConversionError) -> Self {
        match error {
            ObjectConversionError::InvalidCustomTime(_) => Self::internal(INVALID_CUSTOM_TIME),
            ObjectConversionError::Storage(error) => error.into(),
        }
    }
}

impl From<ObjectConversionError> for ObjectError {
    fn from(error: ObjectConversionError) -> Self {
        match error {
            ObjectConversionError::InvalidCustomTime(_) => Self::InvalidMetadata,
            ObjectConversionError::Storage(error) => error,
        }
    }
}

pub(crate) fn object(record: &ObjectRecord) -> Result<pb::Object, ObjectConversionError> {
    let custom_time = record
        .custom_time
        .as_ref()
        .map(|time| {
            crate::object::custom_time_key(time)
                .map(|(seconds, nanos)| Timestamp {
                    seconds,
                    nanos: i32::try_from(nanos).unwrap_or_default(),
                })
                .map_err(ObjectConversionError::InvalidCustomTime)
        })
        .transpose()?;
    Ok(pb::Object {
        name: record.name.clone(),
        bucket: format!("projects/_/buckets/{}", record.bucket),
        // The same validator as the JSON resource, so either API's value names one version.
        etag: crate::json::object_etag(record),
        generation: record.generation,
        metageneration: record.metageneration,
        size: crate::grpc::convert::signed(record.size, "size")?,
        storage_class: record.storage_class.clone(),
        content_type: record.content_type.clone(),
        content_encoding: record.content_encoding.clone().unwrap_or_default(),
        content_disposition: record.content_disposition.clone().unwrap_or_default(),
        content_language: record.content_language.clone().unwrap_or_default(),
        cache_control: record.cache_control.clone().unwrap_or_default(),
        metadata: record.metadata.clone().into_iter().collect(),
        // Cloud Storage returned these three at millisecond precision over gRPC while
        // `finalize_time` kept its microseconds (observed 2026-11-01).
        create_time: Some(millis(record.time_created)),
        update_time: Some(millis(record.updated)),
        // A storage class changes only by rewriting into a new generation, as in JSON
        // `timeStorageClassUpdated`.
        update_storage_class_time: Some(millis(record.time_created)),
        // Catalog records are finalized; the appendable adapter clears this for unfinalized
        // generations. The same stored time as JSON `timeFinalized`.
        finalize_time: Some(record.time_created.into()),
        delete_time: record.time_deleted.map(Into::into),
        soft_delete_time: record.soft_delete_time.map(Into::into),
        hard_delete_time: record.hard_delete_time.map(Into::into),
        // gRPC requests cannot supply a customer key here, so encrypted objects hide their hashes.
        checksums: record
            .customer_key_sha256
            .is_none()
            .then(|| pb::ObjectChecksums {
                crc32c: Some(record.crc32c),
                md5_hash: if !record.has_md5() {
                    Vec::new()
                } else {
                    record.md5.to_vec()
                },
            }),
        customer_encryption: record
            .customer_key_sha256
            .map(|sha256| pb::CustomerEncryption {
                encryption_algorithm: crate::csek::ALGORITHM.into(),
                key_sha256_bytes: sha256.to_vec(),
            }),
        component_count: crate::grpc::convert::component_count(record.component_count)?,
        temporary_hold: record.temporary_hold == Some(true),
        // gRPC reports the optional field as present even for a never-set hold (recorded, v2);
        // the three states exist only in the JSON resource.
        event_based_hold: Some(record.event_based_hold == Some(true)),
        // Cloud Storage returned retention_expire_time at millisecond precision while
        // retention.retain_until_time kept its nanoseconds (observed 2026-10-31).
        retention_expire_time: record
            .earliest_deletion_time()
            .map(|time| unix_nanos_timestamp(time.floor_to_millis())),
        retention: record.retention.map(|retention| pb::object::Retention {
            mode: match retention.mode {
                RetentionMode::Unlocked => pb::object::retention::Mode::Unlocked,
                RetentionMode::Locked => pb::object::retention::Mode::Locked,
            }
            .into(),
            retain_until_time: Some(unix_nanos_timestamp(retention.retain_until)),
        }),
        custom_time,
        ..Default::default()
    })
}

/// Object ACL entries rendered by the shared JSON renderer, so IDs, etags and entity details
/// agree with `objectAccessControls`.
pub(crate) fn object_acl(
    object: &pb::Object,
    bucket: &str,
    project: &str,
    entries: &[AclEntry],
) -> Vec<pb::ObjectAccessControl> {
    let owner = AclOwner {
        kind: AclKind::Object,
        bucket,
        object: Some((&object.name, object.generation)),
        metageneration: object.metageneration,
    };
    entries
        .iter()
        .map(|entry| {
            // The base URL only reaches `selfLink`, which the protobuf resource does not carry.
            let value = crate::json::acl::acl_entry_resource("", owner, entry);
            let mut acl = object_access_control(&value);
            // Cloud Storage repeats a project-team entity under the project ID (observed
            // 2026-11-01).
            acl.entity_alt = acl
                .project_team
                .as_ref()
                .map(|team| format!("project-{}-{project}", team.team))
                .unwrap_or_default();
            acl
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::{self, Projection};
    use std::{
        collections::BTreeMap,
        time::{Duration as StdDuration, UNIX_EPOCH},
    };

    #[test]
    fn enum_wire_values_keep_json_xml_and_grpc_output() {
        let mut record = crate::grpc::test_support::object_record("bucket", UNIX_EPOCH);
        for class in [
            "STANDARD",
            "NEARLINE",
            "COLDLINE",
            "ARCHIVE",
            "MULTI_REGIONAL",
            "DURABLE_REDUCED_AVAILABILITY",
        ] {
            record.storage_class = class.into();
            for (hold, json_hold, grpc_hold) in [
                (None, None, false),
                (Some(false), Some(serde_json::json!(false)), false),
                (Some(true), Some(serde_json::json!(true)), true),
            ] {
                record.event_based_hold = hold;
                record.temporary_hold = hold;
                let resource =
                    json::object_resource(&record, "http://localhost", Projection::NoAcl).unwrap();
                assert_eq!(resource["storageClass"], class, "{class} {hold:?}");
                assert_eq!(
                    resource.get("eventBasedHold"),
                    json_hold.as_ref(),
                    "{class} {hold:?}"
                );
                assert_eq!(
                    resource.get("temporaryHold"),
                    json_hold.as_ref(),
                    "{class} {hold:?}"
                );
                let headers =
                    crate::xml::media_headers(&record, crate::xml::CacheScope::Private).unwrap();
                assert_eq!(headers["x-goog-storage-class"], class, "{class} {hold:?}");
                assert!(
                    !headers.contains_key("x-goog-temporary-hold"),
                    "{class} {hold:?}"
                );
                assert!(
                    !headers.contains_key("x-goog-event-based-hold"),
                    "{class} {hold:?}"
                );
                let output = object(&record).unwrap();
                assert_eq!(output.storage_class, class, "{class} {hold:?}");
                assert_eq!(output.event_based_hold, Some(grpc_hold), "{class} {hold:?}");
                assert_eq!(output.temporary_hold, grpc_hold, "{class} {hold:?}");
            }
        }
        for (kind, role) in [
            (AclKind::Bucket, "WRITER"),
            (AclKind::Bucket, "OWNER"),
            (AclKind::Object, "READER"),
            (AclKind::Object, "OWNER"),
        ] {
            let entry = AclEntry {
                entity: "allUsers".into(),
                role: role.parse().unwrap(),
            };
            let value = crate::json::acl::acl_entry_resource(
                "http://localhost",
                AclOwner {
                    kind,
                    bucket: "bucket",
                    object: Some(("object", 7)),
                    metageneration: 1,
                },
                &entry,
            );
            assert_eq!(value["role"], role, "{kind:?} {role}");
            assert_eq!(bucket_access_control(&value).role, role, "{kind:?} {role}");
            assert_eq!(object_access_control(&value).role, role, "{kind:?} {role}");
            assert_eq!(
                serde_json::to_string(&entry).unwrap(),
                format!(r#"{{"entity":"allUsers","role":"{role}"}}"#),
                "{kind:?} {role}"
            );
        }
    }

    #[test]
    fn out_of_range_bucket_durations_keep_catalog_corruption_guidance() {
        let record = crate::storage::test_fixtures::bucket("bucket", "project");
        for invalid in [
            BucketRecord {
                retention_policy: Some(crate::bucket::RetentionPolicy {
                    retention_period: u64::MAX,
                    effective_time: UNIX_EPOCH,
                    is_locked: false,
                }),
                ..record.clone()
            },
            BucketRecord {
                soft_delete_policy: Some(crate::bucket::SoftDeletePolicy {
                    retention_duration_seconds: u64::MAX,
                    effective_time: UNIX_EPOCH,
                }),
                ..record
            },
        ] {
            let status = bucket(&invalid).unwrap_err();
            assert_eq!(status.code(), tonic::Code::Unavailable);
            assert_eq!(status.message(), "Bucket catalog is unavailable");
        }
    }

    #[test]
    fn completion_encoders_keep_integer_corruption_and_metadata_errors_distinct() {
        let record = crate::grpc::test_support::object_record("bucket", std::time::UNIX_EPOCH);
        for invalid in [
            ObjectRecord {
                size: u64::MAX,
                ..record.clone()
            },
            ObjectRecord {
                component_count: Some(i64::MAX),
                ..record.clone()
            },
        ] {
            let error = object(&invalid).unwrap_err();
            assert!(
                std::error::Error::source(&error)
                    .unwrap()
                    .is::<ObjectError>()
            );
            assert!(matches!(ObjectError::from(error), ObjectError::Corrupt(_)));
            assert_eq!(
                object(&invalid)
                    .map_err(tonic::Status::from)
                    .unwrap_err()
                    .code(),
                tonic::Code::Unavailable
            );
        }
        assert!(matches!(
            object(&ObjectRecord {
                custom_time: Some("corrupt".into()),
                ..record
            })
            .map_err(ObjectError::from),
            Err(ObjectError::InvalidMetadata)
        ));
    }

    #[test]
    fn grpc_record_conversion() {
        let now = UNIX_EPOCH + StdDuration::new(1_700_000_000, 123_456_000);
        let bucket_record = BucketRecord {
            name: "example-bucket".into(),
            project: "123".into(),
            location: "US".into(),
            location_type: "multi-region".into(),
            storage_class: "STANDARD".into(),
            autoclass: None,
            labels: BTreeMap::from([("team".into(), "dev".into())]),
            cors: vec![crate::bucket::CorsRule {
                origin: vec!["https://example.invalid".into()],
                method: vec!["GET".into()],
                response_header: vec!["Content-Type".into()],
                max_age_seconds: Some(300),
            }],
            lifecycle: vec![],
            retention_policy: Some(crate::bucket::RetentionPolicy {
                retention_period: 60,
                effective_time: now,
                is_locked: true,
            }),
            soft_delete_policy: Some(crate::bucket::SoftDeletePolicy {
                retention_duration_seconds: 604800,
                effective_time: now,
            }),
            object_retention_enabled: false,
            versioning_enabled: true,
            versioning_configured: true,
            default_event_based_hold: true,
            public_access_prevention_enforced: false,
            uniform_bucket_level_access_locked_time: Some(now),
            metageneration: 3,
            generation: 0,
            time_created: now,
            updated: now,
        };
        let value =
            json::bucket_resource(&bucket_record, "http://localhost", Projection::NoAcl).unwrap();
        let proto = bucket(&bucket_record).unwrap();
        assert_eq!(proto.project, "projects/123");
        let named = BucketRecord {
            project: "local-project".into(),
            ..bucket_record.clone()
        };
        // SDKs parse the project as a number, so an ID with no registered number renders as 0.
        assert_eq!(bucket(&named).unwrap().project, "projects/0");
        assert_eq!(proto.bucket_id, value["name"]);
        assert_eq!(proto.storage_class, value["storageClass"]);
        assert_eq!(proto.location, value["location"]);
        assert_eq!(proto.metageneration.to_string(), value["metageneration"]);
        assert_eq!(proto.labels["team"], value["labels"]["team"]);
        assert!(proto.versioning.unwrap().enabled);
        assert_eq!(
            proto.cors[0].max_age_seconds,
            value["cors"][0]["maxAgeSeconds"]
        );
        assert_eq!(
            proto
                .retention_policy
                .unwrap()
                .retention_duration
                .unwrap()
                .seconds
                .to_string(),
            value["retentionPolicy"]["retentionPeriod"]
        );
        assert_eq!(
            proto
                .soft_delete_policy
                .unwrap()
                .retention_duration
                .unwrap()
                .seconds
                .to_string(),
            value["softDeletePolicy"]["retentionDurationSeconds"]
        );
        assert!(
            proto
                .iam_config
                .unwrap()
                .uniform_bucket_level_access
                .unwrap()
                .enabled
        );
        assert_eq!(proto.create_time.unwrap().nanos, 123_456_000);
        assert!(proto.acl.is_empty() && proto.lifecycle.is_none() && proto.encryption.is_none());

        let record = crate::grpc::test_support::object_record(&bucket_record.name, now);
        let value = json::object_resource(&record, "http://localhost", Projection::NoAcl).unwrap();
        let proto = object(&record).unwrap();
        assert_eq!(proto.name, value["name"]);
        assert_eq!(proto.size.to_string(), value["size"]);
        assert_eq!(proto.generation.to_string(), value["generation"]);
        assert_eq!(proto.content_encoding, value["contentEncoding"]);
        assert_eq!(proto.content_language, value["contentLanguage"]);
        assert_eq!(proto.cache_control, value["cacheControl"]);
        assert_eq!(proto.metadata["key"], value["metadata"]["key"]);
        assert_eq!(proto.checksums.unwrap().md5_hash, record.md5);
        assert_eq!(proto.custom_time.unwrap().nanos, 123_456_789);
        // The bucket fixture time carries microseconds; the object keeps only the milliseconds,
        // except finalize_time.
        assert_eq!(proto.create_time.unwrap().nanos, 123_000_000);
        assert_eq!(proto.update_time.unwrap().nanos, 123_000_000);
        assert_eq!(proto.update_storage_class_time.unwrap().nanos, 123_000_000);
        assert_eq!(proto.finalize_time.unwrap().nanos, 123_456_000);
        assert!(proto.acl.is_empty() && proto.kms_key.is_empty());
        let composite = object(&ObjectRecord {
            component_count: Some(i64::from(i32::MAX)),
            ..record.clone()
        })
        .unwrap();
        assert_eq!(composite.component_count, i32::MAX);
        assert!(composite.checksums.unwrap().md5_hash.is_empty());
        let multipart = object(&ObjectRecord {
            md5_availability: crate::object::Md5Availability::Unavailable,
            ..record.clone()
        })
        .unwrap();
        assert_eq!(multipart.component_count, 0);
        let checksums = multipart.checksums.unwrap();
        assert_eq!(checksums.crc32c, Some(record.crc32c));
        assert!(checksums.md5_hash.is_empty());
        let encrypted = object(&ObjectRecord {
            customer_key_sha256: Some([7; 32]),
            ..record.clone()
        })
        .unwrap();
        assert!(encrypted.checksums.is_none());
        assert_eq!(
            encrypted.customer_encryption,
            Some(pb::CustomerEncryption {
                encryption_algorithm: "AES256".into(),
                key_sha256_bytes: vec![7; 32],
            })
        );
        assert_eq!(
            object(&ObjectRecord {
                custom_time: Some("corrupt".into()),
                ..record
            })
            .map_err(tonic::Status::from)
            .unwrap_err()
            .code(),
            tonic::Code::Internal
        );
    }
}
