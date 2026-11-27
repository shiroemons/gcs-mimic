//! Catalog persistence and keyed reads for customer-supplied encryption keys.
//!
//! Only the key's SHA-256 fingerprint is persisted. Checks run inside the same catalog call
//! that selects the generation, so a key is always verified against the record whose bytes
//! (or hashes) are returned.

use super::sql;
use super::{
    Catalog,
    derived::{DerivedSource, DerivedSourceSpec},
    objects::{self, ObjectReader, RequestKind},
    read_cache::{self, CachedObject},
    read_pool::ReadPath,
};
#[cfg(test)]
use crate::object::ObjectKey;
use crate::{
    bucket::BucketAccess,
    csek::{CustomerKey, StoredHashes, check_data},
    object::{ObjectConditions, ObjectError, ObjectRecord, ObjectTarget, ReadAhead},
};

/// An object opened for its metadata alone.
pub(crate) struct KeyedMetadata {
    pub record: ObjectRecord,
    pub reader: ObjectReader,
    pub hashes: StoredHashes,
}

/// Whether to fetch the body's start while opening the object. A stored-gzip response depends on
/// the bytes and may be chunked or transcoded, which the streaming path decides.
fn wants_read_ahead(record: &ObjectRecord, read_ahead: ReadAhead) -> bool {
    read_ahead == ReadAhead::Body
        && !record
            .content_encoding
            .as_deref()
            .is_some_and(|encoding| encoding.eq_ignore_ascii_case("gzip"))
}

pub(super) const SCHEMA: &str = "ALTER TABLE objects ADD COLUMN customer_key_sha256 BLOB CHECK(customer_key_sha256 IS NULL OR length(customer_key_sha256) = 32); PRAGMA user_version = 30;";

/// Reads the stored key fingerprint at `index`, `None` for an object without a customer key.
pub(super) fn read(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<[u8; 32]>> {
    sql::optional_fixed_blob(row, index, "invalid customer key fingerprint length")
}

pub(super) fn check(record: &ObjectRecord, key: Option<CustomerKey>) -> Result<(), ObjectError> {
    check_data(record.customer_key_sha256, key).map_err(ObjectError::CustomerEncryption)
}

impl Catalog {
    /// Open object data after verifying `key` against the selected generation.
    pub(crate) async fn open_object_keyed(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
        key: Option<CustomerKey>,
        read_ahead: ReadAhead,
    ) -> Result<(ObjectRecord, ObjectReader), ObjectError> {
        let shareable = key.is_none() && read_cache::is_shareable(&target, access);
        let hit = if shareable {
            self.cached_object(&target).await
        } else {
            None
        };
        if let Some(hit) = &hit {
            objects::check_conditions(Some(&hit.record), conditions, RequestKind::Read)?;
            if let Some(mut reader) = ObjectReader::from_cached(hit) {
                if wants_read_ahead(&hit.record, read_ahead) {
                    reader.read_ahead_cached();
                }
                return Ok((hit.record.clone(), reader));
            }
        }
        let revision = self.0.reads.cache.revision();
        let catalog = self.clone();
        let access = access.clone();
        self.run_read_on(move |connection, path| {
            let (record, mut reader) =
                objects::open_selected(connection, &catalog, &target, conditions, &access)?;
            check(&record, key)?;
            if path == ReadPath::Pooled && wants_read_ahead(&record, read_ahead) {
                reader.read_ahead_on_pool();
            }
            if shareable && path.stores_answers() {
                catalog.0.reads.cache.store_object(
                    revision,
                    CachedObject {
                        record: record.clone(),
                        download: Some(reader.cached_download()),
                    },
                );
            }
            Ok((record, reader))
        })
        .await
    }

    /// Open for a metadata-only response (XML HEAD): no key is needed, a supplied key must
    /// match.
    pub(crate) async fn open_object_metadata_keyed(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
        key: Option<CustomerKey>,
    ) -> Result<KeyedMetadata, ObjectError> {
        let catalog = self.clone();
        let access = access.clone();
        self.run_read(move |connection| {
            let (record, reader) =
                objects::open_selected(connection, &catalog, &target, conditions, &access)?;
            let hashes = crate::csek::check_metadata(record.customer_key_sha256, key)
                .map_err(ObjectError::CustomerEncryption)?;
            Ok(KeyedMetadata {
                record,
                reader,
                hashes,
            })
        })
        .await
    }

    /// Resolve a copy/rewrite source whose bytes require `key`.
    pub(crate) async fn resolve_derived_source_keyed(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
        key: Option<CustomerKey>,
    ) -> Result<DerivedSource, ObjectError> {
        self.resolve_derived_sources_keyed(
            vec![DerivedSourceSpec {
                target,
                conditions,
                access: access.clone(),
            }],
            key,
        )
        .await
        .and_then(|sources| {
            sources
                .into_iter()
                .next()
                .ok_or_else(|| objects::object_unavailable("missing derived source"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_fixtures::{PERMISSIVE, body, bucket, live};
    use crate::{csek::CsekError, object::ObjectMetadataUpdate, test_support::TestDir};
    use rusqlite::Connection;

    async fn setup(dir: &TestDir) -> Catalog {
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        catalog
    }

    async fn put(catalog: &Catalog, name: &str, key: Option<[u8; 32]>) -> ObjectRecord {
        let body = body(vec![bytes::Bytes::from_static(b"secret")]);
        let permit = catalog
            .prepare_object_upload(
                ObjectKey::from_parts_unchecked("bucket", name),
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        catalog
            .put_object_prechecked(
                crate::storage::ObjectUpload {
                    key: ObjectKey::from_parts_unchecked("bucket", name),
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
                live("bucket", "secret"),
                Default::default(),
                PERMISSIVE,
                Some(CustomerKey::from_sha256(sha256)),
                crate::object::ReadAhead::None,
            )
            .await
            .unwrap();
        assert_eq!(record.customer_key_sha256, Some(sha256));
        // Transports that cannot carry a key (gRPC, legacy v1) use the keyless entry points.
        assert_eq!(
            catalog
                .open_object(live("bucket", "secret"), Default::default(), PERMISSIVE)
                .await
                .err(),
            Some(ObjectError::CustomerEncryption(
                CsekError::ResourceIsEncrypted
            ))
        );
        assert_eq!(
            catalog
                .resolve_derived_source(live("bucket", "secret"), Default::default(), PERMISSIVE)
                .await
                .err(),
            Some(ObjectError::CustomerEncryption(
                CsekError::ResourceIsEncrypted
            ))
        );
        let opened = catalog
            .open_object_metadata_keyed(
                live("bucket", "secret"),
                Default::default(),
                PERMISSIVE,
                None,
            )
            .await
            .unwrap();
        assert_eq!(opened.hashes, StoredHashes::Withheld);
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
            .open_object(live("bucket", "plain"), Default::default(), PERMISSIVE)
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
