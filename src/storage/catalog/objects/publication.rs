use super::super::UnixMicros;
use super::*;
use crate::bucket::{ClassLocationConflict, Location};
use std::num::NonZeroI64;

/// The new version a publication transaction records: its blob, how its generation is
/// assigned, the publication time and the mode that decides whether it reports notifications.
#[derive(Clone, Copy)]
pub(in crate::storage::catalog) struct NewVersion<'a> {
    pub(in crate::storage::catalog) blob_id: BlobId,
    pub(in crate::storage::catalog) assignment: GenerationAssignment,
    pub(in crate::storage::catalog) micros: i64,
    pub(in crate::storage::catalog) notification_mode: NotificationMode,
    pub(in crate::storage::catalog) owner_profile: Option<&'a super::super::principals::Profile>,
    pub(in crate::storage::catalog) acl: super::super::acl::PublicationAcl<'a>,
}

pub(in crate::storage::catalog) struct PublicationDefaults {
    pub(in crate::storage::catalog) storage_class: StorageClass,
    pub(in crate::storage::catalog) event_based_hold: bool,
    pub(in crate::storage::catalog) time: std::time::SystemTime,
    pub(in crate::storage::catalog) owner: Option<crate::object::owner::ObjectOwner>,
}

/// Bucket settings shared by every check and mutation in one publication.
pub(in crate::storage::catalog) struct BucketPublication {
    default_class: StorageClass,
    autoclass: Option<crate::bucket::autoclass::Autoclass>,
    pub(in crate::storage::catalog) versioning: Versioning,
    pub(in crate::storage::catalog) default_event_hold: bool,
    retention_period: Option<u64>,
    /// How long a removed generation stays soft deleted; `None` removes it at once.
    soft_delete_seconds: Option<NonZeroI64>,
    /// With the project, what a requested class must fit.
    location: Location,
    project: String,
}

impl BucketPublication {
    pub(in crate::storage::catalog) fn load(
        connection: &Connection,
        bucket: &str,
    ) -> Result<Self, ObjectError> {
        sql::row(
            connection,
            "SELECT storage_class, autoclass, versioning_enabled, default_event_based_hold, json_extract(retention_policy, '$.retention_period'), COALESCE(json_extract(soft_delete_policy, '$.retention_duration_seconds'), 0), location, project FROM buckets WHERE name = ?1",
            [bucket],
            |row| {
                Ok(Self {
                    default_class: row.get(0)?,
                    autoclass: row
                        .get::<_, Option<sql::Json<_>>>(1)?
                        .map(|json| json.0),
                    versioning: if row.get(2)? {
                        Versioning::Enabled
                    } else {
                        Versioning::Disabled
                    },
                    default_event_hold: row.get(3)?,
                    retention_period: row.get(4)?,
                    soft_delete_seconds: NonZeroI64::new(row.get(5)?)
                        .filter(|seconds| seconds.is_positive()),
                    location: row.get(6)?,
                    project: row.get(7)?,
                })
            },
        )
        .optional()?
        .ok_or(ObjectError::BucketNotFound)
    }

    /// Refuses a requested class the bucket's location cannot hold.
    pub(in crate::storage::catalog) fn check_class(
        &self,
        requested: Option<StorageClass>,
    ) -> Result<(), ObjectError> {
        if let Some(class) = requested {
            ClassLocationConflict::check(class, self.location, &self.project)?;
        }
        Ok(())
    }

    /// The class a new generation receives. A requested class must fit the bucket's location
    /// even where Autoclass replaces it, an order no recording covers.
    pub(in crate::storage::catalog) fn storage_class(
        &self,
        requested: Option<StorageClass>,
    ) -> Result<StorageClass, ObjectError> {
        self.check_class(requested)?;
        Ok(crate::bucket::autoclass::object_storage_class(
            self.autoclass.as_ref(),
            requested,
            self.default_class,
        ))
    }

