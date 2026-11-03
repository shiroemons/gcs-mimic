use super::*;
use crate::{bucket::BucketAccess, storage::catalog::resumable::ResumableSession};
use std::time::{Duration, UNIX_EPOCH};

#[test]
fn resumable_retention_overflow_returns_the_publication_metadata_error() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE buckets(name TEXT, retention_policy TEXT); INSERT INTO buckets VALUES('bucket',NULL)").unwrap();
    let mut options = ResumableOptions::new(BucketAccess::Permissive);
    options.content_type = "text/plain".into();
    let session = ResumableSession {
        id: "0".repeat(64),
        blob_id: "0".repeat(32).parse().unwrap(),
        bucket: "bucket".into(),
        name: "object".into(),
        incarnation: "0".repeat(32),
        options: options.clone(),
        total: Some(0),
        offset: 0,
        created: UNIX_EPOCH,
        deadline: UNIX_EPOCH + Duration::from_secs(604800),
        state: SessionState::Active,
        completion: None,
    };
    for period in [10, i64::MAX] {
        connection
            .execute(
                "UPDATE buckets SET retention_policy=?1",
                [serde_json::json!({"retention_period":period}).to_string()],
            )
            .unwrap();
        let record = publication_record(
            &connection,
            &session,
            options.clone(),
            1,
            objects::PublicationDefaults {
                storage_class: crate::bucket::StorageClass::Standard,
                time: UNIX_EPOCH + Duration::from_secs(1),
                event_based_hold: false,
                owner: None,
            },
            Hashes {
                md5: [0; 16],
                crc32c: 0,
            },
            0,
        );
        if period == 10 {
            assert_eq!(
                record.unwrap().retention_expiration_time,
                Some(UNIX_EPOCH + Duration::from_secs(11))
            );
        } else {
            let error = record.unwrap_err();
            assert_eq!(error, ObjectError::InvalidMetadata);
            assert_eq!(error.to_string(), "custom metadata exceeds 8 KiB");
        }
    }
}
