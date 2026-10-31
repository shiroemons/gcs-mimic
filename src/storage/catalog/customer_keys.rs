//! Catalog persistence and keyed reads for customer-supplied encryption keys.
//!
//! Only the key's SHA-256 fingerprint is persisted. Checks run inside the same catalog call
//! that selects the generation, so a key is always verified against the record whose bytes
//! (or hashes) are returned.

use super::{
    Catalog, CatalogError,
    derived::{DerivedSource, DerivedSourceSpec},
    objects::{self, ObjectReader},
};
use crate::{
    bucket::BucketAccess,
    csek::{CustomerKey, check_data},
    object::{ObjectConditions, ObjectError, ObjectRecord},
};
use rusqlite::Connection;

pub(super) const SCHEMA: &str = "ALTER TABLE objects ADD COLUMN customer_key_sha256 BLOB CHECK(customer_key_sha256 IS NULL OR length(customer_key_sha256) = 32); PRAGMA user_version = 30;";

pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(|error| {
        CatalogError::Unavailable(crate::storage::failure::BackendCause::from_error(error))
    })?;
    transaction.execute_batch(SCHEMA).map_err(|error| {
        CatalogError::Unavailable(crate::storage::failure::BackendCause::from_error(error))
    })?;
    transaction.commit().map_err(|error| {
        CatalogError::Unavailable(crate::storage::failure::BackendCause::from_error(error))
    })
}

pub(super) fn decode(value: Option<Vec<u8>>) -> rusqlite::Result<Option<[u8; 32]>> {
    value
        .map(|bytes| {
            bytes.try_into().map_err(|_| {
                rusqlite::Error::FromSqlConversionFailure(
                    27,
                    rusqlite::types::Type::Blob,
                    "invalid customer key fingerprint length".into(),
                )
            })
        })
        .transpose()
}

pub(super) fn check(record: &ObjectRecord, key: Option<CustomerKey>) -> Result<(), ObjectError> {
    check_data(record.customer_key_sha256, key).map_err(ObjectError::CustomerEncryption)
}

