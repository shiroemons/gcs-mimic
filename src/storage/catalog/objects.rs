use super::{
    Activity, Catalog, UploadGuard, Work, micros_column,
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
        ChecksumConflict, ExpectedHashes, Hold, ObjectConditions, ObjectError,
        ObjectMetadataUpdate, ObjectRecord, ObjectTarget, UploadChunk,
    },
};
use bytes::Bytes;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};
use tokio::sync::{OwnedSemaphorePermit, mpsc, oneshot};

#[cfg(test)]
use super::fault::{self, Step};

mod list_query;
#[cfg(test)]
mod reclaim_tests;
pub(super) mod retention;
pub(crate) use list_query::{FoldersAsPrefixes, ListQueryError};

use super::object_hold::HoldColumns;

const CHUNK_SIZE: usize = 64 * 1024;
// When the bucket retention period of an object row starts, as
// `ObjectRecord::retention_period_start` computes it apart from the held case, which callers
// decide. `$table` qualifies the columns where a join makes them ambiguous.
macro_rules! retention_start {
    ($table:literal) => {
        concat!(
            "MAX(",
            $table,
            "time_created, COALESCE(",
            $table,
            "event_based_hold_release_time, ",
            $table,
            "time_created))"
        )
    };
}
pub(super) use retention_start;
// A literal macro lets per-request queries build static SQL with concat!, which
// the connection's prepared statement cache reuses without formatting.
macro_rules! object_columns {
    () => {
        concat!(
            "bucket, name, generation, metageneration, content_type, size,
    md5, crc32c, storage_class, time_created, updated, blob_id,
    content_encoding, content_disposition, content_language, cache_control, metadata,
    time_deleted, event_based_hold, component_count, temporary_hold, custom_time,
    CASE WHEN event_based_hold = 0 THEN ",
            retention_start!(""),
            " + (SELECT json_extract(retention_policy, '$.retention_period') * 1000000 FROM buckets WHERE buckets.name = objects.bucket) END AS retention_expiration_time, soft_delete_time, hard_delete_time,
    retention_mode, retain_until_time, customer_key_sha256, md5_available, owner,
    retain_until_sub_micro_nanos, temporary_hold_released, event_based_hold_released,
    time_storage_class_updated, event_based_hold_release_time"
        )
    };
}
// The object point read and every listing seek decode this row, so it is read by position;
// `object_columns_match_their_positions` keeps the declaration in step with the list above.
sql::column_positions!(enum ObjectColumn {
    Bucket => "bucket",
    Name => "name",
    Generation => "generation",
    Metageneration => "metageneration",
    ContentType => "content_type",
    Size => "size",
    Md5 => "md5",
    Crc32c => "crc32c",
    StorageClass => "storage_class",
    TimeCreated => "time_created",
    Updated => "updated",
    BlobId => "blob_id",
    ContentEncoding => "content_encoding",
    ContentDisposition => "content_disposition",
    ContentLanguage => "content_language",
    CacheControl => "cache_control",
    Metadata => "metadata",
    TimeDeleted => "time_deleted",
    EventBasedHold => "event_based_hold",
    ComponentCount => "component_count",
    TemporaryHold => "temporary_hold",
    CustomTime => "custom_time",
    RetentionExpiration => "retention_expiration_time",
    SoftDeleteTime => "soft_delete_time",
    HardDeleteTime => "hard_delete_time",
    RetentionMode => "retention_mode",
    RetainUntilTime => "retain_until_time",
    CustomerKeySha256 => "customer_key_sha256",
    Md5Available => "md5_available",
    Owner => "owner",
    RetainUntilSubMicroNanos => "retain_until_sub_micro_nanos",
    TemporaryHoldReleased => "temporary_hold_released",
    EventBasedHoldReleased => "event_based_hold_released",
    TimeStorageClassUpdated => "time_storage_class_updated",
    EventBasedHoldReleaseTime => "event_based_hold_release_time",
});
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

