use super::{iam, pb};
use crate::{
    acl::AclEntry,
    auth,
    bucket::{
        BucketRecord, CatalogError,
        autoclass::AutoclassState,
        lifecycle::{
            BucketLifecycle, LifecycleAction, LifecycleDate, LifecycleRule, LifecycleRules,
        },
    },
    json::acl::AclOwner,
    object::{ObjectError, ObjectRecord, retention::RetentionMode, unix_nanos::UnixNanos},
};
use prost_types::{Duration, Timestamp};
use serde_json::{Map, Value, json};

/// A retention time as a protobuf timestamp, straight from its seconds and nanoseconds so no
/// platform clock resolution is involved.
fn unix_nanos_timestamp(time: UnixNanos) -> Timestamp {
    Timestamp {
        seconds: time.secs(),
        nanos: i32::try_from(time.subsec_nanos()).unwrap_or_default(),
    }
}

/// A bucket's rules as Storage v2 reports them; stored rules the contract refuses are corrupt.
pub(super) fn lifecycle(
    lifecycle: &BucketLifecycle,
) -> Result<Option<pb::bucket::Lifecycle>, tonic::Status> {
    let rules = lifecycle.rules()?;
    if rules.is_empty() {
        return Ok(None);
    }
    Ok(Some(pb::bucket::Lifecycle {
        rule: rules
            .into_iter()
            .map(proto_rule)
            .collect::<Result<_, _>>()?,
    }))
}

/// Requested rules, validated by the JSON contract so both APIs report the same errors.
pub(super) fn lifecycle_rules(
    value: Option<&pb::bucket::Lifecycle>,
) -> Result<LifecycleRules, tonic::Status> {
    let rules = value
        .into_iter()
        .flat_map(|value| &value.rule)
        .map(lifecycle_rule)
        .collect();
    LifecycleRules::from_json(&Value::Object(Map::from_iter([(
        "rule".into(),
        Value::Array(rules),
    )])))
    .map_err(super::bucket_update::input_error)
}

/// The JSON spelling of a proto date, which the lifecycle contract then validates like any
/// JSON date.
fn date_text(date: &crate::grpc::generated::google::r#type::Date) -> String {
    format!("{:04}-{:02}-{:02}", date.year, date.month, date.day)
}

fn proto_date(date: &LifecycleDate) -> crate::grpc::generated::google::r#type::Date {
    crate::grpc::generated::google::r#type::Date {
        year: date.year(),
        month: date.month(),
        day: date.day(),
    }
}

fn lifecycle_rule(value: &pb::bucket::lifecycle::Rule) -> Value {
    let action = value.action.as_ref().map(|action| {
        json!({
            "type": action.r#type,
            "storageClass": (!action.storage_class.is_empty()).then_some(&action.storage_class),
        })
    });
    let condition = value.condition.as_ref().map(|condition| {
        json!({
            "age": condition.age_days,
            "createdBefore": condition.created_before.as_ref().map(date_text),
            "customTimeBefore": condition.custom_time_before.as_ref().map(date_text),
            "daysSinceCustomTime": condition.days_since_custom_time,
            "daysSinceNoncurrentTime": condition.days_since_noncurrent_time,
            "isLive": condition.is_live,
            "matchesPrefix": condition.matches_prefix,
            "matchesStorageClass": condition.matches_storage_class,
            "matchesSuffix": condition.matches_suffix,
            "noncurrentTimeBefore": condition.noncurrent_time_before.as_ref().map(date_text),
            "numNewerVersions": condition.num_newer_versions,
        })
    });
    json!({"action":action, "condition":condition})
}