    pub(in crate::storage::catalog) fn retention_expiration(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<std::time::SystemTime>, ObjectError> {
        policy_expiration(record, self.retention_period)
    }

    fn soft_delete_deadline(&self, micros: i64) -> Result<Option<UnixMicros>, ObjectError> {
        self.soft_delete_seconds
            .map(|seconds| {
                seconds
                    .get()
                    .checked_mul(1_000_000)
                    .and_then(|duration| micros.checked_add(duration))
                    .map(UnixMicros)
                    .ok_or_else(|| {
                        ObjectError::Corrupt(
                            "Stored soft delete duration overflows the deletion deadline".into(),
                        )
                    })
            })
            .transpose()
    }
}

/// The clock reading shared by every check and timestamp in one publication transaction.
#[derive(Clone, Copy, Debug)]
pub(in crate::storage::catalog) struct PublicationTime {
    pub(in crate::storage::catalog) time: std::time::SystemTime,
    pub(in crate::storage::catalog) micros: i64,
}

impl PublicationTime {
    pub(in crate::storage::catalog) fn read(clock: &dyn Clock) -> Result<Self, ObjectError> {
        let now = clock.now();
        now.duration_since(std::time::UNIX_EPOCH)
            .map_err(object_unavailable)?;
        let micros = unix_micros(now)?;
        Ok(Self {
            time: system_time_micros(micros)?,
            micros,
        })
    }
}

impl PublicationDefaults {
    pub(in crate::storage::catalog) fn record(
        self,
        target: ObjectTarget<i64>,
        content_type: String,
        size: u64,
        hashes: Hashes,
    ) -> ObjectRecord {
        let ObjectTarget {
            bucket,
            name,
            generation,
        } = target;
        ObjectRecord {
            bucket,
            name,
            generation,
            metageneration: 1,
            owner: self.owner,
            content_type,
            size,
            md5: hashes.md5,
            md5_availability: Default::default(),
            crc32c: hashes.crc32c,
            component_count: None,
            storage_class: self.storage_class,
            content_encoding: None,
            content_disposition: None,
            content_language: None,
            cache_control: None,
            metadata: Default::default(),
            time_created: self.time,
            time_storage_class_updated: None,
            updated: self.time,
            time_deleted: None,
            soft_delete_time: None,
            hard_delete_time: None,
            event_based_hold: if self.event_based_hold {
                Hold::Held
            } else {
                Hold::Unset
            },
            temporary_hold: Hold::Unset,
            event_based_hold_release_time: None,
            custom_time: None,
            retention_expiration_time: None,
            retention: None,
            customer_key_sha256: None,
        }
    }
}

pub(in crate::storage::catalog) fn retention_expiration(
    connection: &Connection,
    record: &ObjectRecord,
) -> Result<Option<std::time::SystemTime>, ObjectError> {
    let period: Option<u64> = sql::scalar(
        connection,
        "SELECT json_extract(retention_policy, '$.retention_period') FROM buckets WHERE name = ?1",
        [&record.bucket],
    )?;
    policy_expiration(record, period)
}

/// The bucket policy deadline of `record`, as the catalog derives `retention_expiration_time`
/// when it reads the row: none without a policy or while the period has not started.
fn policy_expiration(
    record: &ObjectRecord,
    period: Option<u64>,
) -> Result<Option<std::time::SystemTime>, ObjectError> {
    period
        .zip(record.retention_period_start())
        .map(|(period, start)| expiration_after(start, period))
        .transpose()
}

pub(in crate::storage::catalog) fn expiration_after(
    created: std::time::SystemTime,
    period: u64,
) -> Result<std::time::SystemTime, ObjectError> {
    created
        .checked_add(std::time::Duration::from_secs(period))
        .ok_or_else(|| {
            ObjectError::Corrupt("Stored retention period overflows the expiration time".into())
        })
}

/// ACL metadata rejections keep their client-input status across publication paths.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "every other catalog error keeps the shared object mapping"
)]
pub(in crate::storage::catalog) fn acl_update_error(error: CatalogError) -> ObjectError {
    match error {
        CatalogError::InvalidMetadata(reason) => ObjectError::InvalidFixedMetadata(reason),
        CatalogError::Metadata(error) => ObjectError::Metadata(error),
        CatalogError::UnsupportedMetadata(reason) => ObjectError::UnsupportedMetadata(reason),
        error => error.into(),
    }
}