#[cfg(test)]
pub(super) use durability::WRITE_BATCH_BYTES;
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
    PublicationTime, PublishedVersion, RequestKind, Retired, Retirement, acl_update_error,
    last_generation, next_generation, publish_in_transaction, publish_version,
    retention_expiration,
};
use publication::{RetirementContext, retire_previous, validate_timestamps};
pub(super) use reader::ReaderData;
pub(crate) use reader::{FrameLimit, ObjectFrames, ObjectReader};
use reclaim::{JOURNAL_CONFLICT, live_journal_error};
pub(super) use reclaim::{
    Journaled, StagedBlob, blob_referenced, journal_deletion, journal_new_deletion, reclaim,
};
pub(super) use recovery::recover;
pub(super) use schema::{GENERATION_SCHEMA, METADATA_SCHEMA, SCHEMA, VERSIONING_SCHEMA, migrate};
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
            let requested = match &update.acl {
                crate::acl::AclRequest::Omitted => None,
                crate::acl::AclRequest::Entries(entries) => {
                    Some(std::borrow::Cow::Borrowed(entries.as_slice()))
                }
                crate::acl::AclRequest::Predefined(acl) => Some(std::borrow::Cow::Owned(
                    super::acl::predefined_object_entries(&transaction, &bucket, *acl)?,
                )),
            };
            if let Some(entries) = requested {
                super::acl::replace_resource_acl(
                    &transaction,
                    super::acl::ResolvedAclTarget::Object {
                        bucket: &bucket,
                        name: &name,
                        generation: record.generation,
                    },
                    &entries,
                    &access,
                    principals.as_ref(),
                )
                .map_err(acl_update_error)?;
            }
            store_metadata_update(&transaction, &mut record, update, clock.as_ref())?;
            super::notification_outbox::metadata_updated(
                &transaction,
                NotificationMode::for_access(&access),
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
        let (purged, result) = self
            .run(move |connection| {
                delete_selected(connection, clock.as_ref(), &target, conditions, &access)
            })
            .await?;
        self.finish_delete(purged, result).await
    }

    pub(super) async fn finish_delete(
        &self,
        purged: Journaled,
        result: Result<Retired, ObjectError>,
    ) -> Result<(), ObjectError> {
        let retired = result.as_ref().copied().unwrap_or(Retired::Nothing);
        self.schedule_expiry(retired);
        self.reclaim_if(purged | retired.into()).await;
        result.map(drop)
    }
}

/// Applies `update` to the selected generation at the clock's time and stores the result in the
/// caller's transaction.
fn store_metadata_update(
    transaction: &Transaction<'_>,
    record: &mut ObjectRecord,
    update: ObjectMetadataUpdate,
    clock: &dyn Clock,
) -> Result<(), ObjectError> {
    let previous_retention = record.retention;
    let previous_event_hold = record.event_based_hold;
    let override_unlocked = update.override_unlocked_retention;
    update.reject_past_retention(clock.now())?;
    let micros = unix_micros(clock.now())?;
    let now = system_time_micros(micros)?;
    update.apply_to_existing(record, now)?;
    if record.event_based_hold.is_held()
        && !previous_event_hold.is_held()
        && record.retention.is_some()
    {
        return Err(ObjectError::InvalidFixedMetadata(
            "Retention and event based holds cannot be configured together.".into(),
        ));
    }
    if record.retention != previous_retention {
        retention::validate_publication(transaction, record, clock.now())?;
        crate::object::retention::check_transition(
            previous_retention.as_ref(),
            record.retention.as_ref(),
            override_unlocked,
        )?;
    }
    // The deadline read with the row is stale once the event-based hold is set, which suspends
    // the bucket retention period, or released, which restarts it.
    record.retention_expiration_time = retention_expiration(transaction, record)?;
    validate_timestamps(record)?;
    let retention = retention::columns(record.retention.as_ref())?;
    let event = HoldColumns::from(record.event_based_hold);
    let temporary = HoldColumns::from(record.temporary_hold);
    let event_released = record
        .event_based_hold_release_time
        .map(unix_micros)
        .transpose()?;
    record.metageneration = record
        .metageneration
        .checked_add(1)
        .ok_or_else(|| object_unavailable("metageneration exhausted"))?;
    record.updated = now;
    let metadata = serde_json::to_string(&record.metadata).map_err(object_unavailable)?;
    sql::exec(
        transaction,
        "UPDATE objects SET metageneration = ?3, updated = ?4, content_type = ?5,
        content_encoding = ?6, content_disposition = ?7, content_language = ?8,
        cache_control = ?9, metadata = ?10, event_based_hold = ?11,
        temporary_hold = ?13, custom_time = ?14, retention_mode = ?15, retain_until_time = ?16,
        retain_until_sub_micro_nanos = ?17,
        temporary_hold_released = ?18, event_based_hold_released = ?19,
        event_based_hold_release_time = ?20
        WHERE bucket = ?1 AND name = ?2 AND generation = ?12",
        params![
            record.bucket,
            record.name,
            record.metageneration,
            micros,
            record.content_type,
            record.content_encoding,
            record.content_disposition,
            record.content_language,
            record.cache_control,
            metadata,
            event.held,
            record.generation,
            temporary.held,
            record.custom_time,
            retention.mode,
            retention.until,
            retention.sub_micro,
            temporary.released,
            event.released,
            event_released
        ],
    )?;
    Ok(())
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
) -> Result<(Journaled, Result<Retired, ObjectError>), ObjectError> {
    let bucket = &target.bucket;
    authorized_bucket(connection, bucket, access)?;
    super::soft_delete::after_purge(connection, bucket, clock.now().try_into()?, |connection| {
        let transaction = sql::immediate(connection)?;
        let deleted = delete_in_transaction(
            &transaction,
            target,
            DeleteContext {
                now: clock.now(),
                conditions,
                access,
            },
        )?;
        transaction.commit()?;
        Ok(deleted.retired)
    })
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
    let retired = retire_previous(
        transaction,
        Some(&StoredObject { record, blob: id }),
        retirement,
        &publication,
        RetirementContext {
            micros: unix_micros(now)?,
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
    // Object retention depends on versioning; publish_in_transaction checks it with
    // check_replaceable.
    if let Some(stored) = previous {
        stored.record.check_policy_and_holds(now)?;
    }
    Ok(())
}

/// Whether a new live generation may replace `previous` at `now`: no hold or bucket retention
/// policy protects it, and where no versioning keeps the replaced generation as noncurrent, its
/// own retention period has passed. Publication refuses a replacement with this, and a seed
/// replay decides with it before publishing, so that what the replay predicts is the refusal
/// publication meets.
pub(super) fn check_replaceable(
    previous: &ObjectRecord,
    versioning: Versioning,
    now: std::time::SystemTime,
) -> Result<(), ObjectError> {
    previous.check_policy_and_holds(now)?;
    match versioning {
        Versioning::Disabled => previous.check_object_retention(now),
        Versioning::Enabled => Ok(()),
    }
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
            (RequestKind::CopySource, Some(_)) => ObjectError::SourceNotModified,
            (RequestKind::Mutation | RequestKind::CopySource, None) | (RequestKind::Write, _) => {
                ObjectError::PreconditionFailed
            }
        });
    }
    Ok(())
}

