use super::{Catalog, objects, unix_micros};
use crate::{
    bucket::BucketAccess,
    object::{ObjectConditions, ObjectError, ObjectRecord},
};
use objects::{SELECT_SOFT_DELETED_GENERATION, authorized_bucket, object_unavailable, read_object};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub(super) fn retention_seconds(connection: &Connection, bucket: &str) -> Result<i64, ObjectError> {
    connection
        .prepare_cached(
            "SELECT COALESCE(json_extract(soft_delete_policy, '$.retention_duration_seconds'), 0) FROM buckets WHERE name = ?1",
        )
        .and_then(|mut statement| statement.query_row([bucket], |row| row.get(0)))
        .map_err(object_unavailable)
}

pub(super) fn purge_expired(
    connection: &mut Connection,
    bucket: &str,
    now: i64,
) -> Result<(), ObjectError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(object_unavailable)?;
    transaction
        .prepare_cached(
            "INSERT OR IGNORE INTO pending_blob_deletions SELECT blob_id FROM objects WHERE bucket = ?1 AND soft_delete_time IS NOT NULL AND hard_delete_time <= ?2 ORDER BY hard_delete_time LIMIT 100",
        )
        .and_then(|mut statement| statement.execute(params![bucket, now]))
        .map_err(object_unavailable)?;
    transaction
        .prepare_cached(
            "DELETE FROM objects WHERE blob_id IN (SELECT blob_id FROM objects WHERE bucket = ?1 AND soft_delete_time IS NOT NULL AND hard_delete_time <= ?2 ORDER BY hard_delete_time LIMIT 100)",
        )
        .and_then(|mut statement| statement.execute(params![bucket, now]))
        .map_err(object_unavailable)?;
    transaction.commit().map_err(object_unavailable)
}

fn selected(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: i64,
    now: i64,
) -> Result<(ObjectRecord, String), ObjectError> {
    connection
        .prepare_cached(SELECT_SOFT_DELETED_GENERATION)
        .and_then(|mut statement| {
            statement.query_row(params![bucket, name, generation, now], read_object)
        })
        .optional()
        .map_err(object_unavailable)?
        .ok_or(ObjectError::ObjectNotFound)
}

impl Catalog {
    pub(crate) async fn get_soft_deleted_object_authorized(
        &self,
        bucket: &str,
        name: &str,
        generation: i64,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        let (bucket, name, access, clock) = (
            bucket.to_owned(),
            name.to_owned(),
            access.clone(),
            self.0.clock.clone(),
        );
        self.run_object(move |connection| {
            authorized_bucket(connection, &bucket, &access)?;
            let (record, _) = selected(
                connection,
                &bucket,
                &name,
                generation,
                unix_micros(clock.now()).map_err(object_unavailable)?,
            )?;
            objects::check_conditions(
                Some(&record),
                conditions,
                super::objects::RequestKind::Read,
            )?;
            Ok(record)
        })
        .await
    }

