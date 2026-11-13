use super::{
    Activity, Catalog, UploadGuard, Work,
    read_cache::{self, CachedDownload, CachedObject},
    system_time_micros, unavailable, unix_micros,
};
use crate::storage::{BlobId, catalog::sql};
use crate::{
    bucket::{BucketAccess, CatalogError, StorageClass},
    clock::Clock,
    hashing::{Hasher, Hashes},
    notification::NotificationMode,
    object::{
        ExpectedHashes, HashAlgorithm, HashMismatch, Hold, ObjectConditions, ObjectError,
        ObjectMetadataUpdate, ObjectRecord, ObjectTarget, UploadChunk,
    },
};
use bytes::Bytes;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, types::Type};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};
use tokio::{
    io::AsyncWriteExt,
    sync::{OwnedSemaphorePermit, mpsc, oneshot},
};

#[cfg(test)]
use super::fault::{self, Step};

mod list_query;
#[cfg(test)]
mod reclaim_tests;
pub(super) mod retention;
pub(crate) use list_query::{FoldersAsPrefixes, ListQueryError};

use super::object_hold::columns as hold_columns;

const CHUNK_SIZE: usize = 64 * 1024;
// A literal macro lets per-request queries build static SQL with concat!, which
// the connection's prepared statement cache reuses without formatting.
macro_rules! object_columns {
    () => {
        "bucket, name, generation, metageneration, content_type, size,
    md5, crc32c, storage_class, time_created, updated, blob_id,
    content_encoding, content_disposition, content_language, cache_control, metadata,
    time_deleted, event_based_hold, component_count, temporary_hold, custom_time,
    time_created + (SELECT json_extract(retention_policy, '$.retention_period') * 1000000 FROM buckets WHERE buckets.name = objects.bucket), soft_delete_time, hard_delete_time,
    retention_mode, retain_until_time, customer_key_sha256, md5_available, owner,
    retain_until_sub_micro_nanos, temporary_hold_released, event_based_hold_released,
    time_storage_class_updated"
    };
}
const SELECT_LIVE_OBJECT: &str = concat!(
    "SELECT ",
    object_columns!(),
    " FROM objects WHERE bucket = ?1 AND name = ?2 AND time_deleted IS NULL"
);
const SELECT_OBJECT_GENERATION: &str = concat!(
    "SELECT ",
    object_columns!(),
    " FROM objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3 AND soft_delete_time IS NULL"
);
pub(super) const SELECT_SOFT_DELETED_GENERATION: &str = concat!(
    "SELECT ",
    object_columns!(),
    " FROM objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3 AND soft_delete_time IS NOT NULL AND hard_delete_time > ?4"
);

mod durability;
mod list;
mod publication;
mod reader;
mod reclaim;
mod recovery;
mod schema;
#[cfg(test)]
mod tests;
mod upload;

#[cfg(unix)]
pub(super) use durability::sync_dir;
pub(super) use durability::{
    StagedFile, blob_shard, commit_publication, create_upload_stage, publish_blob, unlink,
};
pub(crate) use list::ObjectListCursor;
pub(crate) use list::{ObjectListPage, ObjectListQuery};
pub(super) use list::{SeekStart, list_page, prefix_end};
pub(crate) use publication::Versioning;
pub(super) use publication::{
    BucketPublication, DeletePermission, GenerationAssignment, NewVersion, PublicationDefaults,
    PublicationTime, RequestKind, Retired, Retirement, acl_update_error, publish_in_transaction,
    retention_expiration,
};
use publication::{RetirementContext, retire_previous};
pub(crate) use reader::ObjectReader;
pub(super) use reader::ReaderData;
use reclaim::{JOURNAL_CONFLICT, live_journal_error};
pub(super) use reclaim::{Journaled, StagedBlob, blob_referenced, reclaim};
pub(super) use recovery::recover;
pub(super) use schema::{
    GENERATION_SCHEMA, METADATA_SCHEMA, SCHEMA, VERSIONING_SCHEMA, migrate, migrate_metadata,
    migrate_versioning,
};
pub(super) use upload::{
    CommitQueue, OwnerAssignment, PublishIdentity, UploadSink, UploadedBody, write_stream,
};