/// Decodes a row selected with `object_columns!`.
pub(super) fn read_object(row: &Row<'_>) -> rusqlite::Result<StoredObject> {
    use ObjectColumn as C;
    let md5 = sql::fixed_blob(row, C::Md5.index(), "invalid MD5 length")?;
    Ok(StoredObject {
        record: ObjectRecord {
            bucket: row.get(C::Bucket.index())?,
            name: row.get(C::Name.index())?,
            generation: row.get(C::Generation.index())?,
            metageneration: row.get(C::Metageneration.index())?,
            owner: super::object_owner::read(row, C::Owner.index())?,
            content_type: row.get(C::ContentType.index())?,
            content_encoding: row.get(C::ContentEncoding.index())?,
            content_disposition: row.get(C::ContentDisposition.index())?,
            content_language: row.get(C::ContentLanguage.index())?,
            cache_control: row.get(C::CacheControl.index())?,
            metadata: row.get::<_, sql::Json<_>>(C::Metadata.index())?.0,
            size: row.get(C::Size.index())?,
            md5,
            md5_availability: super::md5_availability::read(row, C::Md5Available.index())?,
            crc32c: row.get(C::Crc32c.index())?,
            component_count: row.get(C::ComponentCount.index())?,
            storage_class: row.get(C::StorageClass.index())?,
            time_created: system_time_micros(row.get(C::TimeCreated.index())?)?,
            time_storage_class_updated: micros_column(row, C::TimeStorageClassUpdated.index())?,
            updated: system_time_micros(row.get(C::Updated.index())?)?,
            time_deleted: micros_column(row, C::TimeDeleted.index())?,
            event_based_hold: super::object_hold::read(
                row,
                C::EventBasedHold.index(),
                C::EventBasedHoldReleased.index(),
            )?,
            temporary_hold: super::object_hold::read(
                row,
                C::TemporaryHold.index(),
                C::TemporaryHoldReleased.index(),
            )?,
            event_based_hold_release_time: micros_column(
                row,
                C::EventBasedHoldReleaseTime.index(),
            )?,
            custom_time: row.get(C::CustomTime.index())?,
            retention_expiration_time: micros_column(row, C::RetentionExpiration.index())?,
            soft_delete_time: micros_column(row, C::SoftDeleteTime.index())?,
            hard_delete_time: micros_column(row, C::HardDeleteTime.index())?,
            retention: retention::read(
                row,
                retention::Columns {
                    mode: C::RetentionMode.index(),
                    until: C::RetainUntilTime.index(),
                    sub_micro: C::RetainUntilSubMicroNanos.index(),
                },
            )?,
            customer_key_sha256: super::customer_keys::read(row, C::CustomerKeySha256.index())?,
        },
        blob: row.get(C::BlobId.index())?,
    })
}