/// Whether the caller holds `storage.objects.delete`, which replacing a live object requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::storage::catalog) enum DeletePermission<'a> {
    Granted,
    /// Borrows what the refusal names, so that the many publications that replace nothing
    /// build no denial.
    Denied {
        identity: Option<&'a crate::auth::Identity>,
        bucket: &'a str,
    },
}

impl DeletePermission<'_> {
    /// The refusal of a publication that would replace a live object, if the caller may not
    /// delete it.
    pub(in crate::storage::catalog) fn overwrite_refusal(self) -> Option<ObjectError> {
        match self {
            Self::Granted => None,
            Self::Denied { identity, bucket } => {
                Some(ObjectError::OverwriteForbidden(crate::auth::Denied::new(
                    identity,
                    crate::auth::OBJECTS_DELETE,
                    crate::auth::denied::Resource::in_bucket(crate::auth::OBJECTS_DELETE, bucket),
                )))
            }
        }
    }
}

/// Whether the bucket keeps replaced live generations as noncurrent versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Versioning {
    Enabled,
    Disabled,
}

/// What happens to a live generation that is replaced or deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::storage::catalog) enum Retirement {
    /// The generation stays readable as a noncurrent version.
    Noncurrent,
    /// The generation is soft deleted when the bucket retains deletions, or removed.
    Removed,
}

/// What retiring a live generation leaves for after its transaction commits.
#[must_use = "a soft delete requires Catalog::schedule_expiry after its commit"]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::storage::catalog) enum Retired {
    /// Nothing: there was no live generation, or it stays readable as noncurrent.
    Nothing,
    /// The generation now has a hard delete time the maintenance scheduler does not know.
    SoftDeleted,
    /// The generation is gone and its blob waits in the deletion journal.
    Journaled,
}

/// Which failure an unmet not-match precondition reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::storage::catalog) enum RequestKind {
    /// Reads answer not modified.
    Read,
    /// The object the request changes, deletes or replaces: a not-match naming its current
    /// value reports [`ObjectError::WriteNotModified`](crate::object::ObjectError).
    Mutation,
    /// The source a copy or rewrite resolves before it writes: a not-match naming its current
    /// value reports [`ObjectError::SourceNotModified`](crate::object::ObjectError).
    CopySource,
    /// Any other object a write names, such as a move source, a compose component or a copy
    /// source checked again when its rewrite publishes: a failed precondition.
    Write,
}

#[derive(Clone, Copy)]
pub(in crate::storage::catalog) enum GenerationAssignment {
    Automatic,
    Imported(super::super::admin::Generation),
}

/// The highest object generation the catalog has handed out, imported generations included.
pub(in crate::storage::catalog) fn last_generation(
    connection: &Connection,
) -> rusqlite::Result<i64> {
    sql::scalar(
        connection,
        "SELECT last_generation FROM object_generation WHERE singleton = 1",
        [],
    )
}

/// The generation an automatically numbered version written at `micros` receives: its write time,
/// unless an earlier version already took that number or a later one.
pub(in crate::storage::catalog) fn next_generation(
    last: i64,
    micros: i64,
) -> Result<i64, ObjectError> {
    let after_last = last
        .checked_add(1)
        .ok_or_else(|| object_unavailable("generation exhausted"))?;
    Ok(micros.max(after_last))
}

/// A version a publication wrote, what it did to the version it replaced, and what the
/// preparation produced alongside the record.
pub(in crate::storage::catalog) struct PublishedVersion<T> {
    pub(in crate::storage::catalog) record: ObjectRecord,
    pub(in crate::storage::catalog) retired: Retired,
    pub(in crate::storage::catalog) prepared: T,
}