/// A streamed upload's destination and everything its publication checks and records,
/// apart from the body and the admission permit.
pub(crate) struct ObjectUpload<'a> {
    pub(crate) bucket: &'a str,
    pub(crate) name: &'a str,
    pub(crate) content_type: String,
    pub(crate) expected: ExpectedHashes,
    pub(crate) conditions: ObjectConditions,
    pub(crate) max_bytes: u64,
    pub(crate) metadata: ObjectMetadataUpdate,
    /// Rechecked at publication: the bucket may have been recreated under another project
    /// while the body streamed, and overwriting needs storage.objects.delete there.
    pub(crate) access: BucketAccess,
}

pub(super) struct StoredObject {
    pub(super) record: ObjectRecord,
    pub(super) blob: BlobId,
}

impl Catalog {
    pub(crate) async fn get_object(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        let shareable = read_cache::is_shareable(&target, access);
        if shareable && let Some(hit) = self.cached_object(&target).await {
            check_conditions(Some(&hit.record), conditions, RequestKind::Read)?;
            return Ok(hit.record.clone());
        }
        let revision = self.0.read_cache.revision();
        let inner = self.0.clone();
        let ObjectTarget {
            bucket,
            name,
            generation,
        } = target;
        let access = access.clone();
        self.run_read_on(move |connection, path| {
            let snapshot = sql::read_snapshot(connection)?;
            let StoredObject { record, .. } = selected(
                &snapshot,
                &bucket,
                &name,
                generation,
                conditions,
                RequestKind::Read,
                &access,
            )?;
            if shareable && path.stores_answers() {
                inner.read_cache.store_object(
                    revision,
                    CachedObject {
                        record: record.clone(),
                        download: None,
                    },
                );
            }
            Ok(record)
        })
        .await
    }

