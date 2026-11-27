use super::{
    Catalog,
    objects::{
        GenerationAssignment, NewVersion, PublicationTime, PublishedVersion,
        check_upload_destination, publish_version, selected,
    },
};
use crate::storage::catalog::sql;
use crate::{
    bucket::BucketAccess,
    object::{Metageneration, ObjectConditions, ObjectError, ObjectRecord},
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
        let clock = self.0.policy.clock.clone();
        let (record, retired) = self
            .run(move |connection| {
                let transaction = sql::immediate(connection)?;
                let now = PublicationTime::read(clock.as_ref())?;
                let super::objects::StoredObject {
                    mut record,
                    blob: blob_id,
                } = selected(
                    &transaction,
                    crate::object::ObjectKey::from_parts_unchecked(&bucket, &source),
                    None,
                    conditions.source,
                    super::objects::RequestKind::Write,
                    &source_delete,
                )?;
                super::objects::authorize_generation(
                    &transaction,
                    record.key(),
                    record.generation.get(),
                    &source_read,
                )?;
                let source_acl = super::acl::effective_object_entries(&transaction, &record)?;
                let previous = check_upload_destination(
                    &transaction,
                    crate::object::ObjectKey::from_parts_unchecked(&bucket, &destination),
                    conditions.destination,
                    &destination_create,
                    now.time,
                )?;
                record.check_retention(now.time)?;
                if source == destination {
                    return Err(ObjectError::InvalidDerivedArgument(
                        "Source and destination object names must be different.".into(),
                    ));
                }
                let publication = super::objects::BucketPublication::load(&transaction, &bucket)?;
                super::notification_outbox::enqueue(
                    &transaction,
                    crate::notification::Message {
                        event: crate::notification::Event::Delete,
                        object: &record,
                        time: now.time,
                        replacement: None,
                    },
                )?;
                // Remove only the live source generation; its blob becomes the destination's blob.
                sql::exec(
                    &transaction,
                    "DELETE FROM objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
                    params![bucket, source, record.generation],
                )?;
                record.name = destination;
                record.metageneration = Metageneration::INITIAL;
                record.time_created = now.time;
                record.updated = now.time;
                record.time_deleted = None;
                let PublishedVersion {
                    record, retired, ..
                } = publish_version(
                    &transaction,
                    previous.as_ref(),
                    &publication,
                    NewVersion {
                        blob_id,
                        assignment: GenerationAssignment::Automatic,
                        micros: now.micros,
                        notification_mode: crate::notification::NotificationMode::for_access(
                            &destination_create,
                        ),
                        owner_profile: None,
                        acl: super::acl::PublicationAcl::Preserve(&source_acl),
                    },
                    |generation| {
                        record.generation = generation;
                        Ok(record)
                    },
                )?;
                transaction.commit()?;
                Ok((record, retired))
            })
            .await?;
        self.schedule_expiry(retired);
        self.reclaim_if(retired.into()).await;
        Ok(record)
    }
}
