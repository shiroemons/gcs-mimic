use super::pb;
use crate::{
    bucket::BucketRecord,
    object::{ObjectRecord, retention::RetentionMode},
};
use prost_types::{Duration, Timestamp};

pub fn bucket(record: &BucketRecord) -> pb::Bucket {
    pb::Bucket {
        name: format!("projects/_/buckets/{}", record.name),
        bucket_id: record.name.clone(),
        project: format!("projects/{}", record.project),
        metageneration: record.metageneration,
        location: record.location.clone(),
        location_type: record.location_type.clone(),
        storage_class: record.storage_class.clone(),
        labels: record.labels.clone().into_iter().collect(),
        create_time: Some(record.time_created.into()),
        update_time: Some(record.updated.into()),
        default_event_based_hold: record.default_event_based_hold,
        versioning: Some(pb::bucket::Versioning {
            enabled: record.versioning_enabled,
        }),
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
        retention_policy: record.retention_policy.as_ref().map(|policy| {
            pb::bucket::RetentionPolicy {
                effective_time: Some(policy.effective_time.into()),
                is_locked: policy.is_locked,
                retention_duration: Some(Duration {
                    seconds: policy.retention_period as i64,
                    nanos: 0,
                }),
            }
        }),
        soft_delete_policy: record.soft_delete_policy.as_ref().map(|policy| {
            pb::bucket::SoftDeletePolicy {
                effective_time: Some(policy.effective_time.into()),
                retention_duration: Some(Duration {
                    seconds: policy.retention_duration_seconds as i64,
                    nanos: 0,
                }),
            }
        }),
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
    }
}

pub fn object(record: &ObjectRecord) -> Result<pb::Object, tonic::Status> {
    let custom_time = record.custom_time.as_ref().map(|time| {
        crate::object::custom_time_key(time)
            .map(|(seconds, nanos)| Timestamp { seconds, nanos: nanos as i32 })
            .map_err(|_| tonic::Status::internal("Stored customTime is invalid; repair the catalog or restore a valid backup"))
    }).transpose()?;
    Ok(pb::Object {
        name: record.name.clone(),
        bucket: format!("projects/_/buckets/{}", record.bucket),
        generation: record.generation,
        metageneration: record.metageneration,
        size: record.size as i64,
        storage_class: record.storage_class.clone(),
        content_type: record.content_type.clone(),
        content_encoding: record.content_encoding.clone().unwrap_or_default(),
        content_disposition: record.content_disposition.clone().unwrap_or_default(),
        content_language: record.content_language.clone().unwrap_or_default(),
        cache_control: record.cache_control.clone().unwrap_or_default(),
        metadata: record.metadata.clone().into_iter().collect(),
        create_time: Some(record.time_created.into()),
        update_time: Some(record.updated.into()),
        delete_time: record.time_deleted.map(Into::into),
        soft_delete_time: record.soft_delete_time.map(Into::into),
        hard_delete_time: record.hard_delete_time.map(Into::into),
        // gRPC requests cannot supply a customer key here, so encrypted objects hide their hashes.
        checksums: record
            .customer_key_sha256
            .is_none()
            .then(|| pb::ObjectChecksums {
                crc32c: Some(record.crc32c),
                md5_hash: if record.component_count.is_some() {
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
        component_count: record.component_count.unwrap_or_default() as i32,
        temporary_hold: record.temporary_hold,
        event_based_hold: Some(record.event_based_hold),
        retention_expire_time: record.earliest_deletion_time().map(Into::into),
        retention: record.retention.map(|retention| pb::object::Retention {
            mode: match retention.mode {
                RetentionMode::Unlocked => pb::object::retention::Mode::Unlocked,
                RetentionMode::Locked => pb::object::retention::Mode::Locked,
            } as i32,
            retain_until_time: Some(retention.retain_until.into()),
        }),
        custom_time,
        ..Default::default()
    })
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
    fn grpc_record_conversion() {
        let now = UNIX_EPOCH + StdDuration::new(1_700_000_000, 123_456_000);
        let bucket_record = BucketRecord {
            name: "example-bucket".into(),
            project: "123".into(),
            location: "US".into(),
            location_type: "multi-region".into(),
            storage_class: "STANDARD".into(),
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
            default_event_based_hold: true,
            public_access_prevention_enforced: false,
            uniform_bucket_level_access_locked_time: Some(now),
            metageneration: 3,
            time_created: now,
            updated: now,
        };
        let value =
            json::bucket_resource(&bucket_record, "http://localhost", Projection::NoAcl).unwrap();
        let proto = bucket(&bucket_record);
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

        let record = ObjectRecord {
            bucket: bucket_record.name,
            name: "folder/%2F.txt".into(),
            generation: 42,
            metageneration: 2,
            content_type: "text/plain".into(),
            content_encoding: Some("gzip".into()),
            content_disposition: None,
            content_language: Some("en".into()),
            cache_control: Some("private".into()),
            metadata: BTreeMap::from([("key".into(), "value".into())]),
            size: 5,
            md5: [3; 16],
            crc32c: 123,
            component_count: None,
            storage_class: "STANDARD".into(),
            time_created: now,
            updated: now,
            time_deleted: None,
            soft_delete_time: None,
            hard_delete_time: None,
            event_based_hold: false,
            temporary_hold: true,
            custom_time: Some("2026-10-20T01:02:03.123456789Z".into()),
            retention_expiration_time: None,
            retention: None,
            customer_key_sha256: None,
        };
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
        assert!(proto.acl.is_empty() && proto.kms_key.is_empty());
        let composite = object(&ObjectRecord {
            component_count: Some(i64::from(i32::MAX)),
            ..record.clone()
        })
        .unwrap();
        assert_eq!(composite.component_count, i32::MAX);
        assert!(composite.checksums.unwrap().md5_hash.is_empty());
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
            .unwrap_err()
            .code(),
            tonic::Code::Internal
        );
    }
}