    /// The bucket's project is verified in the same call that resolves the generation and
    /// opens its blob, so the returned reader never belongs to an unauthorized bucket.
    /// Callers without a customer-supplied key cannot open objects encrypted with one.
    #[cfg(test)]
    pub(crate) async fn open_object(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(ObjectRecord, ObjectReader), ObjectError> {
        self.open_object_keyed(
            target,
            conditions,
            access,
            None,
            crate::object::ReadAhead::None,
        )
        .await
    }

    /// Applies the public update contract after rechecking the caller's authorization.
    pub(crate) async fn update_object_metadata(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        update: ObjectMetadataUpdate,
        validate: impl FnOnce(&ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        access: &BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        let ObjectTarget {
            bucket,
            name,
            generation,
        } = target;
        let clock = self.0.clock.clone();
        let access = access.clone();
        let principals = self.0.principals.get().cloned();
        self.run(move |connection| {
            let transaction = sql::immediate(connection)?;
            let StoredObject { mut record, .. } = selected(
                &transaction,
                &bucket,
                &name,
                generation,
                conditions,
                RequestKind::Mutation,
                &access,
            )?;
            validate(&record)?;
            if let Some(entries) = &update.acl {
                super::acl::replace_resource_acl(
                    &transaction,
                    super::acl::ResolvedAclTarget::Object {
                        bucket: &bucket,
                        name: &name,
                        generation: record.generation,
                    },
                    entries,
                    &access,
                    principals.as_ref(),
                ).map_err(acl_update_error)?;
            }
            let previous_retention = record.retention;
            let previous_event_hold = record.event_based_hold;
            let override_unlocked = update.override_unlocked_retention;
            update.reject_past_retention(clock.now())?;
            update.apply_to_existing(&mut record)?;
            if record.event_based_hold.is_held() && !previous_event_hold.is_held() && record.retention.is_some() {
                return Err(ObjectError::InvalidFixedMetadata(
                    "Retention and event based holds cannot be configured together.".into(),
                ));
            }
            if record.retention != previous_retention {
                retention::validate_publication(&transaction, &record, clock.now())?;
                crate::object::retention::check_transition(
                    previous_retention.as_ref(),
                    record.retention.as_ref(),
                    override_unlocked,
                )?;
            }
            let (retention_mode, retain_until, retain_sub_micro) =
                retention::columns(record.retention.as_ref())?;
            let (event_hold, event_released) = hold_columns(record.event_based_hold);
            let (temporary_hold, temporary_released) = hold_columns(record.temporary_hold);
            record.metageneration = record
                .metageneration
                .checked_add(1)
                .ok_or_else(|| object_unavailable("metageneration exhausted"))?;
            let micros = unix_micros(clock.now())?;
            record.updated = system_time_micros(micros)?;
            let metadata = serde_json::to_string(&record.metadata).map_err(object_unavailable)?;
            sql::exec(&transaction,
                    "UPDATE objects SET metageneration = ?3, updated = ?4, content_type = ?5,
                    content_encoding = ?6, content_disposition = ?7, content_language = ?8,
                    cache_control = ?9, metadata = ?10, event_based_hold = ?11,
                    temporary_hold = ?13, custom_time = ?14, retention_mode = ?15, retain_until_time = ?16,
                    retain_until_sub_micro_nanos = ?17,
                    temporary_hold_released = ?18, event_based_hold_released = ?19
                    WHERE bucket = ?1 AND name = ?2 AND generation = ?12", params![
                        bucket,
                        name,
                        record.metageneration,
                        micros,
                        record.content_type,
                        record.content_encoding,
                        record.content_disposition,
                        record.content_language,
                        record.cache_control,
                        metadata,
                        event_hold,
                        record.generation,
                        temporary_hold,
                        record.custom_time,
                        retention_mode,
                        retain_until,
                        retain_sub_micro,
                        temporary_released,
                        event_released
                    ])?;
            super::change_journal::object(
                &transaction,
                super::change_journal::origin(&access),
                super::change_journal::ChangeKind::Metadata,
                &record,
            )?;
            transaction.commit()?;
            Ok(record)
        })
        .await
    }

    pub(crate) async fn delete_object(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        let access = access.clone();
        let clock = self.0.clock.clone();
        let retired = self
            .run(move |connection| {
                delete_selected(
                    connection,
                    clock.as_ref(),
                    &target,
                    conditions,
                    &access,
                    None,
                )
            })
            .await?;
        self.schedule_expiry(retired);
        self.reclaim_after_commit().await;
        Ok(())
    }
}

/// Shared selection preserves protocol conditions and access.
pub(super) fn open_selected(
    connection: &Connection,
    catalog: &Catalog,
    target: &ObjectTarget,
    conditions: ObjectConditions,
    access: &BucketAccess,
) -> Result<(ObjectRecord, ObjectReader), ObjectError> {
    let snapshot = sql::read_snapshot(connection)?;
    open_selected_in(&snapshot, catalog, target, conditions, access)
}

pub(super) fn open_selected_in(
    connection: &Connection,
    catalog: &Catalog,
    target: &ObjectTarget,
    conditions: ObjectConditions,
    access: &BucketAccess,
) -> Result<(ObjectRecord, ObjectReader), ObjectError> {
    let ObjectTarget {
        bucket,
        name,
        generation,
    } = target;
    let StoredObject { record, blob: id } = selected(
        connection,
        bucket,
        name,
        *generation,
        conditions,
        RequestKind::Read,
        access,
    )?;
    // On the serialized path this cannot race a delete or overwrite. On a pooled reader the blob
    // may already be gone, and the failed open sends the read to the serialized path.
    let file = catalog.0.backend.open(&id)?;
    let remaining = record.size;
    let publicly_readable = publicly_readable(connection, bucket, name, record.generation)?;
    Ok((
        record,
        ObjectReader {
            file,
            remaining,
            position: 0,
            head: None,
            publicly_readable,
        },
    ))
}

/// Whether an anonymous caller could read this generation, which decides the response's caching
/// headers. The read cache keeps the answer, so a repeated download does not decode the policy
/// and the ACL again.
fn publicly_readable(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: i64,
) -> Result<bool, ObjectError> {
    Ok(super::policies::read_policy(connection, bucket)?.allows(
        None,
        crate::auth::OBJECTS_GET,
        super::policies::policy_public_access(connection, bucket)?,
    ) || super::acl::object_granted(
        connection,
        bucket,
        name,
        generation,
        &BucketAccess::Policy {
            identity: None,
            permission: crate::auth::Permission::ObjectsGet,
            project: None,
        },
    )?)
}

/// Shared selection preserves protocol conditions and access.
pub(super) fn delete_selected(
    connection: &mut Connection,
    clock: &dyn crate::clock::Clock,
    target: &ObjectTarget,
    conditions: ObjectConditions,
    access: &BucketAccess,
    sync_binding: Option<&super::sync_state::SyncBinding>,
) -> Result<Retired, ObjectError> {
    let bucket = &target.bucket;
    authorized_bucket(connection, bucket, access)?;
    super::soft_delete::purge_expired(connection, bucket, clock.now().try_into()?)?;
    let transaction = sql::immediate(connection)?;
    let deleted = delete_in_transaction(
        &transaction,
        target,
        DeleteContext {
            now: clock.now(),
            conditions,
            access,
            sync_binding,
        },
    )?;
    transaction.commit()?;
    Ok(deleted.retired)
}

pub(super) struct Deleted {
    pub(super) retired: Retired,
    pub(super) retirement: Retirement,
}

#[derive(Clone, Copy)]
pub(super) struct DeleteContext<'a> {
    pub(super) now: std::time::SystemTime,
    pub(super) conditions: ObjectConditions,
    pub(super) access: &'a BucketAccess,
    pub(super) sync_binding: Option<&'a super::sync_state::SyncBinding>,
}

