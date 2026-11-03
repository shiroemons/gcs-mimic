use super::{Catalog, objects, unix_micros};
use crate::storage::catalog::sql;
use crate::{
    bucket::BucketAccess,
    object::{ObjectConditions, ObjectError, ObjectRecord, ObjectTarget},
};
use objects::{SELECT_SOFT_DELETED_GENERATION, authorized_bucket, object_unavailable, read_object};
use rusqlite::{Connection, OptionalExtension, params};

#[cfg(all(test, unix))]
mod durability_tests;

/// The two authorities a restore checks in its publication transaction.
#[derive(Clone, Copy)]
pub(crate) struct RestoreAccess<'a> {
    pub(crate) restore: &'a BucketAccess,
    pub(crate) create: &'a BucketAccess,
}

pub(super) fn purge_expired(
    connection: &mut Connection,
    bucket: &str,
    now: i64,
) -> Result<(), ObjectError> {
    let transaction = sql::immediate(connection)?;
    let ids = transaction.prepare_cached(
        "DELETE FROM objects WHERE blob_id IN (SELECT blob_id FROM objects WHERE bucket = ?1 AND soft_delete_time IS NOT NULL AND hard_delete_time <= ?2 ORDER BY hard_delete_time, name, generation LIMIT 100) RETURNING blob_id",
    )?.query_map(params![bucket, now], |row| row.get::<_, crate::storage::BlobId>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        sql::exec(
            &transaction,
            "INSERT OR IGNORE INTO pending_blob_deletions VALUES (?1)",
            [id],
        )?;
    }
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

#[derive(Clone)]
struct RestoreRequest {
    target: ObjectTarget<i64>,
    conditions: ObjectConditions,
    restore: BucketAccess,
    create: BucketAccess,
}

fn restore_candidate(
    connection: &Connection,
    request: &RestoreRequest,
    now: objects::PublicationTime,
) -> Result<(objects::StoredObject, Option<objects::StoredObject>), ObjectError> {
    let ObjectTarget {
        bucket,
        name,
        generation,
    } = &request.target;
    authorized_bucket(connection, bucket, &request.restore)?;
    let (_, delete_permission) = authorized_bucket(connection, bucket, &request.create)?;
    let source = match selected(connection, bucket, name, *generation, now.micros) {
        Err(ObjectError::ObjectNotFound) => {
            return Err(
                match objects::selected(
                    connection,
                    bucket,
                    name,
                    Some(*generation),
                    Default::default(),
                    objects::RequestKind::Write,
                    &request.restore,
                ) {
                    Ok(_) => ObjectError::ObjectNotSoftDeleted,
                    Err(error) => error,
                },
            );
        }
        result => result?,
    };
    source.record.check_retention(now.time)?;
    let previous = objects::lookup(connection, bucket, name)?;
    objects::check_publication(
        previous.as_ref(),
        request.conditions,
        delete_permission,
        now.time,
    )?;
    Ok((source, previous))
}

fn restore_in_transaction(
    connection: &mut Connection,
    request: &RestoreRequest,
    source: crate::storage::BlobId,
    id: crate::storage::BlobId,
    published: Result<std::io::Result<()>, crate::task::BlockingFailed>,
    clock: &dyn crate::clock::Clock,
) -> Result<ObjectRecord, ObjectError> {
    let transaction = sql::immediate(connection)?;
    let now = objects::PublicationTime::read(clock)?;
    let (objects::StoredObject { mut record, blob }, previous) =
        restore_candidate(&transaction, request, now)?;
    if blob != source {
        return Err(object_unavailable(
            "soft-deleted source changed during restore",
        ));
    }
    published?.map_err(object_unavailable)?;
    let bucket = objects::BucketPublication::load(&transaction, &request.target.bucket)?;
    record.metageneration = 1;
    record.time_created = now.time;
    record.updated = now.time;
    record.time_deleted = None;
    record.soft_delete_time = None;
    record.hard_delete_time = None;
    let (record, _, ()) = objects::publish_in_transaction(
        &transaction,
        previous.as_ref(),
        &bucket,
        objects::NewVersion {
            blob_id: id,
            assignment: objects::GenerationAssignment::Automatic,
            micros: now.micros,
            origin: super::change_journal::origin(&request.create),
            notification_mode: crate::notification::NotificationMode::for_access(&request.create),
            owner_profile: None,
            acl: super::acl::PublicationAcl::InheritDefault,
        },
        |generation| {
            record.generation = generation;
            Ok((record, ()))
        },
    )?;
    objects::commit_publication(transaction)?;
    Ok(record)
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
        let request = RestoreRequest {
            target,
            conditions,
            restore: access.restore.clone(),
            create: access.create.clone(),
        };
        let catalog = self.clone();
        let permit = self.admit_object_upload()?;
        // Cancellation must retain ownership until publication and cleanup have settled.
        crate::task::spawn_supervised("object-restore", async move {
            let _permit = permit;
            catalog.restore_pinned(request).await
        })
        .await?
    }

    async fn restore_pinned(&self, request: RestoreRequest) -> Result<ObjectRecord, ObjectError> {
        let id = crate::storage::BlobId::generate().map_err(object_unavailable)?;
        let (backend, clock, selection) = (
            self.0.backend.clone(),
            self.0.clock.clone(),
            request.clone(),
        );
        let pinned = self
            .run_upload(move |connection| {
                authorized_bucket(connection, &selection.target.bucket, &selection.restore)?;
                let now = objects::PublicationTime::read(clock.as_ref())?;
                purge_expired(connection, &selection.target.bucket, now.micros)?;
                let transaction = connection.transaction()?;
                let (source, _) = restore_candidate(&transaction, &selection, now)?;
                backend.pin(&source.blob, &id)?;
                Ok(source.blob)
            })
            .await;
        let outcome = async {
            let source = pinned?;
            #[cfg(test)]
            super::fault::hit(&self.0.data_dir, super::fault::Step::BeforePublish)
                .map_err(object_unavailable)?;
            let backend = self.0.backend.clone();
            let published =
                crate::task::run_blocking("restore-publish", move || backend.publish_pinned(&id))
                    .await;
            #[cfg(test)]
            super::fault::hit(&self.0.data_dir, super::fault::Step::RestorePinned)
                .map_err(object_unavailable)?;
            let clock = self.0.clock.clone();
            self.run_upload(move |connection| {
                restore_in_transaction(connection, &request, source, id, published, clock.as_ref())
            })
            .await
        }
        .await;
        if outcome.is_err() {
            self.discard_staged(objects::StagedBlob::pin(
                &self.0.backend,
                &self.0.data_dir,
                id,
            ))
            .await;
        }
        self.reclaim_after_commit().await;
        outcome
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
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM objects JOIN pending_blob_deletions USING(blob_id)",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
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

    #[test]
    fn soft_delete_expiry_journals_exactly_the_deleted_rows() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE objects(bucket TEXT, name TEXT, generation INTEGER, blob_id TEXT PRIMARY KEY, soft_delete_time INTEGER, hard_delete_time INTEGER);
            CREATE TABLE pending_blob_deletions(blob_id TEXT PRIMARY KEY);
            WITH RECURSIVE entries(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM entries WHERE n < 201)
            INSERT INTO objects SELECT 'bucket', 'item', n, printf('%032x',n), 1, 10 FROM entries;
            CREATE TRIGGER change_expiry AFTER INSERT ON pending_blob_deletions BEGIN UPDATE objects SET hard_delete_time=20 WHERE blob_id=NEW.blob_id; END;").unwrap();
        purge_expired(&mut connection, "bucket", 10).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM objects JOIN pending_blob_deletions USING(blob_id)",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM objects", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            101
        );
        connection.execute_batch("CREATE TRIGGER reject_journal BEFORE INSERT ON pending_blob_deletions BEGIN SELECT RAISE(ABORT,'journal rejected'); END;").unwrap();
        assert!(purge_expired(&mut connection, "bucket", 10).is_err());
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM objects", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            101
        );
    }
}
