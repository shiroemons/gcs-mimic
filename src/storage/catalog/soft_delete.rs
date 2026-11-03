use super::{Catalog, objects, unix_micros};
use crate::storage::catalog::sql;
use crate::{
    bucket::BucketAccess,
    object::{ObjectConditions, ObjectError, ObjectRecord, ObjectTarget},
};
use objects::{SELECT_SOFT_DELETED_GENERATION, authorized_bucket, object_unavailable, read_object};
use rusqlite::{Connection, OptionalExtension, params};

/// The two authorities a restore checks in its publication transaction.
#[derive(Clone, Copy)]
pub(crate) struct RestoreAccess<'a> {
    pub(crate) restore: &'a BucketAccess,
    pub(crate) create: &'a BucketAccess,
}

pub(super) fn retention_seconds(connection: &Connection, bucket: &str) -> Result<i64, ObjectError> {
    sql::scalar(connection,
            "SELECT COALESCE(json_extract(soft_delete_policy, '$.retention_duration_seconds'), 0) FROM buckets WHERE name = ?1", [bucket])
        .map_err(object_unavailable)
}

pub(super) fn purge_expired(
    connection: &mut Connection,
    bucket: &str,
    now: i64,
) -> Result<(), ObjectError> {
    let transaction = sql::immediate(connection)?;
    sql::exec(
        &transaction,
        "INSERT OR IGNORE INTO pending_blob_deletions SELECT blob_id FROM objects WHERE bucket = ?1 AND soft_delete_time IS NOT NULL AND hard_delete_time <= ?2 ORDER BY hard_delete_time LIMIT 100",
        params![bucket, now],
    )?;
    sql::exec(
        &transaction,
        "DELETE FROM objects WHERE blob_id IN (SELECT blob_id FROM objects WHERE bucket = ?1 AND soft_delete_time IS NOT NULL AND hard_delete_time <= ?2 ORDER BY hard_delete_time LIMIT 100)",
        params![bucket, now],
    )?;
    transaction.commit().map_err(object_unavailable)
}

fn selected(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: i64,
    now: i64,
) -> Result<objects::StoredObject, ObjectError> {
    sql::row(
        connection,
        SELECT_SOFT_DELETED_GENERATION,
        params![bucket, name, generation, now],
        read_object,
    )
    .optional()?
    .ok_or(ObjectError::ObjectNotFound)
}

impl Catalog {
    pub(crate) async fn get_soft_deleted_object(
        &self,
        target: ObjectTarget<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        let ObjectTarget {
            bucket,
            name,
            generation,
        } = target;
        let (access, clock) = (access.clone(), self.0.clock.clone());
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            let object_acl = objects::object_acl_authority(&transaction, &bucket, &access)?;
            let objects::StoredObject { record, .. } = selected(
                &transaction,
                &bucket,
                &name,
                generation,
                unix_micros(clock.now())?,
            )
            .map_err(|error| {
                if object_acl.is_some() && error == ObjectError::ObjectNotFound {
                    ObjectError::Forbidden
                } else {
                    error
                }
            })?;
            if let Some(access) = object_acl
                && !super::acl::object_granted(
                    &transaction,
                    &bucket,
                    &name,
                    record.generation,
                    access,
                )?
            {
                return Err(ObjectError::Forbidden);
            }
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
    pub(crate) async fn restore_object(
        &self,
        target: ObjectTarget<i64>,
        conditions: ObjectConditions,
        access: RestoreAccess<'_>,
    ) -> Result<ObjectRecord, ObjectError> {
        let ObjectTarget {
            bucket,
            name,
            generation,
        } = target;
        let (restore_access, create_access) = (access.restore.clone(), access.create.clone());
        let clock = self.0.clock.clone();
        let backend = self.0.backend.clone();
        let result = self
            .run(move |connection| {
                authorized_bucket(connection, &bucket, &restore_access)?;
                let now = clock.now();
                let micros = unix_micros(now)?;
                purge_expired(connection, &bucket, micros)?;
                let id = super::super::blob_id::BlobId::generate().map_err(object_unavailable)?;
                let text = id.to_hex();
                // Durable ownership precedes linking; recovery reclaims uncommitted output.
                sql::exec(
                    connection,
                    "INSERT INTO pending_blob_deletions VALUES (?1)",
                    [&text],
                )?;
                let result = (|| {
                    let transaction = sql::immediate(connection)?;
                    authorized_bucket(&transaction, &bucket, &restore_access)?;
                    let (_, delete_permission) =
                        authorized_bucket(&transaction, &bucket, &create_access)?;
                    let objects::StoredObject {
                        mut record,
                        blob: original,
                    } = match selected(&transaction, &bucket, &name, generation, micros) {
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
                        delete_permission,
                        now,
                    )?;
                    let (versioning, _) =
                        Catalog::bucket_settings_in_transaction(&transaction, &bucket)?;
                    backend.publish_pin(&original, &id)?;
                    record.metageneration = 1;
                    record.time_created = super::system_time_micros(micros)?;
                    record.updated = record.time_created;
                    record.time_deleted = None;
                    record.soft_delete_time = None;
                    record.hard_delete_time = None;
                    let (record, _, ()) = objects::publish_in_transaction(
                        &transaction,
                        previous.as_ref(),
                        versioning,
                        objects::NewVersion {
                            blob_id: id,
                            assignment: objects::GenerationAssignment::Automatic,
                            micros,
                            origin: super::change_journal::origin(&create_access),
                            notification_mode: crate::notification::NotificationMode::for_access(
                                &create_access,
                            ),
                            owner_profile: None,
                            acl: super::acl::PublicationAcl::InheritDefault,
                        },
                        |generation| {
                            record.generation = generation;
                            Ok((record, ()))
                        },
                    )?;
                    sql::exec(
                        &transaction,
                        "DELETE FROM pending_blob_deletions WHERE blob_id = ?1",
                        [&text],
                    )?;
                    transaction.commit()?;
                    Ok(record)
                })();
                if !backend.is_memory()
                    && let Err(error) = backend.remove_draft(&id)
                {
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