/// [`publish_in_transaction`] for a publication whose preparation yields only the record.
pub(in crate::storage::catalog) fn publish_version(
    transaction: &Transaction<'_>,
    previous: Option<&StoredObject>,
    bucket: &BucketPublication,
    version: NewVersion<'_>,
    prepare: impl FnOnce(i64) -> Result<ObjectRecord, ObjectError>,
) -> Result<PublishedVersion<()>, ObjectError> {
    publish_in_transaction(transaction, previous, bucket, version, |generation| {
        prepare(generation).map(|record| (record, ()))
    })
}

/// The caller owns protocol-specific completion and commits before reclaiming retired bytes.
/// Preparation may also yield a value derived from the record before publication changes it,
/// such as a completion response rendered inside the transaction.
pub(in crate::storage::catalog) fn publish_in_transaction<T>(
    transaction: &Transaction<'_>,
    previous: Option<&StoredObject>,
    bucket: &BucketPublication,
    version: NewVersion<'_>,
    prepare: impl FnOnce(i64) -> Result<(ObjectRecord, T), ObjectError>,
) -> Result<PublishedVersion<T>, ObjectError> {
    let NewVersion {
        blob_id,
        assignment,
        micros,
        notification_mode,
        owner_profile,
        acl,
    } = version;
    let last = last_generation(transaction)?;
    let generation = match assignment {
        GenerationAssignment::Automatic => next_generation(last, micros)?,
        GenerationAssignment::Imported(generation) => generation.get(),
    };
    let (mut record, prepared) = prepare(generation)?;
    // Reused records must not carry a previous generation's class-change history into publication,
    // nor a hold release: a new generation's retention period starts at its creation.
    record.time_storage_class_updated = None;
    record.event_based_hold_release_time = None;
    if let Some(owner) = &mut record.owner {
        super::super::object_owner::canonicalize(owner, owner_profile);
    }
    record.retention_expiration_time = bucket.retention_expiration(&record)?;
    let now = system_time_micros(micros)?;
    // A retained live version may become noncurrent, but it cannot be replaced outright.
    if let Some(StoredObject {
        record: previous, ..
    }) = previous
    {
        super::check_replaceable(previous, bucket.versioning, now)?;
    }
    retention::validate_publication(transaction, &record, now)?;
    validate_timestamps(&record)?;
    let retention = retention::columns(record.retention.as_ref())?;
    let event = HoldColumns::from(record.event_based_hold);
    let temporary = HoldColumns::from(record.temporary_hold);
    if matches!(assignment, GenerationAssignment::Imported(_)) {
        let exists: bool = sql::scalar(
            transaction,
            "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3)",
            params![record.bucket, record.name, generation],
        )?;
        if exists {
            return Err(ObjectError::PreconditionFailed);
        }
    }
    let metadata = serde_json::to_string(&record.metadata).map_err(object_unavailable)?;
    let owner = record
        .owner
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(object_unavailable)?;
    let retirement = match bucket.versioning {
        Versioning::Enabled => Retirement::Noncurrent,
        Versioning::Disabled => Retirement::Removed,
    };
    let retired = retire_previous(
        transaction,
        previous,
        retirement,
        bucket,
        RetirementContext {
            micros,
            replacement: Some(generation),
            notification_mode,
        },
    )?;
    sql::exec(
        transaction,
        "UPDATE object_generation SET last_generation = ?1 WHERE singleton = 1",
        [last.max(generation)],
    )?;
    let size = i64::try_from(record.size).map_err(object_unavailable)?;
    let time_created = unix_micros(record.time_created)?;
    let updated = unix_micros(record.updated)?;
    let class_updated = record
        .time_storage_class_updated
        .map(unix_micros)
        .transpose()?;
    transaction.prepare_cached(
        "INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,content_encoding,content_disposition,content_language,cache_control,metadata,event_based_hold,component_count,temporary_hold,custom_time,retention_mode,retain_until_time,customer_key_sha256,md5_available,owner,retain_until_sub_micro_nanos,temporary_hold_released,event_based_hold_released,time_storage_class_updated) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?19,?11,?12,?13,?14,?15,?16,?17,?18,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30)",
    ).and_then(|mut statement| statement.execute(params![record.bucket, record.name, generation, record.metageneration,
            record.content_type, size,
            &record.md5[..], record.crc32c, record.storage_class, time_created, blob_id,
            record.content_encoding, record.content_disposition, record.content_language,
            record.cache_control, metadata, event.held, record.component_count,
            updated, temporary.held, record.custom_time,
            retention.mode, retention.until,
            record.customer_key_sha256.as_ref().map(|sha256| &sha256[..]),
            record.md5_availability == crate::object::Md5Availability::Available, owner,
            retention.sub_micro, temporary.released, event.released, class_updated]))?;
    super::super::acl::publish_object_acl(transaction, &record, acl)?;
    if notification_mode == NotificationMode::Emit {
        super::super::notification_outbox::published(transaction, &record, previous, now)?;
    }
    Ok(PublishedVersion {
        record,
        retired,
        prepared,
    })
}

