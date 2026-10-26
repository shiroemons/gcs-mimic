//! Source identities enter only through trusted, checksum-verified publication.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{Admin, AdminError, BucketName, Generation, ObjectName, Publication};
use crate::object::{ObjectRecord, UploadChunk};
use std::{io, time::SystemTime};
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug)]
pub struct ImportedIdentity {
    generation: Generation,
    time_created: SystemTime,
    updated: SystemTime,
}

impl ImportedIdentity {
    pub fn new(
        generation: Generation,
        time_created: SystemTime,
        updated: SystemTime,
    ) -> Result<Self, AdminError> {
        for time in [time_created, updated] {
            let micros = super::super::unix_micros(time).map_err(AdminError::from)?;
            if super::super::system_time_micros(micros)
                .map_err(|error| AdminError::Invalid(error.to_string()))?
                != time
            {
                return Err(AdminError::Invalid(
                    "imported timestamps must have exact microsecond precision".into(),
                ));
            }
        }
        Ok(Self {
            generation,
            time_created,
            updated,
        })
    }

    pub fn generation(self) -> Generation {
        self.generation
    }
    pub fn time_created(self) -> SystemTime {
        self.time_created
    }
    pub fn updated(self) -> SystemTime {
        self.updated
    }
}

impl Admin {
    pub async fn publish_imported_object(
        &self,
        bucket: &BucketName,
        name: &ObjectName,
        request: Publication,
        identity: ImportedIdentity,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
    ) -> Result<ObjectRecord, AdminError> {
        if request.expected.md5.is_none() || request.expected.crc32c.is_none() {
            return Err(AdminError::Invalid(
                "imported publication requires MD5 and CRC32C".into(),
            ));
        }
        if request.mode != super::PublishMode::Replace {
            return Err(AdminError::Invalid(
                "imported publication requires Replace mode".into(),
            ));
        }
        self.publish_with_identity(bucket, name, request, Some(identity), body)
            .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{
        clock::Clock,
        object::{ExpectedHashes, ObjectMetadataUpdate},
        storage::{
            Catalog,
            admin::{BucketDefinition, GenerationCondition, Origin, PublishMode},
        },
        test_support::TestDir,
    };
    use bytes::Bytes;
    use md5::{Digest, Md5};
    use std::{
        sync::Arc,
        time::{Duration, UNIX_EPOCH},
    };

    struct FixedClock;
    impl Clock for FixedClock {
        fn now(&self) -> SystemTime {
            UNIX_EPOCH + Duration::from_secs(1)
        }
    }

    async fn setup(versioning: bool) -> (TestDir, Catalog, Admin, BucketName, ObjectName) {
        let dir = TestDir::new();
        let catalog = Catalog::open_with_clock(&dir.0, Arc::new(FixedClock))
            .await
            .unwrap();
        let admin = catalog.admin(Origin::Import);
        let bucket = BucketName::new("import-bucket").unwrap();
        let mut definition =
            BucketDefinition::default_for(&bucket, "project", &FixedClock).unwrap();
        definition.0.versioning_enabled = versioning;
        admin.ensure_bucket(definition).await.unwrap();
        (
            dir,
            catalog,
            admin,
            bucket,
            ObjectName::new("source").unwrap(),
        )
    }

    fn identity(generation: i64) -> ImportedIdentity {
        ImportedIdentity::new(
            Generation::new(generation).unwrap(),
            UNIX_EPOCH - Duration::from_micros(1_234_567),
            UNIX_EPOCH + Duration::from_micros(9_876_543),
        )
        .unwrap()
    }

    fn request() -> Publication {
        Publication {
            content_type: "text/plain".into(),
            metadata: ObjectMetadataUpdate::default(),
            expected: ExpectedHashes {
                md5: Some(Md5::digest(b"data").into()),
                crc32c: Some(crc32c::crc32c(b"data")),
            },
            condition: None,
            max_bytes: 1024,
            mode: PublishMode::Replace,
        }
    }

    fn body() -> mpsc::Receiver<Result<UploadChunk, io::Error>> {
        let (sender, receiver) = mpsc::channel(2);
        sender
            .try_send(Ok(UploadChunk::Data(Bytes::from_static(b"data"))))
            .unwrap();
        sender.try_send(Ok(UploadChunk::End)).unwrap();
        receiver
    }

    #[test]
    fn imported_identity_rejects_lossy_precision() {
        let generation = Generation::new(1).unwrap();
        for time in [
            UNIX_EPOCH + Duration::from_nanos(100),
            UNIX_EPOCH - Duration::from_nanos(100),
        ] {
            assert!(matches!(
                ImportedIdentity::new(generation, time, UNIX_EPOCH),
                Err(AdminError::Invalid(_))
            ));
            assert!(matches!(
                ImportedIdentity::new(generation, UNIX_EPOCH, time),
                Err(AdminError::Invalid(_))
            ));
        }
        for micros in [i64::MIN, 0, i64::MAX] {
            let time = super::super::super::system_time_micros(micros).unwrap();
            assert!(ImportedIdentity::new(generation, time, time).is_ok());
        }
        assert!(
            ImportedIdentity::new(
                generation,
                UNIX_EPOCH + Duration::from_micros(i64::MAX as u64 + 1),
                UNIX_EPOCH
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn imported_identity_preserves_times_hashes_history_and_allocator_after_restart() {
        let (dir, catalog, admin, bucket, name) = setup(true).await;
        let source = identity(9_000_000_000_000_000);
        let mut record = admin
            .publish_imported_object(&bucket, &name, request(), source, body())
            .await
            .unwrap();
        assert_eq!(record.generation, source.generation().get());
        assert_eq!(
            (record.time_created, record.updated),
            (source.time_created(), source.updated())
        );
        assert_eq!(record.md5, request().expected.md5.unwrap());
        assert_eq!(record.crc32c, request().expected.crc32c.unwrap());
        admin
            .publish_imported_object(&bucket, &name, request(), identity(7), body())
            .await
            .unwrap();
        record.time_deleted = Some(FixedClock.now());
        assert_eq!(
            admin
                .publish_imported_object(&bucket, &name, request(), source, body())
                .await,
            Err(AdminError::Precondition)
        );
        assert_eq!(
            admin
                .get_object(&bucket, &name, None)
                .await
                .unwrap()
                .generation,
            7
        );
        assert_eq!(
            admin
                .get_object(&bucket, &name, Some(source.generation()))
                .await
                .unwrap(),
            record
        );
        let mut stale = request();
        stale.condition = Some(GenerationCondition::Is(source.generation()));
        assert_eq!(
            admin
                .publish_imported_object(&bucket, &name, stale, identity(8), body())
                .await,
            Err(AdminError::Precondition)
        );
        catalog.close().await.unwrap();
        let catalog = Catalog::open_with_clock(&dir.0, Arc::new(FixedClock))
            .await
            .unwrap();
        let admin = catalog.admin(Origin::Import);
        assert_eq!(
            admin
                .get_object(&bucket, &name, Some(source.generation()))
                .await
                .unwrap(),
            record
        );
        let (_, mut reader) = admin
            .open_object(&bucket, &name, Some(source.generation()))
            .await
            .unwrap();
        assert_eq!(reader.next_chunk().await.unwrap().unwrap(), &b"data"[..]);
        assert!(reader.next_chunk().await.unwrap().is_none());
        let ordinary = admin
            .publish_object(&bucket, &name, request(), body())
            .await
            .unwrap();
        assert_eq!(ordinary.generation, source.generation().get() + 1);
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn imported_identity_rejects_bad_hashes_without_mutation_or_blob_leaks() {
        let (dir, catalog, admin, bucket, name) = setup(false).await;
        let original = admin
            .publish_object(&bucket, &name, request(), body())
            .await
            .unwrap();
        for field in ["md5", "crc32c", "missing"] {
            let mut invalid = request();
            match field {
                "md5" => invalid.expected.md5 = Some([0; 16]),
                "crc32c" => invalid.expected.crc32c = Some(0),
                _ => invalid.expected = ExpectedHashes::default(),
            }
            let failure = admin
                .publish_imported_object(&bucket, &name, invalid, identity(i64::MAX), body())
                .await
                .unwrap_err();
            if field == "missing" {
                assert!(matches!(failure, AdminError::Invalid(_)));
            } else {
                assert_eq!(failure, AdminError::Checksum);
            }
            assert_eq!(
                admin.get_object(&bucket, &name, None).await.unwrap(),
                original
            );
        }
        let next = admin
            .publish_object(&bucket, &name, request(), body())
            .await
            .unwrap();
        assert_eq!(next.generation, original.generation + 1);
        assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
        let blobs: usize = std::fs::read_dir(dir.0.join("blobs"))
            .unwrap()
            .map(|entry| std::fs::read_dir(entry.unwrap().path()).unwrap().count())
            .sum();
        assert_eq!(blobs, 1);
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn imported_identity_rejects_modes_that_could_discard_source_identity() {
        let (dir, catalog, admin, bucket, name) = setup(false).await;
        let original = admin
            .publish_object(&bucket, &name, request(), body())
            .await
            .unwrap();
        for mode in [
            PublishMode::InheritFromReplaced,
            PublishMode::SkipIfSameHash,
            PublishMode::InheritAndSkipIfSameHash,
        ] {
            let mut input = request();
            input.mode = mode;
            assert!(matches!(
                admin
                    .publish_imported_object(&bucket, &name, input, identity(i64::MAX), body())
                    .await,
                Err(AdminError::Invalid(_))
            ));
            assert_eq!(
                admin.get_object(&bucket, &name, None).await.unwrap(),
                original
            );
            assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
        }
        let imported = admin
            .publish_imported_object(&bucket, &name, request(), identity(7), body())
            .await
            .unwrap();
        assert_eq!(imported.generation, 7);
        let next = admin
            .publish_object(&bucket, &name, request(), body())
            .await
            .unwrap();
        assert_eq!(next.generation, original.generation + 1);
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn imported_identity_collision_is_atomic_and_exhaustion_is_safe() {
        let (_dir, catalog, admin, bucket, name) = setup(false).await;
        let (left, right) = tokio::join!(
            admin.publish_imported_object(&bucket, &name, request(), identity(i64::MAX), body()),
            admin.publish_imported_object(&bucket, &name, request(), identity(i64::MAX), body()),
        );
        assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
        assert_eq!(left.err().or(right.err()), Some(AdminError::Precondition));
        assert!(
            admin
                .publish_object(&bucket, &name, request(), body())
                .await
                .is_err()
        );
        assert_eq!(
            admin
                .get_object(&bucket, &name, None)
                .await
                .unwrap()
                .generation,
            i64::MAX
        );
        let other = ObjectName::new("other").unwrap();
        admin
            .publish_imported_object(&bucket, &other, request(), identity(i64::MAX), body())
            .await
            .unwrap();
        let third = ObjectName::new("third").unwrap();
        admin
            .publish_imported_object(&bucket, &third, request(), identity(2), body())
            .await
            .unwrap();
        catalog.close().await.unwrap();
    }
}