    /// Both permissions and live conditions are checked in the publication transaction.
    pub(crate) async fn restore_object_authorized(
        &self,
        bucket: &str,
        name: &str,
        generation: i64,
        conditions: ObjectConditions,
        restore_access: &BucketAccess,
        create_access: &BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        let (bucket, name) = (bucket.to_owned(), name.to_owned());
        let (restore_access, create_access) = (restore_access.clone(), create_access.clone());
        let (clock, data_dir) = (self.0.clock.clone(), self.0.data_dir.clone());
        let result = self
            .run_object(move |connection| {
                authorized_bucket(connection, &bucket, &restore_access)?;
                let now = clock.now();
                let micros = unix_micros(now).map_err(object_unavailable)?;
                purge_expired(connection, &bucket, micros)?;
                let id = super::super::blob_id::BlobId::generate().map_err(object_unavailable)?;
                let text = id.to_hex();
                // Durable ownership precedes linking; recovery reclaims uncommitted output.
                connection
                    .prepare_cached("INSERT INTO pending_blob_deletions VALUES (?1)")
                    .and_then(|mut statement| statement.execute([&text]))
                    .map_err(object_unavailable)?;
                let stage = id.staging_path(&data_dir);
                let result = (|| {
                    let transaction = connection
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .map_err(object_unavailable)?;
                    authorized_bucket(&transaction, &bucket, &restore_access)?;
                    let (_, allow_overwrite) =
                        authorized_bucket(&transaction, &bucket, &create_access)?;
                    let (mut record, original) =
                        match selected(&transaction, &bucket, &name, generation, micros) {
                            Err(ObjectError::ObjectNotFound) => {
                                return Err(
                                    match objects::selected(
                                        &transaction,
                                        &bucket,
                                        &name,
                                        Some(generation),
                                        Default::default(),
                                        super::objects::RequestKind::Write,
                                        &restore_access,
                                    ) {
                                        Ok(_) => ObjectError::ObjectNotSoftDeleted,
                                        Err(error) => error,
                                    },
                                );
                            }
                            result => result?,
                        };
                    record.check_retention(now)?;
                    let previous = objects::lookup(&transaction, &bucket, &name)?;
                    objects::check_publication(
                        previous.as_ref(),
                        conditions,
                        allow_overwrite,
                        now,
                    )?;
                    let (versioning, _) =
                        Catalog::bucket_settings_in_transaction(&transaction, &bucket)
                            .map_err(object_unavailable)?;
                    std::fs::hard_link(objects::blob_path(&data_dir, &original)?, &stage)
                        .map_err(object_unavailable)?;
                    objects::publish_blob(&stage, &id.blob_path(&data_dir))
                        .map_err(object_unavailable)?;
                    record.metageneration = 1;
                    record.time_created = now;
                    record.updated = now;
                    record.time_deleted = None;
                    record.soft_delete_time = None;
                    record.hard_delete_time = None;
                    let (record, _, ()) = objects::publish_in_transaction(
                        &transaction,
                        previous.as_ref(),
                        versioning,
                        objects::NewVersion {
                            blob_id: &text,
                            assignment: objects::GenerationAssignment::Automatic,
                            micros,
                            origin: super::change_journal::origin(&create_access),
                        },
                        |generation| {
                            record.generation = generation;
                            Ok((record, ()))
                        },
                    )?;
                    transaction
                        .prepare_cached("DELETE FROM pending_blob_deletions WHERE blob_id = ?1")
                        .and_then(|mut statement| statement.execute([&text]))
                        .map_err(object_unavailable)?;
                    transaction.commit().map_err(object_unavailable)?;
                    Ok(record)
                })();
                if let Err(error) = objects::unlink(&stage) {
                    crate::diagnostics::emit(
                        crate::diagnostics::LogLevel::Warn,
                        "restore_cleanup_deferred",
                        serde_json::json!({"message":error.to_string()}),
                    );
                }
                result
            })
            .await;
        self.reclaim_after_commit().await;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_delete_expiry_reclaims_at_most_one_hundred_rows_and_rolls_back_errors() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE objects(bucket TEXT, name TEXT, generation INTEGER, blob_id TEXT PRIMARY KEY, soft_delete_time INTEGER, hard_delete_time INTEGER);
            CREATE TABLE pending_blob_deletions(blob_id TEXT PRIMARY KEY);
            WITH RECURSIVE entries(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM entries WHERE n < 201)
            INSERT INTO objects SELECT 'bucket', 'item', n, printf('%032x',n), 1, 10 FROM entries;
            INSERT INTO objects VALUES('other','item',202,'other',1,10),('bucket','later',203,'later',1,20);").unwrap();
        purge_expired(&mut connection, "bucket", 10).unwrap();
        let remaining: i64 = connection
            .query_row("SELECT count(*) FROM objects", [], |row| row.get(0))
            .unwrap();
        let pending: i64 = connection
            .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!((remaining, pending), (103, 100));
        connection.execute_batch("CREATE TRIGGER reject_expiry BEFORE DELETE ON objects BEGIN SELECT RAISE(ABORT,'expiry rejected'); END;").unwrap();
        assert!(purge_expired(&mut connection, "bucket", 10).is_err());
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM objects", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            remaining
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            pending
        );
    }
}