/// The caller owns the transaction, including any extra lifecycle protection checks.
pub(super) fn delete_in_transaction(
    transaction: &Transaction<'_>,
    target: &ObjectTarget,
    context: DeleteContext<'_>,
) -> Result<Deleted, ObjectError> {
    let ObjectTarget {
        bucket,
        name,
        generation,
    } = target;
    let DeleteContext {
        now,
        conditions,
        access,
        sync_binding,
    } = context;
    let StoredObject { record, blob: id } = selected(
        transaction,
        bucket,
        name,
        *generation,
        conditions,
        RequestKind::Mutation,
        access,
    )?;
    let publication = BucketPublication::load(transaction, bucket)?;
    // Deleting a live version in a versioned bucket only makes it noncurrent.
    let retirement = if publication.versioning == Versioning::Enabled && generation.is_none() {
        Retirement::Noncurrent
    } else {
        Retirement::Removed
    };
    if retirement == Retirement::Noncurrent {
        record.check_policy_and_holds(now)?;
    } else {
        record.check_retention(now)?;
    }
    if let Some(binding) = sync_binding {
        super::sync_state::delete_in_transaction(transaction, binding, &record)?;
    }
    super::change_journal::object(
        transaction,
        super::change_journal::origin(access),
        super::change_journal::ChangeKind::Delete,
        &record,
    )?;
    let retired = retire_previous(
        transaction,
        Some(&StoredObject { record, blob: id }),
        retirement,
        &publication,
        RetirementContext {
            micros: unix_micros(now)?,
            origin: super::change_journal::origin(access),
            replacement: None,
            notification_mode: NotificationMode::for_access(access),
        },
    )?;
    Ok(Deleted {
        retired,
        retirement,
    })
}

pub(super) use crate::storage::failure::object_unavailable;