impl Catalog {
    /// Open object data after verifying `key` against the selected generation.
    pub(crate) async fn open_object_keyed(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
        key: Option<CustomerKey>,
    ) -> Result<(ObjectRecord, ObjectReader), ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let catalog = self.clone();
        let access = access.clone();
        self.run_object(move |connection| {
            let (record, reader) = objects::open_selected(
                connection, &catalog, &bucket, &name, generation, conditions, &access,
            )?;
            check(&record, key)?;
            Ok((record, reader))
        })
        .await
    }

    /// Open for a metadata-only response (XML HEAD): no key is needed, a supplied key must
    /// match. The returned flag says whether hashes of the stored bytes may be revealed.
    pub(crate) async fn open_object_metadata_keyed(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
        key: Option<CustomerKey>,
    ) -> Result<(ObjectRecord, ObjectReader, bool), ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let catalog = self.clone();
        let access = access.clone();
        self.run_object(move |connection| {
            let (record, reader) = objects::open_selected(
                connection, &catalog, &bucket, &name, generation, conditions, &access,
            )?;
            let reveal = crate::csek::check_metadata(record.customer_key_sha256, key)
                .map_err(ObjectError::CustomerEncryption)?;
            Ok((record, reader, reveal))
        })
        .await
    }

    /// Resolve a copy/rewrite source whose bytes require `key`.
    pub(crate) async fn resolve_derived_source_keyed(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
        key: Option<CustomerKey>,
    ) -> Result<DerivedSource, ObjectError> {
        self.resolve_derived_sources_keyed(
            vec![DerivedSourceSpec {
                bucket: bucket.into(),
                name: name.into(),
                generation,
                conditions,
                access: access.clone(),
            }],
            key,
        )
        .await
        .map(|mut sources| sources.remove(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bucket::BucketRecord,
        csek::CsekError,
        object::{ObjectMetadataUpdate, UploadChunk},
        test_support::TestDir,
    };
    use std::time::UNIX_EPOCH;

    async fn setup(dir: &TestDir) -> Catalog {
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog
            .insert_bucket(BucketRecord {
                name: "bucket".into(),
                project: "project".into(),
                location: "US".into(),
                location_type: "multi-region".into(),
                storage_class: "STANDARD".into(),
                autoclass: None,
                labels: Default::default(),
                versioning_enabled: false,
                default_event_based_hold: false,
                public_access_prevention_enforced: false,
                uniform_bucket_level_access_locked_time: None,
                lifecycle: Vec::new(),
                retention_policy: None,
                soft_delete_policy: None,
                object_retention_enabled: false,
                cors: Vec::new(),
                metageneration: 1,
                time_created: UNIX_EPOCH,
                updated: UNIX_EPOCH,
            })
            .await
            .unwrap();
        catalog
    }

    async fn put(catalog: &Catalog, name: &str, key: Option<[u8; 32]>) -> ObjectRecord {
        let (sender, body) = tokio::sync::mpsc::channel(2);
        sender
            .send(Ok(UploadChunk::Data(bytes::Bytes::from_static(b"secret"))))
            .await
            .unwrap();
        sender.send(Ok(UploadChunk::End)).await.unwrap();
        let permit = catalog
            .prepare_object_upload(
                "bucket",
                name,
                Default::default(),
                &BucketAccess::Permissive,
            )
            .await
            .unwrap();
        catalog
            .put_object_prechecked(
                crate::storage::ObjectUpload {
                    bucket: "bucket",
                    name,
                    content_type: "text/plain".into(),
                    expected: Default::default(),
                    conditions: Default::default(),
                    max_bytes: 1024,
                    metadata: ObjectMetadataUpdate {
                        customer_key_sha256: key,
                        ..Default::default()
                    },
                    access: BucketAccess::Permissive,
                },
                body,
                permit,
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn fingerprint_survives_restart_and_keyless_readers_are_refused() {
        let dir = TestDir::new();
        let catalog = setup(&dir).await;
        let sha256 = crate::csek::key_sha256(&[5; 32]);
        assert_eq!(
            put(&catalog, "secret", Some(sha256))
                .await
                .customer_key_sha256,
            Some(sha256)
        );
        catalog.close().await.unwrap();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        // Only the fingerprint is persisted: no catalog column can hold the raw key.
        let (record, _) = catalog
            .open_object_keyed(
                "bucket",
                "secret",
                None,
                Default::default(),
                &BucketAccess::Permissive,
                Some(CustomerKey::from_sha256(sha256)),
            )
            .await
            .unwrap();
        assert_eq!(record.customer_key_sha256, Some(sha256));
        // Transports that cannot carry a key (gRPC, legacy v1) use the keyless entry points.
        assert_eq!(
            catalog
                .open_object_authorized(
                    "bucket",
                    "secret",
                    None,
                    Default::default(),
                    &BucketAccess::Permissive
                )
                .await
                .err(),
            Some(ObjectError::CustomerEncryption(
                CsekError::ResourceIsEncrypted
            ))
        );
        assert_eq!(
            catalog
                .resolve_derived_source(
                    "bucket",
                    "secret",
                    None,
                    Default::default(),
                    &BucketAccess::Permissive
                )
                .await
                .err(),
            Some(ObjectError::CustomerEncryption(
                CsekError::ResourceIsEncrypted
            ))
        );
        let (_, _, reveal) = catalog
            .open_object_metadata_keyed(
                "bucket",
                "secret",
                None,
                Default::default(),
                &BucketAccess::Permissive,
                None,
            )
            .await
            .unwrap();
        assert!(!reveal);
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn schema_29_catalog_migrates_with_unencrypted_objects() {
        let dir = TestDir::new();
        let catalog = setup(&dir).await;
        put(&catalog, "plain", None).await;
        catalog.close().await.unwrap();
        let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
        super::super::test_support::downgrade_customer_key_schema(&connection);
        connection
            .execute_batch("PRAGMA user_version = 29;")
            .unwrap();
        drop(connection);
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let (record, _) = catalog
            .open_object_authorized(
                "bucket",
                "plain",
                None,
                Default::default(),
                &BucketAccess::Permissive,
            )
            .await
            .unwrap();
        assert_eq!(record.customer_key_sha256, None);
        catalog.close().await.unwrap();
        let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            super::super::CATALOG_SCHEMA_VERSION
        );
        // The column rejects anything but a 32-byte fingerprint.
        assert!(
            connection
                .execute("UPDATE objects SET customer_key_sha256 = zeroblob(16)", [])
                .is_err()
        );
    }
}