/// A typed rule as Storage v2 reports it. A function rather than `TryFrom`: rustc's dead-code
/// analysis treats an impl for a generated type that only the impl builds as unused, and with
/// it the date accessors it calls.
fn proto_rule(value: &LifecycleRule) -> Result<pb::bucket::lifecycle::Rule, ObjectError> {
    let condition = &value.condition;
    if condition.size_above_bytes.is_some() || condition.size_below_bytes.is_some() {
        return Err(ObjectError::UnsupportedMetadata(
            "lifecycle sizeAboveBytes/sizeBelowBytes cannot be represented by Storage v2; use the JSON API".into(),
        ));
    }
    let (kind, storage_class) = match value.action {
        LifecycleAction::Delete => ("Delete", ""),
        LifecycleAction::SetStorageClass(class) => ("SetStorageClass", class.as_str()),
        LifecycleAction::AbortIncompleteMultipartUpload => ("AbortIncompleteMultipartUpload", ""),
    };
    Ok(pb::bucket::lifecycle::Rule {
        action: Some(pb::bucket::lifecycle::rule::Action {
            r#type: kind.into(),
            storage_class: storage_class.into(),
        }),
        condition: Some(pb::bucket::lifecycle::rule::Condition {
            age_days: condition.age.map(i32::from),
            created_before: condition.created_before.as_ref().map(proto_date),
            custom_time_before: condition.custom_time_before.as_ref().map(proto_date),
            days_since_custom_time: condition.days_since_custom_time.map(i32::from),
            days_since_noncurrent_time: condition.days_since_noncurrent_time.map(i32::from),
            is_live: condition.is_live,
            matches_prefix: condition.matches_prefix.clone(),
            matches_storage_class: condition
                .matches_storage_class
                .iter()
                .map(|class| class.as_str().into())
                .collect(),
            matches_suffix: condition.matches_suffix.clone(),
            noncurrent_time_before: condition.noncurrent_time_before.as_ref().map(proto_date),
            num_newer_versions: condition.num_newer_versions.map(i32::from),
        }),
    })
}

macro_rules! access_control {
    ($name:ident, $type:path) => {
        /// Takes every field from the helpers the JSON renderer uses, so both transports derive
        /// the same ids, etags and entity fields.
        pub(crate) fn $name(owner: AclOwner<'_>, entry: &AclEntry) -> $type {
            let entity = &entry.entity;
            $type {
                role: entry.role.as_str().into(),
                id: owner.entry_id(entity).unwrap_or_default(),
                entity: entity.as_str().into(),
                entity_alt: String::new(),
                entity_id: entity.entity_id().unwrap_or_default().into(),
                etag: owner.etag(),
                email: entity.email().unwrap_or_default().into(),
                domain: entity.domain().unwrap_or_default().into(),
                project_team: entity.team().map(|(team, project)| pb::ProjectTeam {
                    project_number: project.into(),
                    team: team.as_str().into(),
                }),
            }
        }
    };
}

access_control!(bucket_access_control, pb::BucketAccessControl);
access_control!(object_access_control, pb::ObjectAccessControl);

/// Whether a bucket resource carries its lifecycle rules. A read whose mask leaves them out
/// must not fail on rules Storage v2 cannot represent (size conditions).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lifecycle {
    Include,
    Omit,
}