/// Resolves the bucket and re-validates `access` against the owning project observed on the
/// same connection, before any object state is read or changed.
///
/// Returns the storage class and whether the caller may delete objects, which replacing a
/// live object also requires. A missing bucket is reported before authorization, matching the
/// adapter's missing-bucket rule.
pub(super) fn authorized_bucket<'a>(
    connection: &Connection,
    bucket: &'a str,
    access: &'a BucketAccess,
) -> Result<(StorageClass, DeletePermission<'a>), ObjectError> {
    let (storage_class, project): (StorageClass, String) = sql::row(
        connection,
        "SELECT storage_class, project FROM buckets WHERE name = ?1",
        [bucket],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or(ObjectError::BucketNotFound)?;
    let delete_permission =
        super::check_access(connection, bucket, &project, access).map_err(ObjectError::from)?;
    Ok((storage_class, delete_permission))
}

pub(super) fn lookup(
    connection: &Connection,
    bucket: &str,
    name: &str,
) -> Result<Option<StoredObject>, ObjectError> {
    sql::row(
        connection,
        SELECT_LIVE_OBJECT,
        params![bucket, name],
        read_object,
    )
    .optional()
    .map_err(object_unavailable)
}

pub(super) fn selected(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: Option<i64>,
    conditions: ObjectConditions,
    request: RequestKind,
    access: &BucketAccess,
) -> Result<StoredObject, ObjectError> {
    let object_acl = object_acl_authority(connection, bucket, access)?;
    let record = if let Some(generation) = generation {
        sql::row(
            connection,
            SELECT_OBJECT_GENERATION,
            params![bucket, name, generation],
            read_object,
        )
        .optional()?
    } else {
        lookup(connection, bucket, name)?
    };
    if let Some(check) = object_acl {
        let granted = record
            .as_ref()
            .map(|stored| {
                super::acl::object_granted(
                    connection,
                    bucket,
                    name,
                    stored.record.generation,
                    check.access,
                )
            })
            .transpose()?
            .unwrap_or(false);
        if !granted {
            return Err(check.refusal());
        }
    }
    let stored = record.ok_or(ObjectError::ObjectNotFound)?;
    check_conditions(Some(&stored.record), conditions, request)?;
    Ok(stored)
}

/// A caller the bucket refused an object permission, whose object ACL may still grant it.
pub(super) struct ObjectAclCheck<'a> {
    pub(super) access: &'a BucketAccess,
    /// The bucket's refusal, which stands when the object ACL grants nothing either.
    denied: Box<crate::auth::Denied>,
}

impl ObjectAclCheck<'_> {
    pub(super) fn refusal(self) -> ObjectError {
        ObjectError::Denied(self.denied)
    }
}

pub(super) fn object_acl_authority<'a>(
    connection: &Connection,
    bucket: &str,
    access: &'a BucketAccess,
) -> Result<Option<ObjectAclCheck<'a>>, ObjectError> {
    match authorized_bucket(connection, bucket, access) {
        Ok(_) => Ok(None),
        Err(ObjectError::Denied(denied))
            if matches!(
                access,
                BucketAccess::Policy { permission, .. } if crate::acl::object_permission(*permission)
            ) =>
        {
            Ok(Some(ObjectAclCheck { access, denied }))
        }
        Err(error) => Err(error),
    }
}

/// Rechecks a pinned source without selecting a newer generation.
pub(super) fn authorize_generation(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: i64,
    access: &BucketAccess,
) -> Result<(), ObjectError> {
    if let Some(check) = object_acl_authority(connection, bucket, access)?
        && !super::acl::object_granted(connection, bucket, name, generation, check.access)?
    {
        return Err(check.refusal());
    }
    Ok(())
}

/// Preconditions are evaluated first: a failing precondition means nothing would be replaced.
pub(super) fn check_publication(
    previous: Option<&StoredObject>,
    conditions: ObjectConditions,
    delete_permission: DeletePermission<'_>,
    now: std::time::SystemTime,
) -> Result<(), ObjectError> {
    check_conditions(
        previous.map(|stored| &stored.record),
        conditions,
        RequestKind::Mutation,
    )?;
    if previous.is_some()
        && let Some(refusal) = delete_permission.overwrite_refusal()
    {
        return Err(refusal);
    }
    // Object retention depends on versioning; publish_in_transaction checks it.
    if let Some(stored) = previous {
        stored.record.check_policy_and_holds(now)?;
    }
    Ok(())
}

pub(super) fn skip_same(
    mode: super::admin::PublishMode,
    previous: Option<&StoredObject>,
    size: u64,
    md5: [u8; 16],
    crc32c: u32,
) -> bool {
    matches!(
        mode,
        super::admin::PublishMode::SkipIfSameHash
            | super::admin::PublishMode::InheritAndSkipIfSameHash
    ) && previous.is_some_and(|StoredObject { record, .. }| {
        record.size == size && record.md5 == md5 && record.crc32c == crc32c
    })
}

