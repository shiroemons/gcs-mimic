use super::{
    Catalog,
    objects::{
        GenerationAssignment, authorized_bucket, check_publication, lookup, object_unavailable,
        publish_in_transaction, selected,
    },
    unix_micros,
};
use crate::{
    bucket::BucketAccess,
    object::{ObjectConditions, ObjectError, ObjectRecord},
};
use rusqlite::{TransactionBehavior, params};

impl Catalog {
    #[allow(
        clippy::too_many_arguments,
        reason = "Both names and authorization decisions are checked in one transaction"
    )]
    pub(crate) async fn move_object_authorized(
        &self,
        bucket: &str,
        source: &str,
        destination: &str,
        source_conditions: ObjectConditions,
        conditions: ObjectConditions,
        source_read: &BucketAccess,
        source_delete: &BucketAccess,
        destination_create: &BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        let bucket = bucket.to_owned();
        let source = source.to_owned();
        let destination = destination.to_owned();
        let source_read = source_read.clone();
        let source_delete = source_delete.clone();
        let destination_create = destination_create.clone();
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
                    false,
                    &source_delete,
                )?;
                let (_, allow_overwrite) =
                    authorized_bucket(&transaction, &bucket, &destination_create)?;
                let previous = lookup(&transaction, &bucket, &destination)?;
                check_publication(previous.as_ref(), conditions, allow_overwrite)?;
                if record.has_hold() {
                    return Err(ObjectError::ActiveHold);
                }
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
                    .execute(
                        "DELETE FROM objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
                        params![bucket, source, record.generation],
                    )
                    .map_err(object_unavailable)?;
                record.name = destination;
                record.metageneration = 1;
                record.time_created = now;
                record.updated = now;
                record.time_deleted = None;
                let (record, _, ()) = publish_in_transaction(
                    &transaction,
                    micros,
                    GenerationAssignment::Automatic,
                    previous.as_ref(),
                    versioning,
                    &blob_id,
                    origin,
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
