use super::{
    Catalog,
    objects::{
        GenerationAssignment, NewVersion, authorized_bucket, check_publication, lookup,
        object_unavailable, publish_in_transaction, selected,
    },
    unix_micros,
};
use crate::{
    bucket::BucketAccess,
    object::{ObjectConditions, ObjectError, ObjectRecord},
};
use rusqlite::{TransactionBehavior, params};

/// The three authorities a move checks in its single transaction.
#[derive(Clone, Copy)]
pub(crate) struct MoveAccess<'a> {
    pub(crate) source_read: &'a BucketAccess,
    pub(crate) source_delete: &'a BucketAccess,
    pub(crate) destination_create: &'a BucketAccess,
}

impl Catalog {
    /// Both names and all three authorization decisions are checked in one transaction.
    pub(crate) async fn move_object_authorized(
        &self,
        bucket: &str,
        source: &str,
        destination: &str,
        source_conditions: ObjectConditions,
        conditions: ObjectConditions,
        access: MoveAccess<'_>,
    ) -> Result<ObjectRecord, ObjectError> {
        let bucket = bucket.to_owned();
        let source = source.to_owned();
        let destination = destination.to_owned();
        let source_read = access.source_read.clone();
        let source_delete = access.source_delete.clone();
        let destination_create = access.destination_create.clone();
        let clock = self.0.clock.clone();
        let record = self
            .run_object(move |connection| {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(object_unavailable)?;
                authorized_bucket(&transaction, &bucket, &source_read)?;
                let (mut record, blob_id) = selected(
                    &transaction,
                    &bucket,
                    &source,
                    None,
                    source_conditions,
                    super::objects::RequestKind::Write,
                    &source_delete,
                )?;
                let (_, allow_overwrite) =
                    authorized_bucket(&transaction, &bucket, &destination_create)?;
                let previous = lookup(&transaction, &bucket, &destination)?;
                check_publication(previous.as_ref(), conditions, allow_overwrite, clock.now())?;
                record.check_retention(clock.now())?;
                if source == destination {
                    return Err(ObjectError::InvalidMetadata);
                }
                let (versioning, _) = Self::bucket_settings_in_transaction(&transaction, &bucket)
                    .map_err(object_unavailable)?;
                let now = clock.now();
                let micros = unix_micros(now).map_err(object_unavailable)?;
                let origin = super::change_journal::origin(&destination_create);
                super::change_journal::object(
                    &transaction,
                    origin,
                    super::change_journal::ChangeKind::Delete,
                    &record,
                )
                .map_err(object_unavailable)?;
                // Remove only the live source generation; its blob becomes the destination's blob.
                transaction
                    .prepare_cached(
                        "DELETE FROM objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
                    )
                    .and_then(|mut statement| {
                        statement.execute(params![bucket, source, record.generation])
                    })
                    .map_err(object_unavailable)?;
                record.name = destination;
                record.metageneration = 1;
                record.time_created = now;
                record.updated = now;
                record.time_deleted = None;
                let (record, _, ()) = publish_in_transaction(
                    &transaction,
                    previous.as_ref(),
                    versioning,
                    NewVersion {
                        blob_id: &blob_id,
                        assignment: GenerationAssignment::Automatic,
                        micros,
                        origin,
                    },
                    |generation| {
                        record.generation = generation;
                        Ok((record, ()))
                    },
                )?;
                transaction.commit().map_err(object_unavailable)?;
                Ok(record)
            })
            .await?;
        self.reclaim_after_commit().await;
        Ok(record)
    }
}