pub(super) fn inherit_metadata(
    mode: super::admin::PublishMode,
    previous: Option<&StoredObject>,
    record: &mut ObjectRecord,
) {
    if matches!(
        mode,
        super::admin::PublishMode::InheritFromReplaced
            | super::admin::PublishMode::InheritAndSkipIfSameHash
    ) && let Some(StoredObject {
        record: previous, ..
    }) = previous
    {
        record.content_type.clone_from(&previous.content_type);
        record
            .content_encoding
            .clone_from(&previous.content_encoding);
        record
            .content_disposition
            .clone_from(&previous.content_disposition);
        record
            .content_language
            .clone_from(&previous.content_language);
        record.cache_control.clone_from(&previous.cache_control);
        record.metadata.clone_from(&previous.metadata);
    }
}

pub(super) fn check_conditions(
    record: Option<&ObjectRecord>,
    conditions: ObjectConditions,
    request: RequestKind,
) -> Result<(), ObjectError> {
    let generation = record.map_or(0, |record| record.generation);
    let metageneration = record.map(|record| record.metageneration);
    if conditions
        .if_generation_match
        .is_some_and(|value| value != generation)
        || conditions
            .if_metageneration_match
            .is_some_and(|value| Some(value) != metageneration)
    {
        return Err(ObjectError::PreconditionFailed);
    }
    if conditions
        .if_generation_not_match
        .is_some_and(|value| record.is_none() || value == generation)
        || conditions
            .if_metageneration_not_match
            .is_some_and(|value| metageneration.is_none() || Some(value) == metageneration)
    {
        return Err(match (request, record) {
            (RequestKind::Read, _) => ObjectError::NotModified,
            // A not-match on an absent object holds no current value to name.
            (RequestKind::Mutation, Some(_)) => ObjectError::WriteNotModified,
            (RequestKind::Mutation, None) | (RequestKind::Write, _) => {
                ObjectError::PreconditionFailed
            }
        });
    }
    Ok(())
}

pub(super) fn read_object(row: &Row<'_>) -> rusqlite::Result<StoredObject> {
    let md5: Vec<u8> = row.get(6)?;
    let md5 = md5
        .try_into()
        .map_err(|_| sql::conversion_error(6, Type::Blob, "invalid MD5 length"))?;
    Ok(StoredObject {
        record: ObjectRecord {
            bucket: row.get(0)?,
            name: row.get(1)?,
            generation: row.get(2)?,
            metageneration: row.get(3)?,
            owner: super::object_owner::read(row, 29)?,
            content_type: row.get(4)?,
            content_encoding: row.get(12)?,
            content_disposition: row.get(13)?,
            content_language: row.get(14)?,
            cache_control: row.get(15)?,
            metadata: serde_json::from_str(&row.get::<_, String>(16)?)
                .map_err(|error| sql::conversion_error(16, Type::Text, error))?,
            size: row.get(5)?,
            md5,
            md5_availability: super::md5_availability::read(row, 28)?,
            crc32c: row.get(7)?,
            component_count: row.get(19)?,
            storage_class: row.get(8)?,
            time_created: system_time_micros(row.get(9)?)?,
            time_storage_class_updated: row
                .get::<_, Option<i64>>(33)?
                .map(system_time_micros)
                .transpose()?,
            updated: system_time_micros(row.get(10)?)?,
            time_deleted: row
                .get::<_, Option<i64>>(17)?
                .map(system_time_micros)
                .transpose()?,
            event_based_hold: super::object_hold::read(row, 18, 32)?,
            temporary_hold: super::object_hold::read(row, 20, 31)?,
            custom_time: row.get(21)?,
            retention_expiration_time: row
                .get::<_, Option<i64>>(22)?
                .map(system_time_micros)
                .transpose()?,
            soft_delete_time: row
                .get::<_, Option<i64>>(23)?
                .map(system_time_micros)
                .transpose()?,
            hard_delete_time: row
                .get::<_, Option<i64>>(24)?
                .map(system_time_micros)
                .transpose()?,
            retention: retention::read(row, 25, 30)?,
            customer_key_sha256: super::customer_keys::decode(row.get(27)?)?,
        },
        blob: row.get(11)?,
    })
}