pub(crate) fn bucket(
    record: &BucketRecord,
    lifecycle: Lifecycle,
) -> Result<pb::Bucket, tonic::Status> {
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
        lifecycle: match lifecycle {
            Lifecycle::Include => self::lifecycle(&record.lifecycle)?,
            Lifecycle::Omit => None,
        },
        // Cloud Storage returns the project number here, and the Java SDK parses it as a number.
        // The local registry has no project numbers, so this matches JSON `projectNumber`.
        project: format!("projects/{}", crate::acl::project_number(&record.project)),
        metageneration: record.metageneration,
        location: record.location.as_str().into(),
        location_type: record.location.kind().as_str().into(),
        storage_class: record.storage_class.as_str().into(),
        autoclass: record.autoclass.as_ref().map(|config| {
            let (terminal_storage_class, terminal_storage_class_update_time) = match config.state {
                AutoclassState::Enabled {
                    terminal_storage_class,
                    terminal_storage_class_update_time,
                } => (
                    Some(terminal_storage_class.as_str().into()),
                    Some(terminal_storage_class_update_time.into()),
                ),
                AutoclassState::Disabled => (None, None),
            };
            pb::bucket::Autoclass {
                enabled: config.is_enabled(),
                toggle_time: Some(config.toggle_time.into()),
                terminal_storage_class,
                terminal_storage_class_update_time,
            }
        }),
        labels: record
            .labels
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        create_time: Some(record.time_created.into()),
        update_time: Some(record.updated.into()),
        default_event_based_hold: record.default_event_based_hold,
        // Cloud Storage omits the message for a bucket whose versioning is currently off, as JSON does.
        versioning: record
            .versioning
            .is_enabled()
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
            public_access_prevention: record.public_access_prevention.as_str().into(),
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
                .map(unix_nanos_timestamp)
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
        storage_class: record.storage_class.as_str().into(),
        content_type: record.content_type.clone(),
        content_encoding: record.content_encoding.clone().unwrap_or_default(),
        content_disposition: record.content_disposition.clone().unwrap_or_default(),
        content_language: record.content_language.clone().unwrap_or_default(),
        cache_control: record.cache_control.clone().unwrap_or_default(),
        metadata: record
            .metadata
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        // Cloud Storage returned these three at millisecond precision over gRPC while
        // `finalize_time` kept its microseconds (observed 2026-11-01).
        create_time: Some(millis(record.time_created)),
        update_time: Some(millis(record.updated)),
        update_storage_class_time: Some(millis(record.storage_class_updated())),
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
        temporary_hold: record.temporary_hold.is_held(),
        // gRPC reports the optional field as present even for a never-set hold (recorded, v2);
        // the three states exist only in the JSON resource.
        event_based_hold: Some(record.event_based_hold.is_held()),
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

/// Object ACL entries built from the helpers the JSON renderer uses, so IDs, etags and entity
/// details agree with `objectAccessControls`.
pub(crate) fn object_acl(
    object: &pb::Object,
    bucket: &str,
    project: &str,
    entries: &[AclEntry],
) -> Vec<pb::ObjectAccessControl> {
    let owner = AclOwner::Object {
        bucket,
        name: &object.name,
        generation: object.generation,
        metageneration: object.metageneration,
    };
    entries
        .iter()
        .map(|entry| {
            let mut acl = object_access_control(owner, entry);
            // Cloud Storage repeats a project-team entity under the project ID (observed
            // 2026-11-01).
            acl.entity_alt = entry
                .entity
                .team()
                .map(|(team, _)| format!("project-{}-{project}", team.as_str()))
                .unwrap_or_default();
            acl
        })
        .collect()
}

/// A stored binding as IAM reports it; conditions are not supported, so none is stored.
impl From<auth::Binding> for iam::Binding {
    fn from(binding: auth::Binding) -> Self {
        Self {
            role: binding.role.as_str().into(),
            members: binding
                .members
                .into_iter()
                .map(auth::Member::into_string)
                .collect(),
            condition: None,
        }
    }
}

/// A requested binding, before the shared policy rules validate it.
impl From<iam::Binding> for auth::policy_input::BindingInput {
    fn from(binding: iam::Binding) -> Self {
        Self {
            role: binding.role,
            members: binding.members,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acl::AclKind;
    use crate::json::{self, Projection};
    use std::{
        collections::BTreeMap,
        time::{Duration as StdDuration, UNIX_EPOCH},
    };

    #[test]
    fn grpc_lifecycle_roundtrips_every_condition_through_shared_rules() {
        let stored = vec![
            serde_json::json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":30,"createdBefore":"2026-01-01","customTimeBefore":"2026-02-01","daysSinceCustomTime":2,"daysSinceNoncurrentTime":3,"isLive":false,"matchesPrefix":["a/"],"matchesStorageClass":["STANDARD"],"matchesSuffix":[".txt"],"noncurrentTimeBefore":"2026-03-01","numNewerVersions":4}}),
        ];
        let read = BucketLifecycle::from_stored(stored.clone());
        let proto = lifecycle(&read).unwrap().unwrap();
        assert_eq!(lifecycle_rules(Some(&proto)).unwrap().to_values(), stored);
        let typed = LifecycleRules::from_stored(&stored).unwrap().into_vec();
        assert_eq!(proto_rule(&typed[0]).unwrap(), proto.rule[0]);
        for (mut rule, field) in [
            (proto.rule[0].clone(), "age_days"),
            (proto.rule[0].clone(), "created_before"),
            (proto.rule[0].clone(), "action.type"),
            (proto.rule[0].clone(), "action.storage_class"),
        ] {
            match field {
                "age_days" => rule.condition.as_mut().unwrap().age_days = Some(-1),
                "created_before" => {
                    rule.condition.as_mut().unwrap().created_before =
                        Some(crate::grpc::generated::google::r#type::Date {
                            year: 2025,
                            month: 2,
                            day: 29,
                        })
                }
                "action.type" => rule.action.as_mut().unwrap().r#type = "Unknown".into(),
                _ => rule.action.as_mut().unwrap().storage_class.clear(),
            }
            let error =
                lifecycle_rules(Some(&pb::bucket::Lifecycle { rule: vec![rule] })).unwrap_err();
            assert_eq!(error.code(), tonic::Code::InvalidArgument);
            // Cloud Storage's unknown-action text names the rejected value, not the field.
            let named = if field == "action.type" {
                "Unknown"
            } else {
                field
            };
            assert!(error.message().contains(named), "{error}");
        }
    }

    #[test]
    fn grpc_lifecycle_refuses_size_conditions_instead_of_dropping_them() {
        for field in ["sizeAboveBytes", "sizeBelowBytes"] {
            let stored =
                vec![serde_json::json!({"action":{"type":"Delete"},"condition":{field:"1"}})];
            let error = lifecycle(&BucketLifecycle::from_stored(stored))
                .expect_err("protobuf has no size condition fields");
            assert_eq!(error.code(), tonic::Code::Unimplemented);
            assert!(error.message().contains(field));
            assert!(error.message().contains("JSON API"));
        }
    }

    #[test]
    fn grpc_lifecycle_rejects_integer_overflow_without_truncation() {
        let rule = |age: i64| json!({"action":{"type":"Delete"},"condition":{"age":age}});
        let converted = lifecycle(&BucketLifecycle::from_stored(vec![rule(i64::from(
            i32::MAX,
        ))]))
        .unwrap()
        .unwrap();
        assert_eq!(
            converted.rule[0].condition.as_ref().unwrap().age_days,
            Some(i32::MAX)
        );
        // A stored count past int32 cannot become a typed rule, so it reads as corrupt rather
        // than wrapping.
        let error = lifecycle(&BucketLifecycle::from_stored(vec![rule(
            i64::from(i32::MAX) + 1,
        )]))
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unavailable);
    }

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
            record.storage_class = class.parse().unwrap();
            for (hold, json_hold, grpc_hold) in [
                (None, None, false),
                (Some(false), Some(serde_json::json!(false)), false),
                (Some(true), Some(serde_json::json!(true)), true),
            ] {
                record.event_based_hold = hold.into();
                record.temporary_hold = hold.into();
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
        let bucket_owner = AclOwner::Bucket {
            bucket: "bucket",
            metageneration: 1,
        };
        let object_owner = AclOwner::Object {
            bucket: "bucket",
            name: "object",
            generation: 7,
            metageneration: 1,
        };
        for (kind, owner, role) in [
            (AclKind::Bucket, bucket_owner, "WRITER"),
            (AclKind::Bucket, bucket_owner, "OWNER"),
            (AclKind::Object, object_owner, "READER"),
            (AclKind::Object, object_owner, "OWNER"),
        ] {
            let entry = AclEntry {
                entity: "allUsers".parse().unwrap(),
                role: role.parse().unwrap(),
            };
            let value = crate::json::acl::acl_entry_resource("http://localhost", owner, &entry);
            assert_eq!(value["role"], role, "{kind:?} {role}");
            assert_eq!(
                bucket_access_control(owner, &entry).role,
                role,
                "{kind:?} {role}"
            );
            assert_eq!(
                object_access_control(owner, &entry).role,
                role,
                "{kind:?} {role}"
            );
            assert_eq!(
                serde_json::to_string(&entry).unwrap(),
                format!(r#"{{"entity":"allUsers","role":"{role}"}}"#),
                "{kind:?} {role}"
            );
        }
    }

    /// Both transports describe an ACL entry with the same ID, etag and derived entity fields,
    /// for every kind of entity and of owner.
    #[test]
    fn access_controls_agree_with_the_json_resource() {
        let owners = [
            AclOwner::Bucket {
                bucket: "bucket",
                metageneration: 3,
            },
            AclOwner::DefaultObject { metageneration: 3 },
            AclOwner::Object {
                bucket: "bucket",
                name: "dir/object",
                generation: 1_700_000_000_000_000,
                metageneration: 2,
            },
        ];
        let entities = [
            "allUsers",
            "allAuthenticatedUsers",
            "user-liz@example.com",
            "user-00b4903a97",
            "group-team@example.com",
            "group-00b4903a97",
            "domain-example.com",
            "project-owners-123",
            "project-viewers-0",
        ];
        let text =
            |value: &Value, field: &str| value[field].as_str().unwrap_or_default().to_owned();
        for owner in owners {
            for entity in entities {
                let entry = AclEntry {
                    entity: entity.parse().unwrap(),
                    role: "OWNER".parse().unwrap(),
                };
                let json = crate::json::acl::acl_entry_resource("http://host", owner, &entry);
                let team = json.get("projectTeam").map(|team| pb::ProjectTeam {
                    project_number: text(team, "projectNumber"),
                    team: text(team, "team"),
                });
                let bucket = bucket_access_control(owner, &entry);
                let object = object_access_control(owner, &entry);
                for (role, id, name, entity_id, etag, email, domain, project_team) in [
                    (
                        &bucket.role,
                        &bucket.id,
                        &bucket.entity,
                        &bucket.entity_id,
                        &bucket.etag,
                        &bucket.email,
                        &bucket.domain,
                        &bucket.project_team,
                    ),
                    (
                        &object.role,
                        &object.id,
                        &object.entity,
                        &object.entity_id,
                        &object.etag,
                        &object.email,
                        &object.domain,
                        &object.project_team,
                    ),
                ] {
                    let case = format!("{owner:?} {entity}");
                    assert_eq!(*role, text(&json, "role"), "{case}");
                    assert_eq!(*id, text(&json, "id"), "{case}");
                    assert_eq!(*name, text(&json, "entity"), "{case}");
                    assert_eq!(*entity_id, text(&json, "entityId"), "{case}");
                    assert_eq!(*etag, text(&json, "etag"), "{case}");
                    assert_eq!(*email, text(&json, "email"), "{case}");
                    assert_eq!(*domain, text(&json, "domain"), "{case}");
                    assert_eq!(*project_team, team, "{case}");
                }
            }
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
            let status = bucket(&invalid, Lifecycle::Include).unwrap_err();
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

    #[tokio::test]
    async fn grpc_v2_outputs_the_stored_lifecycle_storage_class_timestamp() {
        let (original, changed) =
            crate::storage::test_fixtures::lifecycle_transitioned_object().await;
        assert_eq!(
            object(&original).unwrap().update_storage_class_time,
            Some(millis(original.time_created))
        );
        let result = object(&changed).unwrap();
        assert_eq!(
            result.update_storage_class_time,
            Some(millis(changed.storage_class_updated()))
        );
        assert_ne!(result.update_storage_class_time, result.update_time);
        assert_ne!(result.update_storage_class_time, result.create_time);
        assert_eq!(result.storage_class, "COLDLINE");
    }

    #[test]
    fn grpc_record_conversion() {
        let now = UNIX_EPOCH + StdDuration::new(1_700_000_000, 123_456_000);
        let bucket_record = BucketRecord {
            name: "example-bucket".into(),
            project: "123".into(),
            location: "US".parse().unwrap(),
            storage_class: crate::bucket::StorageClass::Standard,
            autoclass: None,
            labels: BTreeMap::from([("team".into(), "dev".into())]),
            cors: vec![crate::bucket::CorsRule {
                origin: vec!["https://example.invalid".into()],
                method: vec!["GET".into()],
                response_header: vec!["Content-Type".into()],
                max_age_seconds: Some(300),
            }],
            lifecycle: Default::default(),
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
            versioning: crate::bucket::Versioning::Enabled,
            default_event_based_hold: true,
            public_access_prevention: crate::bucket::PublicAccessPrevention::Inherited,
            uniform_bucket_level_access_locked_time: Some(now),
            metageneration: 3,
            generation: 0,
            time_created: now,
            updated: now,
        };
        let value =
            json::bucket_resource(&bucket_record, "http://localhost", Projection::NoAcl).unwrap();
        let proto = bucket(&bucket_record, Lifecycle::Include).unwrap();
        assert_eq!(proto.project, "projects/123");
        let named = BucketRecord {
            project: "local-project".into(),
            ..bucket_record.clone()
        };
        // SDKs parse the project as a number, so an ID with no registered number renders as 0.
        assert_eq!(
            bucket(&named, Lifecycle::Include).unwrap().project,
            "projects/0"
        );
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
