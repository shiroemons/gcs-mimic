use super::{
    Catalog,
    objects::{
        GenerationAssignment, NewVersion, authorized_bucket, check_publication, lookup,
        publish_in_transaction, selected,
    },
    unix_micros,
};
use crate::storage::catalog::sql;
use crate::{
    bucket::BucketAccess,
    object::{ObjectConditions, ObjectError, ObjectRecord},
};
use rusqlite::params;

/// The same-bucket rename: the live source becomes the destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MoveNames {
    pub(crate) bucket: String,
    pub(crate) source: String,
    pub(crate) destination: String,
}

/// Preconditions on the live source and the destination version it replaces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MoveConditions {
    pub(crate) source: ObjectConditions,
    pub(crate) destination: ObjectConditions,
}

/// The three authorities a move checks in its single transaction.
#[derive(Clone, Copy)]
pub(crate) struct MoveAccess<'a> {
    pub(crate) source_read: &'a BucketAccess,
    pub(crate) source_delete: &'a BucketAccess,
    pub(crate) destination_create: &'a BucketAccess,
}

impl Catalog {
    /// Both names and all three authorization decisions are checked in one transaction.
    pub(crate) async fn move_object(
        &self,
        names: MoveNames,
        conditions: MoveConditions,
        access: MoveAccess<'_>,
    ) -> Result<ObjectRecord, ObjectError> {
        let MoveNames {
            bucket,
            source,
            destination,
        } = names;
        let source_read = access.source_read.clone();
        let source_delete = access.source_delete.clone();
        let destination_create = access.destination_create.clone();
        let clock = self.0.clock.clone();
        let record = self
            .run(move |connection| {
                let transaction = sql::immediate(connection)?;
                let super::objects::StoredObject {
                    mut record,
                    blob: blob_id,
                } = selected(
                    &transaction,
                    &bucket,
                    &source,
                    None,
                    conditions.source,
                    super::objects::RequestKind::Write,
                    &source_delete,
                )?;
                super::objects::authorize_generation(
                    &transaction,
                    &record.bucket,
                    &record.name,
                    record.generation,
                    &source_read,
                )?;
                let source_acl = super::acl::effective_object_entries(&transaction, &record)?;
                let (_, delete_permission) =
                    authorized_bucket(&transaction, &bucket, &destination_create)?;
                let previous = lookup(&transaction, &bucket, &destination)?;
                check_publication(
                    previous.as_ref(),
                    conditions.destination,
                    delete_permission,
                    clock.now(),
                )?;
                record.check_retention(clock.now())?;
                if source == destination {
                    return Err(ObjectError::InvalidMetadata);
                }
                let (versioning, _) = Self::bucket_settings_in_transaction(&transaction, &bucket)?;
                let now = clock.now();
                let micros = unix_micros(now)?;
                let origin = super::change_journal::origin(&destination_create);
                super::notification_outbox::enqueue(
                    &transaction,
                    crate::notification::Message {
                        event: crate::notification::Event::Delete,
                        object: &record,
                        time: now,
                        replacement: None,
                    },
                )?;
                super::change_journal::object(
                    &transaction,
                    origin,
                    super::change_journal::ChangeKind::Delete,
                    &record,
                )?;
                // Remove only the live source generation; its blob becomes the destination's blob.
                sql::exec(
                    &transaction,
                    "DELETE FROM objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
                    params![bucket, source, record.generation],
                )?;
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
                        notification_mode: crate::notification::NotificationMode::for_access(
                            &destination_create,
                        ),
                        owner_profile: None,
                        acl: super::acl::PublicationAcl::Preserve(&source_acl),
                    },
                    |generation| {
                        record.generation = generation;
                        Ok((record, ()))
                    },
                )?;
                transaction.commit()?;
                Ok(record)
            })
            .await?;
        self.reclaim_after_commit().await;
        Ok(record)
    }
}