/// Refuses a record whose times, or the retention deadline computed from them, a response could
/// not render.
pub(super) fn validate_timestamps(record: &ObjectRecord) -> Result<(), ObjectError> {
    let outside = || {
        ObjectError::InvalidFixedMetadata("Object timestamps and computed retention expiration must remain within the supported RFC3339 UTC range".into())
    };
    if let Some(retention) = record.retention
        && !retention
            .retain_until
            .micros_and_remainder()
            .is_some_and(|(micros, _)| crate::clock::rfc3339_micros(i128::from(micros)))
    {
        return Err(outside());
    }
    for time in [
        Some(record.time_created),
        Some(record.updated),
        record.time_storage_class_updated,
        record.retention_expiration_time,
    ]
    .into_iter()
    .flatten()
    {
        let micros = unix_micros(time)?;
        if !crate::clock::rfc3339_micros(i128::from(micros)) {
            return Err(outside());
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(super) struct RetirementContext {
    pub(super) micros: i64,
    pub(super) replacement: Option<i64>,
    pub(super) notification_mode: NotificationMode,
}

pub(super) fn retire_previous(
    connection: &Connection,
    previous: Option<&StoredObject>,
    retirement: Retirement,
    bucket: &BucketPublication,
    context: RetirementContext,
) -> Result<Retired, ObjectError> {
    let RetirementContext {
        micros,
        replacement,
        notification_mode,
    } = context;
    let Some(StoredObject { record, blob: id }) = previous else {
        return Ok(Retired::Nothing);
    };
    if notification_mode == NotificationMode::Emit {
        super::super::notification_outbox::enqueue(
            connection,
            crate::notification::Message {
                event: match retirement {
                    Retirement::Noncurrent => crate::notification::Event::Archive,
                    Retirement::Removed => crate::notification::Event::Delete,
                },
                object: record,
                time: system_time_micros(micros)?,
                replacement,
            },
        )?;
    }
    match retirement {
        Retirement::Noncurrent => {
            sql::exec(
                connection,
                "UPDATE objects SET time_deleted = ?2 WHERE blob_id = ?1",
                params![id, micros],
            )?;
            Ok(Retired::Nothing)
        }
        Retirement::Removed => {
            if let Some(deadline) = bucket.soft_delete_deadline(micros)? {
                sql::exec(
                    connection,
                    "UPDATE objects SET time_deleted = COALESCE(time_deleted, ?2), soft_delete_time = ?2, hard_delete_time = ?3 WHERE blob_id = ?1",
                    params![id, micros, deadline],
                )?;
                Ok(Retired::SoftDeleted)
            } else {
                sql::exec(connection, "DELETE FROM objects WHERE blob_id = ?1", [id])?;
                journal_new_deletion(connection, id)?;
                Ok(Retired::Journaled)
            }
        }
    }
}
