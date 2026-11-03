use super::{Catalog, UploadGuard, system_time_micros, unavailable, unix_micros};
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
use bytes::{Bytes, BytesMut};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, types::Type};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{OwnedSemaphorePermit, mpsc, oneshot},
};

#[cfg(test)]
use super::fault::{self, Step};

#[cfg(test)]
mod reclaim_tests;
pub(super) mod retention;

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
    retain_until_sub_micro_nanos, temporary_hold_released, event_based_hold_released"
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

/// The new version a publication transaction records: its blob, how its generation is
/// assigned, the publication time and the origin the change journal reports.
#[derive(Clone, Copy)]
pub(super) struct NewVersion<'a> {
    pub(super) blob_id: BlobId,
    pub(super) assignment: GenerationAssignment,
    pub(super) micros: i64,
    pub(super) origin: super::admin::Origin,
    pub(super) notification_mode: NotificationMode,
    pub(super) owner_profile: Option<&'a super::principals::Profile>,
    pub(super) acl: super::acl::PublicationAcl<'a>,
}

pub(super) struct PublicationDefaults {
    pub(super) storage_class: StorageClass,
    pub(super) event_based_hold: bool,
    pub(super) time: std::time::SystemTime,
    pub(super) owner: Option<crate::object::owner::ObjectOwner>,
}

/// Bucket settings shared by every check and mutation in one publication.
pub(super) struct BucketPublication {
    default_class: StorageClass,
    autoclass: Option<crate::bucket::autoclass::Autoclass>,
    pub(super) versioning: Versioning,
    pub(super) default_event_hold: bool,
    retention_period: Option<u64>,
    soft_delete_seconds: i64,
}

impl BucketPublication {
    pub(super) fn load(connection: &Connection, bucket: &str) -> Result<Self, ObjectError> {
        let (default_class, autoclass, versioning, default_event_hold, retention_period, soft_delete_seconds) = sql::row(
            connection,
            "SELECT storage_class, autoclass, versioning_enabled, default_event_based_hold, json_extract(retention_policy, '$.retention_period'), COALESCE(json_extract(soft_delete_policy, '$.retention_duration_seconds'), 0) FROM buckets WHERE name = ?1",
            [bucket],
            |row| Ok((row.get(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, bool>(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        ).optional()?.ok_or(ObjectError::BucketNotFound)?;
        let autoclass = autoclass
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|error| {
                ObjectError::Corrupt(crate::storage::failure::BackendCause::with_source(
                    format!("bucket autoclass: {error}"),
                    error,
                ))
            })?;
        Ok(Self {
            default_class,
            autoclass,
            versioning: if versioning {
                Versioning::Enabled
            } else {
                Versioning::Disabled
            },
            default_event_hold,
            retention_period,
            soft_delete_seconds,
        })
    }

    pub(super) fn storage_class(&self, requested: Option<StorageClass>) -> StorageClass {
        crate::bucket::autoclass::object_storage_class(
            self.autoclass.as_ref(),
            requested,
            self.default_class,
        )
    }

    pub(super) fn retention_expiration(
        &self,
        created: std::time::SystemTime,
    ) -> Result<Option<std::time::SystemTime>, ObjectError> {
        self.retention_period
            .map(|period| expiration_after(created, period))
            .transpose()
    }

    fn soft_delete_deadline(&self, micros: i64) -> Result<Option<i64>, ObjectError> {
        if self.soft_delete_seconds <= 0 {
            return Ok(None);
        }
        self.soft_delete_seconds
            .checked_mul(1_000_000)
            .and_then(|duration| micros.checked_add(duration))
            .map(Some)
            .ok_or(ObjectError::InvalidMetadata)
    }
}

/// The clock reading shared by every check and timestamp in one publication transaction.
#[derive(Clone, Copy, Debug)]
pub(super) struct PublicationTime {
    pub(super) time: std::time::SystemTime,
    pub(super) micros: i64,
}

impl PublicationTime {
    pub(super) fn read(clock: &dyn Clock) -> Result<Self, ObjectError> {
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

pub(super) struct StoredObject {
    pub(super) record: ObjectRecord,
    pub(super) blob: BlobId,
}

impl PublicationDefaults {
    pub(super) fn record(
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
            custom_time: None,
            retention_expiration_time: None,
            retention: None,
            customer_key_sha256: None,
        }
    }
}

pub(super) fn retention_expiration(
    connection: &Connection,
    record: &ObjectRecord,
) -> Result<Option<std::time::SystemTime>, ObjectError> {
    let period: Option<u64> = sql::scalar(
        connection,
        "SELECT json_extract(retention_policy, '$.retention_period') FROM buckets WHERE name = ?1",
        [&record.bucket],
    )?;
    period
        .map(|period| expiration_after(record.time_created, period))
        .transpose()
}

pub(super) fn expiration_after(
    created: std::time::SystemTime,
    period: u64,
) -> Result<std::time::SystemTime, ObjectError> {
    created
        .checked_add(std::time::Duration::from_secs(period))
        .ok_or(ObjectError::InvalidMetadata)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObjectListCursor {
    pub name: String,
    pub is_prefix: bool,
    pub generation: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectListQuery {
    pub bucket: String,
    pub prefix: String,
    pub delimiter: String,
    pub(crate) match_glob: Option<crate::match_glob::Glob>,
    pub start_offset: String,
    pub end_offset: String,
    pub include_trailing_delimiter: bool,
    pub versions: bool,
    pub soft_deleted: bool,
    pub(crate) cursor: Option<ObjectListCursor>,
}

#[derive(Debug)]
pub struct ObjectListPage {
    pub items: Vec<ObjectRecord>,
    pub prefixes: Vec<String>,
    pub(crate) next: Option<ObjectListCursor>,
    /// The listed bucket's metageneration, read by the same catalog job as the page so the two
    /// agree. Only an authorized listing fills it; other producers leave it empty.
    pub(crate) bucket_metageneration: Option<i64>,
}

pub(super) const SCHEMA: &str = "CREATE TABLE objects (
            bucket TEXT NOT NULL COLLATE BINARY,
            name TEXT NOT NULL COLLATE BINARY,
            generation INTEGER NOT NULL CHECK(generation > 0),
            metageneration INTEGER NOT NULL CHECK(metageneration > 0),
            content_type TEXT NOT NULL,
            size INTEGER NOT NULL CHECK(size >= 0),
            md5 BLOB NOT NULL CHECK(length(md5) = 16),
            crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
            storage_class TEXT NOT NULL,
            time_created INTEGER NOT NULL,
            updated INTEGER NOT NULL,
            blob_id TEXT NOT NULL UNIQUE,
            PRIMARY KEY(bucket, name)
         ) WITHOUT ROWID;
         CREATE TABLE pending_blob_deletions (blob_id TEXT PRIMARY KEY) WITHOUT ROWID;
         PRAGMA user_version = 2;";

pub(super) const GENERATION_SCHEMA: &str = "CREATE TABLE object_generation (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    last_generation INTEGER NOT NULL CHECK(last_generation >= 0)
) WITHOUT ROWID;";

pub(super) const METADATA_SCHEMA: &str = "ALTER TABLE objects ADD COLUMN content_encoding TEXT;
    ALTER TABLE objects ADD COLUMN content_disposition TEXT;
    ALTER TABLE objects ADD COLUMN content_language TEXT;
    ALTER TABLE objects ADD COLUMN cache_control TEXT;
    ALTER TABLE objects ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';
    PRAGMA user_version = 4;";

pub(super) fn migrate_metadata(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(METADATA_SCHEMA)?;
    transaction.commit().map_err(unavailable)
}

pub(super) const VERSIONING_SCHEMA: &str =
    "ALTER TABLE objects RENAME TO objects_before_versioning;
CREATE TABLE objects (
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY,
    generation INTEGER NOT NULL CHECK(generation > 0),
    metageneration INTEGER NOT NULL CHECK(metageneration > 0),
    content_type TEXT NOT NULL,
    size INTEGER NOT NULL CHECK(size >= 0),
    md5 BLOB NOT NULL CHECK(length(md5) = 16),
    crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
    storage_class TEXT NOT NULL,
    time_created INTEGER NOT NULL,
    updated INTEGER NOT NULL,
    blob_id TEXT NOT NULL UNIQUE,
    content_encoding TEXT,
    content_disposition TEXT,
    content_language TEXT,
    cache_control TEXT,
    metadata TEXT NOT NULL DEFAULT '{}',
    time_deleted INTEGER,
    event_based_hold INTEGER NOT NULL DEFAULT 0 CHECK(event_based_hold IN (0, 1)),
    PRIMARY KEY(bucket, name, generation)
) WITHOUT ROWID;
INSERT INTO objects (bucket, name, generation, metageneration, content_type, size,
    md5, crc32c, storage_class, time_created, updated, blob_id, content_encoding,
    content_disposition, content_language, cache_control, metadata, time_deleted,
    event_based_hold)
SELECT bucket, name, generation, metageneration, content_type, size, md5, crc32c,
    storage_class, time_created, updated, blob_id, content_encoding,
    content_disposition, content_language, cache_control, metadata, NULL, 0
FROM objects_before_versioning;
DROP TABLE objects_before_versioning;
CREATE UNIQUE INDEX objects_live ON objects(bucket, name) WHERE time_deleted IS NULL;
PRAGMA user_version = 9;";

pub(super) fn migrate_versioning(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(VERSIONING_SCHEMA)?;
    transaction.commit().map_err(unavailable)
}

#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration at open"
)]
pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(SCHEMA)?;
    transaction.execute_batch(GENERATION_SCHEMA)?;
    transaction.execute("INSERT INTO object_generation VALUES (1, 0)", [])?;
    transaction.pragma_update(None, "user_version", 3)?;
    transaction.commit().map_err(unavailable)
}

#[cfg(unix)]
pub(super) fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(test)]
    fault::record_catalog_thread_io(path);
    crate::fsutil::sync_dir(path)?;
    #[cfg(test)]
    fault::dir_synced(path);
    Ok(())
}

static SHARD_CREATION: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The shard directory that holds a blob; every blob path built by `blob_path` has one.
pub(super) fn blob_shard(path: &Path) -> io::Result<&Path> {
    path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "blob path has no shard directory",
        )
    })
}

pub(super) fn publish_blob(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(test)]
    fault::record_catalog_thread_io(source);
    let shard = blob_shard(destination)?;
    {
        // A concurrent publisher must not pass a creator whose parent fsync is unfinished.
        let creation = SHARD_CREATION.lock();
        // The lock guards no data, so a creator that unwound must not stop all
        // later publishing. It may have left a shard whose parent entry was never
        // synced, so the next publisher syncs the parent before clearing the poison.
        #[cfg(unix)]
        let unwound = creation.is_err();
        let _creation = creation.unwrap_or_else(std::sync::PoisonError::into_inner);
        #[cfg(unix)]
        let blobs = shard
            .parent()
            .ok_or_else(|| io::Error::other("blob shard has no parent directory"))?;
        match fs::create_dir(shard) {
            Ok(()) => {
                #[cfg(all(test, unix))]
                fault::dir_created(shard);
                #[cfg(unix)]
                if let Err(error) = sync_dir(blobs) {
                    if let Err(cleanup) = fs::remove_dir(shard) {
                        crate::diagnostics::emit(
                            crate::diagnostics::LogLevel::Warn,
                            "blob_shard_cleanup_deferred",
                            serde_json::json!({"message": cleanup.to_string()}),
                        );
                    }
                    return Err(error);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(shard)?.is_dir() {
                    return Err(io::Error::other("blob shard is not a directory"));
                }
                #[cfg(unix)]
                if unwound {
                    sync_dir(blobs)?;
                }
            }
            Err(error) => return Err(error),
        }
        SHARD_CREATION.clear_poison();
    }
    #[cfg(unix)]
    {
        crate::fsutil::rename_noreplace(source, destination)?;
        #[cfg(test)]
        fault::renamed(source, destination);
        sync_dir(shard)
    }
    #[cfg(windows)]
    {
        // Preserve extended paths and keep publication on the same volume. Windows
        // uses write-through publication, not Unix directory-fsync durability.
        let source = source.canonicalize()?;
        let file_name = destination.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "blob destination has no file name",
            )
        })?;
        let destination = shard.canonicalize()?.join(file_name);
        crate::fsutil::move_write_through(&source, &destination)
    }
}

/// Commits a publication transaction after its blob is durable in `blobs/`.
///
/// Every object publication commits here, so fault-injection tests can fail the commit or
/// report an error after it succeeded, the ambiguous outcome cleanup must survive.
pub(super) fn commit_publication(transaction: Transaction<'_>) -> Result<(), ObjectError> {
    #[cfg(test)]
    let data_dir = fault::database_dir(&transaction);
    #[cfg(test)]
    fault::hit(&data_dir, Step::BeforeCommit).map_err(object_unavailable)?;
    transaction.commit()?;
    #[cfg(test)]
    fault::hit(&data_dir, Step::AfterCommit).map_err(object_unavailable)?;
    Ok(())
}

impl Catalog {
    /// Moves a durable staged body into its blob shard on the blocking pool.
    pub(super) async fn publish_staged(
        &self,
        source: PathBuf,
        destination: PathBuf,
    ) -> Result<(), ObjectError> {
        #[cfg(test)]
        fault::hit(&self.0.data_dir, Step::BeforePublish).map_err(object_unavailable)?;
        crate::task::run_blocking("blob-publication", move || {
            publish_blob(&source, &destination)
        })
        .await?
        .map_err(object_unavailable)?;
        #[cfg(test)]
        fault::hit(&self.0.data_dir, Step::AfterPublish).map_err(object_unavailable)?;
        Ok(())
    }
}

pub(super) fn unlink(path: &Path) -> io::Result<()> {
    #[cfg(test)]
    fault::record_catalog_thread_io(path);
    #[cfg(all(test, unix))]
    let retained = fault::retain_before_unlink(path)?;
    // Windows may defer removal until the final reader closes. Recovery also
    // scans unreferenced blobs, so a deferred deletion cannot become live data.
    let result = match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    };
    #[cfg(all(test, unix))]
    if result.is_ok()
        && let Some(file) = retained
    {
        fault::unlinked(path, &file)?;
    }
    result
}

fn os_metadata(entry: &fs::DirEntry) -> bool {
    let name = entry.file_name();
    let name = name.to_string_lossy();
    name == ".DS_Store" || name.starts_with("._")
}

pub(super) fn blob_referenced(connection: &Connection, id: &BlobId) -> rusqlite::Result<bool> {
    sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)",
        [id],
    )
}

const JOURNAL_CONFLICT: &str = "SELECT pending.blob_id FROM pending_blob_deletions AS pending JOIN objects ON objects.blob_id = pending.blob_id ORDER BY pending.blob_id LIMIT 1";

fn live_journal_error(id: BlobId) -> ObjectError {
    object_unavailable(format!(
        "deletion journal blob {id}: deletion journal references a live blob; its file is kept"
    ))
}

pub(super) fn reclaim(
    connection: &Connection,
    data_dir: &Path,
    id: &BlobId,
) -> Result<(), ObjectError> {
    let referenced: bool = blob_referenced(connection, id)?;
    if referenced {
        return Err(ObjectError::Unavailable(
            "deletion journal references a live blob".into(),
        ));
    }
    let path = id.blob_path(data_dir);
    let shard = blob_shard(&path)?;
    #[cfg(test)]
    fault::hit(data_dir, Step::ReclaimUnlink).map_err(object_unavailable)?;
    unlink(&path)?;
    #[cfg(unix)]
    if shard.exists() {
        #[cfg(test)]
        fault::hit(data_dir, Step::ReclaimSync).map_err(object_unavailable)?;
        sync_dir(shard)?;
    }
    // Windows has no directory fsync; the shard is still resolved so both platforms reject
    // the same malformed blob paths.
    #[cfg(not(unix))]
    let _ = shard;
    #[cfg(test)]
    fault::hit(data_dir, Step::ReclaimForget).map_err(object_unavailable)?;
    sql::exec(
        connection,
        "DELETE FROM pending_blob_deletions WHERE blob_id = ?1",
        [id],
    )?;
    Ok(())
}

pub(super) fn recover(
    connection: &Connection,
    data_dir: &Path,
    now: i64,
) -> Result<(), CatalogError> {
    // The phases run in this order: staging and derived state first, then the deletion
    // journal, so the reference and orphan scans only see committed, durable state.
    let recover = || -> Result<(), ObjectError> {
        fs::create_dir_all(data_dir.join("staging"))?;
        fs::create_dir_all(data_dir.join("blobs"))?;
        #[cfg(unix)]
        {
            sync_dir(data_dir)?;
            // A pre-crash shard creator may have stopped before making its parent durable.
            sync_dir(&data_dir.join("blobs"))?;
        }
        super::resumable_recovery::recover(connection, data_dir)?;
        super::derived::recover(connection, data_dir, now)?;
        remove_unretained_staging(connection, data_dir)?;
        drain_deletion_journal(connection, data_dir)?;
        verify_referenced_blobs(connection, data_dir)?;
        remove_orphan_blobs(connection, data_dir)
    };
    recover().map_err(|error| match error {
        ObjectError::Unavailable(reason) => CatalogError::Unavailable(reason),
        error => unavailable(error),
    })
}

/// Removes staging entries no active upload or derived transfer still owns.
fn remove_unretained_staging(connection: &Connection, data_dir: &Path) -> Result<(), ObjectError> {
    for entry in fs::read_dir(data_dir.join("staging"))? {
        let entry = entry?;
        if os_metadata(&entry) {
            continue;
        }
        let retained: bool = sql::scalar(
            connection,
            "SELECT EXISTS(SELECT 1 FROM resumable_sessions WHERE blob_id = ?1 AND state = 'ACTIVE') OR EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1) OR EXISTS(SELECT 1 FROM derived_inputs WHERE blob_id = ?1) OR EXISTS(SELECT 1 FROM xml_multipart_parts WHERE blob_id = ?1)",
            [entry.file_name().to_string_lossy().as_ref()],
        )?;
        if retained {
            continue;
        }
        #[cfg(test)]
        fault::hit(data_dir, Step::RecoveryStagingUnlink).map_err(|error| {
            crate::storage::failure::object_context(
                format_args!("staging entry {}", entry.path().display()),
                error,
            )
        })?;
        unlink(&entry.path()).map_err(|error| {
            crate::storage::failure::object_context(
                format_args!("staging entry {}", entry.path().display()),
                error,
            )
        })?;
    }
    #[cfg(unix)]
    sync_dir(&data_dir.join("staging"))?;
    Ok(())
}

/// Finishes deletions committed before a crash.
fn drain_deletion_journal(connection: &Connection, data_dir: &Path) -> Result<(), ObjectError> {
    // Keyset pages bound memory independently of the number of objects or pending deletions.
    loop {
        let ids = connection
            .prepare_cached(
                "SELECT blob_id FROM pending_blob_deletions AS pending WHERE NOT EXISTS (SELECT 1 FROM objects WHERE objects.blob_id = pending.blob_id) ORDER BY blob_id LIMIT 128",
            )?
            .query_map([], |r| r.get::<_, BlobId>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if ids.is_empty() {
            break;
        }
        for id in ids {
            reclaim(connection, data_dir, &id).map_err(|error| {
                crate::storage::failure::object_context(
                    format_args!("deletion journal blob {id}"),
                    error,
                )
            })?;
        }
    }
    sql::optional_scalar::<BlobId>(connection, JOURNAL_CONFLICT, [])?
        .map_or(Ok(()), |id| Err(live_journal_error(id)))
}

/// Every catalog row must name a regular blob file of the recorded size.
fn verify_referenced_blobs(connection: &Connection, data_dir: &Path) -> Result<(), ObjectError> {
    let mut after = String::new();
    loop {
        let rows = connection
            .prepare_cached(
                "SELECT blob_id, size FROM objects WHERE blob_id > ?1 ORDER BY blob_id LIMIT 128",
            )?
            .query_map([&after], |r| {
                Ok((r.get::<_, BlobId>(0)?, r.get::<_, u64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.is_empty() {
            return Ok(());
        }
        for (id, size) in rows {
            let path = id.blob_path(data_dir);
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                crate::storage::failure::object_context(format_args!("referenced blob {id}"), error)
            })?;
            if !metadata.is_file() || metadata.len() != size {
                return Err(object_unavailable(format!(
                    "referenced blob {id} has invalid size or type"
                )));
            }
            after = id.to_hex();
        }
    }
}

/// Removes blobs no catalog row references; misplaced entries fail recovery.
fn remove_orphan_blobs(connection: &Connection, data_dir: &Path) -> Result<(), ObjectError> {
    // Directory iterators and indexed single-ID probes also keep orphan scans bounded.
    for shard in fs::read_dir(data_dir.join("blobs"))? {
        let shard = shard?;
        if os_metadata(&shard) {
            continue;
        }
        if !shard.file_type()?.is_dir() {
            return Err(object_unavailable(format!(
                "invalid blob directory entry {}",
                shard.path().display()
            )));
        }
        for blob in fs::read_dir(shard.path())? {
            let blob = blob?;
            if os_metadata(&blob) {
                continue;
            }
            let id = blob.file_name().into_string().map_err(|_| {
                object_unavailable(format!("invalid blob ID {}", blob.path().display()))
            })?;
            let id = id.parse::<BlobId>().map_err(|error| {
                crate::storage::failure::object_context(
                    format_args!("blob entry {}", blob.path().display()),
                    error,
                )
            })?;
            let expected = id.blob_path(data_dir);
            if expected != blob.path() {
                return Err(object_unavailable(format!(
                    "blob in wrong shard {}",
                    blob.path().display()
                )));
            }
            // The same statement as `reclaim`, so this per-blob scan reuses its cache entry.
            let referenced: bool = blob_referenced(connection, &id)?;
            if !referenced {
                unlink(&blob.path()).map_err(|error| {
                    crate::storage::failure::object_context(
                        format_args!("blob entry {}", blob.path().display()),
                        error,
                    )
                })?;
            }
        }
        #[cfg(unix)]
        sync_dir(&shard.path())?;
    }
    Ok(())
}

/// An open immutable generation. Unlinking its directory entry cannot change its bytes.
pub struct ObjectReader {
    file: ReaderData,
    remaining: u64,
    position: u64,
    /// Selected with the object and authorization, so a concurrent policy change cannot mix headers.
    pub(crate) publicly_readable: bool,
}

pub(super) enum ReaderData {
    Disk(tokio::fs::File),
    Memory(super::super::memory::Reader),
}

#[cfg(test)]
impl ReaderData {
    async fn stream_position(&mut self) -> io::Result<u64> {
        match self {
            Self::Disk(file) => file.stream_position().await,
            Self::Memory(reader) => io::Seek::stream_position(reader),
        }
    }
}

impl ObjectReader {
    /// Appendable generations grow in place, so reads stop at the persisted size.
    pub(super) fn bounded_data(file: ReaderData, remaining: u64) -> Self {
        Self {
            file,
            remaining,
            position: 0,
            publicly_readable: false,
        }
    }

    /// Wraps an arbitrary file so server tests can exercise reader failures.
    #[cfg(test)]
    pub(crate) fn bounded_for_test(file: tokio::fs::File, remaining: u64) -> Self {
        Self::bounded_data(ReaderData::Disk(file), remaining)
    }

    /// Transfers the already-opened immutable generation; no storage path is exposed.
    pub async fn into_file(self) -> io::Result<File> {
        match self.file {
            ReaderData::Disk(file) => Ok(file.into_std().await),
            ReaderData::Memory(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "RAM objects have no filesystem descriptor",
            )),
        }
    }

    pub(crate) async fn into_reader(self) -> Box<dyn io::Read + Send> {
        match self.file {
            ReaderData::Disk(file) => Box::new(file.into_std().await),
            ReaderData::Memory(reader) => Box::new(reader),
        }
    }

    pub(crate) async fn set_range(&mut self, offset: u64, length: u64) -> io::Result<()> {
        match &mut self.file {
            ReaderData::Disk(file) => {
                file.seek(io::SeekFrom::Start(offset)).await?;
            }
            ReaderData::Memory(reader) => {
                io::Seek::seek(reader, io::SeekFrom::Start(offset))?;
            }
        }
        self.remaining = length;
        self.position = offset;
        Ok(())
    }

    /// Reads up to `limit` leading bytes and rewinds, so a caller can classify the content
    /// before choosing response headers. The selected range and current position are preserved;
    /// it reads one bounded chunk, never the whole object.
    pub(crate) async fn peek_prefix(&mut self, limit: usize) -> io::Result<Bytes> {
        let offset = self.position;
        let remaining = self.remaining;
        let prefix = match self.next_chunk().await? {
            Some(bytes) => bytes.slice(..bytes.len().min(limit)),
            None => Bytes::new(),
        };
        self.set_range(offset, remaining).await?;
        Ok(prefix)
    }

    pub async fn next_chunk(&mut self) -> io::Result<Option<Bytes>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let limit = self.remaining.min(CHUNK_SIZE as u64);
        if let ReaderData::Memory(reader) = &mut self.file {
            let bytes = reader
                .next_chunk(limit as usize)
                .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
            self.remaining -= bytes.len() as u64;
            self.position += bytes.len() as u64;
            return Ok(Some(bytes));
        }
        // Reading into spare capacity skips zero-filling a buffer the read overwrites;
        // `take` keeps the read within the persisted size even if capacity is larger.
        let mut buffer = BytesMut::with_capacity(limit as usize);
        let ReaderData::Disk(file) = &mut self.file else {
            return Err(io::Error::other("invalid object reader backend"));
        };
        let length = file.take(limit).read_buf(&mut buffer).await?;
        if length == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        self.remaining -= length as u64;
        self.position += length as u64;
        Ok(Some(buffer.freeze()))
    }
}

/// Creates an exclusive staging file; publication checks the final name atomically.
pub(super) async fn create_upload_stage(stage: PathBuf) -> Result<File, ObjectError> {
    crate::task::run_blocking("upload-stage", move || -> io::Result<File> {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(stage)
    })
    .await?
    .map_err(object_unavailable)
}

/// Whether a transaction left blobs for reclamation.
#[must_use = "journaled blobs require reclaim_if"]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Journaled(bool);

impl Journaled {
    pub(super) const NOTHING: Self = Self(false);
    pub(super) const SOME: Self = Self(true);
    pub(super) fn any(self) -> bool {
        self.0
    }
}

impl From<Option<BlobId>> for Journaled {
    fn from(value: Option<BlobId>) -> Self {
        Self(value.is_some())
    }
}

impl std::ops::BitOr for Journaled {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// A publication candidate whose outcome has settled before its reference is probed.
pub(super) struct StagedBlob {
    pub(super) id: BlobId,
    pub(super) stage: Option<PathBuf>,
}

impl StagedBlob {
    pub(super) fn pin(backend: &super::BlobBackend, root: &Path, id: BlobId) -> Self {
        Self {
            id,
            stage: (!backend.is_memory()).then(|| id.staging_path(root)),
        }
    }
}

enum CommitOutcome {
    Published {
        record: ObjectRecord,
        journaled: Journaled,
    },
    Replayed(ObjectRecord),
    Unchanged(ObjectRecord),
}

impl CommitOutcome {
    fn into_record(self) -> ObjectRecord {
        match self {
            Self::Published { record, .. } | Self::Replayed(record) | Self::Unchanged(record) => {
                record
            }
        }
    }
}

/// The streamed body's size and digests.
struct UploadedBody {
    size: u64,
    md5: [u8; 16],
    crc32c: u32,
    sha256: [u8; 32],
}

/// Everything a single-request upload commits once its blob is durable.
struct UploadCommit {
    backend: crate::config::Backend,
    bucket: String,
    name: String,
    content_type: String,
    metadata: ObjectMetadataUpdate,
    conditions: ObjectConditions,
    access: BucketAccess,
    principals: Option<super::principals::Profile>,
    mode: super::admin::PublishMode,
    identity: PublishIdentity,
    owner: OwnerAssignment,
    replay: Option<super::upload_replay::UploadReplay>,
    clock: Arc<dyn Clock>,
    blob_id: BlobId,
    body: UploadedBody,
}

pub(super) enum OwnerAssignment {
    Request,
    Preserve(Option<crate::object::owner::ObjectOwner>),
    Unknown,
}

/// The authority that fixes a publication's generation and timestamps.
#[derive(Clone, Copy)]
pub(super) enum PublishIdentity {
    Assigned,
    /// Admin publication validates imported identities against their request before admission.
    Imported(super::admin::ImportedIdentity),
}

impl PublishIdentity {
    fn assignment(self) -> GenerationAssignment {
        match self {
            Self::Assigned => GenerationAssignment::Automatic,
            Self::Imported(identity) => identity.generation().map_or(
                GenerationAssignment::Automatic,
                GenerationAssignment::Imported,
            ),
        }
    }

    fn timestamps(self) -> Option<(std::time::SystemTime, std::time::SystemTime)> {
        match self {
            Self::Assigned => None,
            Self::Imported(identity) => Some((identity.time_created(), identity.updated())),
        }
    }

    fn checks_past_retention(self) -> bool {
        matches!(self, Self::Assigned)
    }

    fn retires_appendable(self) -> bool {
        !matches!(self, Self::Assigned)
    }
}

impl UploadCommit {
    /// Identifies the request for upload replay: target, metadata, conditions and body.
    fn fingerprint(&self) -> Vec<u8> {
        super::upload_replay::fingerprint(&(
            &self.bucket,
            &self.name,
            &self.content_type,
            &self.metadata,
            self.conditions,
            self.body.size,
            self.body.sha256,
        ))
    }

    fn record(
        &self,
        generation: i64,
        defaults: PublicationDefaults,
    ) -> Result<ObjectRecord, ObjectError> {
        let time = defaults.time;
        let mut record = defaults.record(
            ObjectTarget::new(&self.bucket, &self.name, generation),
            self.content_type.clone(),
            self.body.size,
            Hashes {
                md5: self.body.md5,
                crc32c: self.body.crc32c,
            },
        );
        if self.identity.checks_past_retention() {
            self.metadata.reject_past_retention(time)?;
        }
        self.metadata.clone().apply(&mut record)?;
        if let Some((created, updated)) = self.identity.timestamps() {
            record.time_created = created;
            record.updated = updated;
        }
        Ok(record)
    }

    /// Publishes the upload in one immediate transaction, or returns the replayed or
    /// unchanged record.
    fn commit(self, connection: &mut Connection) -> Result<CommitOutcome, ObjectError> {
        let transaction = sql::immediate(connection)?;
        let now = PublicationTime::read(self.clock.as_ref())?;
        let (_, delete_permission) = authorized_bucket(&transaction, &self.bucket, &self.access)?;
        let micros = now.micros;
        let fingerprint = self.replay.as_ref().map(|_| self.fingerprint());
        if let (Some(replay), Some(fingerprint)) = (&self.replay, &fingerprint) {
            replay.check_incarnation(&transaction, &self.bucket)?;
            if let Some(record) = replay.replay(&transaction, micros, fingerprint)? {
                return Ok(CommitOutcome::Replayed(record));
            }
            super::upload_replay::capacity(&transaction, micros)?;
        }
        let bucket = BucketPublication::load(&transaction, &self.bucket)?;
        let storage_class = bucket.storage_class(self.metadata.storage_class);
        let previous = lookup(&transaction, &self.bucket, &self.name)?;
        // Rechecked under the write lock so a concurrent create cannot be overwritten.
        check_publication(
            previous.as_ref(),
            self.conditions,
            delete_permission,
            now.time,
        )?;
        let body = &self.body;
        if skip_same(
            self.mode,
            previous.as_ref(),
            body.size,
            body.md5,
            body.crc32c,
        ) {
            return previous
                .map(|stored| CommitOutcome::Unchanged(stored.record))
                .ok_or(ObjectError::ObjectNotFound);
        }
        let defaults = PublicationDefaults {
            storage_class,
            event_based_hold: bucket.default_event_hold,
            time: now.time,
            owner: match &self.owner {
                OwnerAssignment::Request => Some(super::object_owner::for_publication(
                    &transaction,
                    &self.bucket,
                    &self.access,
                    self.principals.as_ref(),
                )?),
                OwnerAssignment::Preserve(owner) => owner.clone(),
                OwnerAssignment::Unknown => None,
            },
        };
        let (record, retired, ()) = publish_in_transaction(
            &transaction,
            previous.as_ref(),
            &bucket,
            NewVersion {
                blob_id: self.blob_id,
                assignment: self.identity.assignment(),
                micros,
                origin: super::change_journal::origin(&self.access),
                notification_mode: NotificationMode::for_access(&self.access),
                owner_profile: self.principals.as_ref(),
                acl: super::acl::PublicationAcl::requested(self.metadata.predefined_acl),
            },
            |generation| {
                let mut record = self.record(generation, defaults)?;
                inherit_metadata(self.mode, previous.as_ref(), &mut record);
                Ok((record, ()))
            },
        )?;
        self.finish_metadata(&transaction, &record)?;
        if let (Some(replay), Some(fingerprint)) = (&self.replay, &fingerprint) {
            replay.store(
                &transaction,
                micros,
                fingerprint,
                &record,
                &self.access,
                None,
            )?;
        }
        commit_publication(transaction)?;
        Ok(CommitOutcome::Published {
            record,
            journaled: retired.into(),
        })
    }

    fn finish_metadata(
        &self,
        transaction: &Transaction<'_>,
        record: &ObjectRecord,
    ) -> Result<(), ObjectError> {
        super::admin::apply_acl(
            transaction,
            self.metadata.acl.as_deref(),
            &self.access,
            record,
        )?;
        // Only a reserved generation can match an appendable row.
        if self.identity.retires_appendable() {
            super::appendable::retire_published(transaction, record, self.backend)?;
        }
        Ok(())
    }
}

impl Catalog {
    /// Each examined entry requires one indexed seek; unmatched candidates have a page budget.
    pub(crate) async fn list_objects(
        &self,
        query: ObjectListQuery,
        limit: usize,
        access: &BucketAccess,
    ) -> Result<ObjectListPage, ObjectError> {
        debug_assert!(
            (1..=1000).contains(&limit),
            "adapters validate object list limits"
        );
        let limit = limit.clamp(1, 1000);
        let access = access.clone();
        let clock = self.0.clock.clone();
        let soft_deleted = query.soft_deleted;
        let result = self
            .run(move |connection| {
                // The catalog worker serializes mutations; each seek ends before glob matching.
                authorized_bucket(connection, &query.bucket, &access)?;
                let now = unix_micros(clock.now())?;
                if query.soft_deleted {
                    super::soft_delete::purge_expired(connection, &query.bucket, now)?;
                }
                let mut page = list_page_at(connection, &query, limit, now)?;
                page.bucket_metageneration =
                    Some(bucket_metageneration(connection, &query.bucket)?);
                Ok(page)
            })
            .await;
        if soft_deleted {
            self.reclaim_after_commit().await;
        }
        result
    }

    pub(super) async fn run_upload<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, ObjectError> + Send + 'static,
    ) -> Result<T, ObjectError> {
        let execution = self
            .0
            .execution
            .clone()
            .acquire_owned()
            .await
            .map_err(object_unavailable)?;
        self.run_upload_admitted(execution, operation).await
    }

    pub(super) async fn run_upload_admitted<T: Send + 'static>(
        &self,
        execution: OwnedSemaphorePermit,
        operation: impl FnOnce(&mut Connection) -> Result<T, ObjectError> + Send + 'static,
    ) -> Result<T, ObjectError> {
        let activity = self.activity()?;
        let inner = self.0.clone();
        self.0
            .executor
            .run(move || {
                let (_execution, _activity) = (execution, activity);
                match inner.with_connection(operation) {
                    Ok(result) => result,
                    Err(super::ConnectionUnavailable::Closed) => Err(object_unavailable(
                        crate::storage::failure::BackendFailure::CatalogClosed,
                    )),
                    Err(error) => Err(object_unavailable(error)),
                }
            })
            .await
            .map_err(object_unavailable)?
    }

    /// Advisory check that also rejects a bucket or an overwrite the caller is not authorized
    /// for. Publication repeats every part of it inside its own transaction.
    pub(crate) async fn check_upload(
        &self,
        bucket: &str,
        name: &str,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let access = access.clone();
        let clock = self.0.clock.clone();
        self.run_waiting(move |connection| {
            let transaction = connection.transaction()?;
            let (_, delete_permission) = authorized_bucket(&transaction, &bucket, &access)?;
            let previous = lookup(&transaction, &bucket, &name)?;
            check_publication(
                previous.as_ref(),
                conditions,
                delete_permission,
                clock.now(),
            )
        })
        .await
    }

    /// HTTP connection admission bounds streaming uploads.
    /// File I/O uses short Tokio filesystem jobs, never a blocking worker awaiting a body.
    #[cfg(test)]
    pub(crate) async fn put_object(
        &self,
        upload: ObjectUpload<'_>,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
    ) -> Result<ObjectRecord, ObjectError> {
        let permit = self
            .prepare_object_upload(
                upload.bucket,
                upload.name,
                upload.conditions,
                &upload.access,
            )
            .await?;
        self.put_object_prechecked(upload, body, permit).await
    }

    pub(crate) async fn prepare_object_upload(
        &self,
        bucket: &str,
        name: &str,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<UploadGuard, ObjectError> {
        let permit = self.admit_object_upload()?;
        self.check_upload(bucket, name, conditions, access).await?;
        Ok(permit)
    }

    pub(crate) fn admit_object_upload(&self) -> Result<UploadGuard, ObjectError> {
        let activity = self.activity()?;
        let mut uploads = self
            .0
            .uploads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if uploads.closed || !self.is_healthy() {
            return Err(object_unavailable(
                crate::storage::failure::BackendFailure::CatalogClosed,
            ));
        }
        uploads.active += 1;
        Ok(UploadGuard {
            slot: super::UploadSlot::Counted(self.clone()),
            replay: None,
            _activity: activity,
        })
    }

    pub(crate) async fn put_object_prechecked(
        &self,
        upload: ObjectUpload<'_>,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
        permit: UploadGuard,
    ) -> Result<ObjectRecord, ObjectError> {
        self.put_object_with_mode(
            upload,
            body,
            permit,
            super::admin::PublishMode::Replace,
            PublishIdentity::Assigned,
            OwnerAssignment::Request,
        )
        .await
    }

    pub(super) async fn put_object_with_mode(
        &self,
        upload: ObjectUpload<'_>,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
        mut permit: UploadGuard,
        mode: super::admin::PublishMode,
        identity: PublishIdentity,
        owner: OwnerAssignment,
    ) -> Result<ObjectRecord, ObjectError> {
        let ObjectUpload {
            bucket,
            name,
            content_type,
            expected,
            conditions,
            max_bytes,
            metadata,
            access,
        } = upload;
        crate::object::validate_fixed_metadata("contentType", &content_type)?;
        metadata.validate()?;
        let catalog = self.clone();
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let (_request, cancellation) = oneshot::channel::<()>();
        let replay = permit.replay.take();
        // Cancellation must not release directory ownership during publication or cleanup.
        crate::task::spawn_supervised("object-upload", async move {
            let _permit = permit;
            let blob = super::super::blob_id::BlobId::generate().map_err(object_unavailable)?;
            let id = blob;
            let stage = blob.staging_path(&catalog.0.data_dir);
            let published = blob.blob_path(&catalog.0.data_dir);
            let mut file = catalog.0.backend.stage(&id).await?;
            let result = async {
                let (size, md5, crc32c, sha256) =
                    write_stream(&mut file, expected, max_bytes, body, cancellation, &catalog)
                        .await?;
                match file {
                    UploadSink::Memory(memory) => memory.publish(id.to_hex())?,
                    UploadSink::Disk(file) => {
                        drop(file);
                        catalog
                            .publish_staged(stage.clone(), published.clone())
                            .await?;
                    }
                }
                let commit = UploadCommit {
                    backend: catalog.0.backend.kind(),
                    bucket,
                    name,
                    content_type,
                    metadata,
                    conditions,
                    access,
                    principals: catalog.0.principals.get().cloned(),
                    mode,
                    identity,
                    owner,
                    replay,
                    clock: catalog.0.clock.clone(),
                    blob_id: id,
                    body: UploadedBody {
                        size,
                        md5,
                        crc32c,
                        sha256,
                    },
                };
                catalog
                    .run_upload(move |connection| commit.commit(connection))
                    .await
            }
            .await;
            match &result {
                Ok(CommitOutcome::Published { journaled, .. }) => {
                    catalog.reclaim_if(*journaled).await
                }
                Ok(CommitOutcome::Replayed(_) | CommitOutcome::Unchanged(_)) | Err(_) => {
                    catalog
                        .discard_staged(StagedBlob {
                            id,
                            stage: (!catalog.0.backend.is_memory()).then_some(stage),
                        })
                        .await;
                }
            }
            result.map(CommitOutcome::into_record)
        })
        .await?
    }

    /// Cleanup cannot change a settled publication result; recovery retries remaining bytes.
    pub(super) async fn discard_staged(&self, blob: StagedBlob) {
        if let Err(error) = self.try_discard_staged(blob).await {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "upload_cleanup_deferred",
                serde_json::json!({"message": format!("deferred upload cleanup: {error}")}),
            );
        }
    }

    async fn try_discard_staged(&self, blob: StagedBlob) -> Result<(), ObjectError> {
        let StagedBlob { id, stage } = blob;
        if let Some(stage) = stage {
            crate::task::run_blocking("upload-discard", move || unlink(&stage)).await??;
        }
        #[cfg(test)]
        fault::hit(&self.0.data_dir, Step::DiscardProbe).map_err(object_unavailable)?;
        // Probe the committed reference before cleanup: SQL commit errors can be ambiguous.
        let journaled = self
            .run_upload(move |connection| {
                if blob_referenced(connection, &id)? {
                    return Ok(Journaled::NOTHING);
                }
                sql::exec(
                    connection,
                    "INSERT OR IGNORE INTO pending_blob_deletions VALUES (?1)",
                    [id],
                )?;
                Ok(Journaled::SOME)
            })
            .await?;
        if journaled.any() {
            self.reclaim_pending().await?;
        }
        Ok(())
    }

    pub(crate) async fn get_object(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        let ObjectTarget {
            bucket,
            name,
            generation,
        } = target;
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            selected(
                &transaction,
                &bucket,
                &name,
                generation,
                conditions,
                RequestKind::Read,
                &access,
            )
            .map(|stored| stored.record)
        })
        .await
    }

    pub(crate) async fn authorize_object_snapshot(
        &self,
        record: &ObjectRecord,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        let record = record.clone();
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            authorize_generation(
                &transaction,
                &record.bucket,
                &record.name,
                record.generation,
                &access,
            )
        })
        .await
    }

    /// The bucket's project is verified in the same call that resolves the generation and
    /// opens its blob, so the returned reader never belongs to an unauthorized bucket.
    /// Callers without a customer-supplied key cannot open objects encrypted with one.
    pub(crate) async fn open_object(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(ObjectRecord, ObjectReader), ObjectError> {
        self.open_object_keyed(target, conditions, access, None)
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
                RequestKind::Write,
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
        self.run(move |connection| {
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
        self.reclaim_after_commit().await;
        Ok(())
    }
}

/// Shared selection preserves protocol conditions and access.
pub(super) fn open_selected(
    connection: &mut Connection,
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
    let transaction = connection.transaction()?;
    let StoredObject { record, blob: id } = selected(
        &transaction,
        bucket,
        name,
        *generation,
        conditions,
        RequestKind::Read,
        access,
    )?;
    // Open under the same execution permit that serializes deletes and overwrites.
    let file = catalog.0.backend.open(&id)?;
    let remaining = record.size;
    let publicly_readable = super::policies::read_policy(&transaction, bucket)?.allows(
        None,
        crate::auth::OBJECTS_GET,
        super::policies::policy_public_access(&transaction, bucket)?,
    ) || super::acl::object_granted(
        &transaction,
        bucket,
        name,
        record.generation,
        &BucketAccess::Policy {
            identity: None,
            permission: crate::auth::Permission::ObjectsGet,
            project: None,
        },
    )?;
    Ok((
        record,
        ObjectReader {
            file,
            remaining,
            position: 0,
            publicly_readable,
        },
    ))
}

/// Shared selection preserves protocol conditions and access.
pub(super) fn delete_selected(
    connection: &mut Connection,
    clock: &dyn crate::clock::Clock,
    target: &ObjectTarget,
    conditions: ObjectConditions,
    access: &BucketAccess,
    sync_binding: Option<&super::sync_state::SyncBinding>,
) -> Result<(), ObjectError> {
    let ObjectTarget {
        bucket,
        name,
        generation,
    } = target;
    authorized_bucket(connection, bucket, access)?;
    super::soft_delete::purge_expired(connection, bucket, unix_micros(clock.now())?)?;
    let transaction = sql::immediate(connection)?;
    let StoredObject { record, blob: id } = selected(
        &transaction,
        bucket,
        name,
        *generation,
        conditions,
        RequestKind::Write,
        access,
    )?;
    let publication = BucketPublication::load(&transaction, bucket)?;
    // Deleting a live version in a versioned bucket only makes it noncurrent.
    let retirement = if publication.versioning == Versioning::Enabled && generation.is_none() {
        Retirement::Noncurrent
    } else {
        Retirement::Removed
    };
    if retirement == Retirement::Noncurrent {
        record.check_policy_and_holds(clock.now())?;
    } else {
        record.check_retention(clock.now())?;
    }
    if let Some(binding) = sync_binding {
        super::sync_state::delete_in_transaction(&transaction, binding, &record)?;
    }
    super::change_journal::object(
        &transaction,
        super::change_journal::origin(access),
        super::change_journal::ChangeKind::Delete,
        &record,
    )?;
    retire_previous(
        &transaction,
        Some(&StoredObject { record, blob: id }),
        retirement,
        &publication,
        RetirementContext {
            micros: unix_micros(clock.now())?,
            origin: super::change_journal::origin(access),
            replacement: None,
            notification_mode: NotificationMode::for_access(access),
        },
    )?;
    transaction.commit()?;
    Ok(())
}

pub(super) enum UploadSink {
    Disk(tokio::fs::File),
    Memory(super::super::memory::Stage),
}

pub(super) async fn write_stream(
    file: &mut UploadSink,
    mut expected: ExpectedHashes,
    max_bytes: u64,
    mut body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
    mut cancellation: oneshot::Receiver<()>,
    catalog: &Catalog,
) -> Result<(u64, [u8; 16], u32, [u8; 32]), ObjectError> {
    let mut size = 0_u64;
    let mut hasher = Hasher::default();
    let mut sha256 = ring::digest::Context::new(&ring::digest::SHA256);
    let closing = catalog.0.admin_shutdown.notified();
    tokio::pin!(closing);
    closing.as_mut().enable();
    #[cfg(test)]
    let data_dir = &catalog.0.data_dir;
    loop {
        if catalog.0.admission.is_closed() {
            return Err(object_unavailable("catalog is closing"));
        }
        let chunk = tokio::select! {
            biased;
            _ = &mut closing => return Err(object_unavailable("catalog is closing")),
            _ = &mut cancellation => return Err(ObjectError::BodyAborted("upload cancelled".into())),
            chunk = body.recv() => chunk,
        };
        let chunk = chunk
            .ok_or_else(|| ObjectError::BodyAborted("missing body EOF".into()))?
            .map_err(|error| {
                if error.kind() == io::ErrorKind::StorageFull {
                    ObjectError::Io(error.into())
                } else {
                    ObjectError::body_aborted(error)
                }
            })?;
        let chunk = match chunk {
            UploadChunk::End => break,
            UploadChunk::EndWithHashes(hashes) => {
                if hashes.md5.zip(expected.md5).is_some_and(|(a, b)| a != b)
                    || hashes
                        .crc32c
                        .zip(expected.crc32c)
                        .is_some_and(|(a, b)| a != b)
                {
                    return Err(ObjectError::ChecksumMismatch);
                }
                expected.md5 = expected.md5.or(hashes.md5);
                expected.crc32c = expected.crc32c.or(hashes.crc32c);
                break;
            }
            UploadChunk::Data(chunk) => chunk,
        };
        size = size
            .checked_add(chunk.len() as u64)
            .filter(|size| *size <= max_bytes)
            .ok_or(ObjectError::TooLarge)?;
        for part in chunk.chunks(CHUNK_SIZE) {
            hasher.update(part);
            sha256.update(part);
            #[cfg(test)]
            fault::hit(data_dir, Step::BodyWrite).map_err(object_unavailable)?;
            match file {
                UploadSink::Disk(file) => file.write_all(part).await?,
                UploadSink::Memory(stage) => stage.write(part)?,
            }
        }
    }
    let Hashes { md5, crc32c } = hasher.finish();
    if let Some(provided) = expected.crc32c.filter(|value| *value != crc32c) {
        return Err(ObjectError::HashMismatch(Box::new(HashMismatch {
            algorithm: HashAlgorithm::Crc32c,
            provided: provided.to_be_bytes().to_vec(),
            calculated: crc32c.to_be_bytes().to_vec(),
        })));
    }
    if let Some(provided) = expected.md5.filter(|value| *value != md5) {
        return Err(ObjectError::HashMismatch(Box::new(HashMismatch {
            algorithm: HashAlgorithm::Md5,
            provided: provided.to_vec(),
            calculated: md5.to_vec(),
        })));
    }
    if let UploadSink::Disk(file) = file {
        make_durable(
            file,
            #[cfg(test)]
            data_dir,
        )
        .await?;
    }
    Ok((
        size,
        md5,
        crc32c,
        sha256
            .finish()
            .as_ref()
            .try_into()
            .map_err(object_unavailable)?,
    ))
}

/// Makes every byte written to a staged body durable before it can be published.
///
/// Tests record the synced length for power-loss snapshots from the `sync_all` result
/// itself, so a write path that skips this call, or a sync dropped from it together with
/// the result it yields, leaves its bytes volatile and fails the power-loss crash tests.
pub(super) async fn make_durable(
    file: &mut tokio::fs::File,
    #[cfg(test)] data_dir: &Path,
) -> Result<(), ObjectError> {
    #[cfg(test)]
    fault::hit(data_dir, Step::BodyFlush).map_err(object_unavailable)?;
    // tokio finishes the last write in the background, and sync_all drops its error;
    // flush reports it, so a failed tail cannot publish a short blob at the full size.
    file.flush().await?;
    #[cfg(test)]
    fault::hit(data_dir, Step::BodySync).map_err(object_unavailable)?;
    let synced = file.sync_all().await;
    #[cfg(test)]
    if synced.is_ok() {
        fault::file_synced(data_dir, &file.metadata().await?);
    }
    synced.map_err(object_unavailable)
}

pub(super) use crate::storage::failure::object_unavailable;

pub(super) fn prefix_end(prefix: &str) -> Option<Vec<u8>> {
    let mut end = Vec::new();
    prefix_end_into(prefix, &mut end).then_some(end)
}

fn prefix_end_into(prefix: &str, end: &mut Vec<u8>) -> bool {
    end.clear();
    end.extend_from_slice(prefix.as_bytes());
    let Some(index) = end.iter().rposition(|byte| *byte != u8::MAX) else {
        return false;
    };
    end[index] += 1;
    end.truncate(index + 1);
    true
}

/// Which rows a listing walks: live objects, every live generation, or soft-deleted ones.
#[derive(Clone, Copy)]
enum ListSource {
    Live,
    Versions,
    SoftDeleted,
}

impl ListSource {
    fn of(query: &ObjectListQuery) -> Self {
        if query.soft_deleted {
            Self::SoftDeleted
        } else if query.versions {
            Self::Versions
        } else {
            Self::Live
        }
    }
}

/// Whether a seek may return the lower bound itself.
#[derive(Clone, Copy)]
pub(super) enum SeekStart {
    Inclusive,
    Exclusive,
}

/// Whether a listing stops before an upper name bound.
#[derive(Clone, Copy)]
enum UpperBound {
    Open,
    Bounded,
}

// Every list statement numbers its parameters the same way: bucket ?1, lower bound ?2, upper
// bound ?3, generation ?4 and the current time ?5. Binding the time instead of embedding it
// gives each listing shape one static text that the connection cache reuses.
macro_rules! visible_live {
    () => {
        "soft_delete_time IS NULL"
    };
}
macro_rules! visible_soft_deleted {
    () => {
        "soft_delete_time IS NOT NULL AND hard_delete_time > ?5"
    };
}
macro_rules! name_upper_bound {
    () => {
        "AND name < CAST(?3 AS TEXT)"
    };
}
macro_rules! list_seek {
    ($source:literal, $comparison:literal, $upper:expr, $visible:expr, $order:literal) => {
        concat!(
            "SELECT ",
            object_columns!(),
            " FROM ",
            $source,
            " WHERE bucket = ?1 AND name ",
            $comparison,
            " CAST(?2 AS TEXT) ",
            $upper,
            " AND ",
            $visible,
            " ORDER BY ",
            $order,
            " LIMIT 1"
        )
    };
}
macro_rules! list_versioned {
    ($upper:expr, $visible:expr) => {
        concat!(
            "SELECT ",
            object_columns!(),
            " FROM objects WHERE bucket = ?1 AND (name, generation) > (CAST(?2 AS TEXT), ?4) ",
            $upper,
            " AND ",
            $visible,
            " ORDER BY name, generation LIMIT 1"
        )
    };
}

fn list_seek_sql(source: ListSource, start: SeekStart, upper: UpperBound) -> &'static str {
    macro_rules! shapes {
        ($source:literal, $visible:expr, $order:literal) => {
            match (start, upper) {
                (SeekStart::Inclusive, UpperBound::Open) => {
                    list_seek!($source, ">=", "", $visible, $order)
                }
                (SeekStart::Inclusive, UpperBound::Bounded) => {
                    list_seek!($source, ">=", name_upper_bound!(), $visible, $order)
                }
                (SeekStart::Exclusive, UpperBound::Open) => {
                    list_seek!($source, ">", "", $visible, $order)
                }
                (SeekStart::Exclusive, UpperBound::Bounded) => {
                    list_seek!($source, ">", name_upper_bound!(), $visible, $order)
                }
            }
        };
    }
    match source {
        ListSource::SoftDeleted => shapes!(
            "objects INDEXED BY objects_soft_deleted",
            visible_soft_deleted!(),
            "name, generation"
        ),
        ListSource::Versions => shapes!("objects", visible_live!(), "name, generation"),
        // The primary key includes history; a live seek must not walk noncurrent rows.
        ListSource::Live => shapes!(
            "objects INDEXED BY objects_live",
            "time_deleted IS NULL",
            "name"
        ),
    }
}

fn list_versioned_sql(source: ListSource, upper: UpperBound) -> &'static str {
    match (source, upper) {
        (ListSource::SoftDeleted, UpperBound::Open) => {
            list_versioned!("", visible_soft_deleted!())
        }
        (ListSource::SoftDeleted, UpperBound::Bounded) => {
            list_versioned!(name_upper_bound!(), visible_soft_deleted!())
        }
        (_, UpperBound::Open) => list_versioned!("", visible_live!()),
        (_, UpperBound::Bounded) => list_versioned!(name_upper_bound!(), visible_live!()),
    }
}

/// Whether another visible generation of a name follows generation `?4`.
fn later_generation_sql(source: ListSource) -> &'static str {
    match source {
        ListSource::SoftDeleted => concat!(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND name = ?2 AND generation > ?4 AND ",
            visible_soft_deleted!(),
            ")"
        ),
        _ => concat!(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND name = ?2 AND generation > ?4 AND ",
            visible_live!(),
            ")"
        ),
    }
}

/// Runs a list statement with the leading parameters it declares, so shapes that omit the
/// upper bound, generation or time share one binding convention.
fn query_list_row<T>(
    statement: &mut rusqlite::CachedStatement<'_>,
    values: [&dyn rusqlite::ToSql; 5],
    read: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    let count = statement.parameter_count();
    statement.query_row(
        rusqlite::params_from_iter(values.into_iter().take(count)),
        read,
    )
}

/// The three seeks one listing alternates between.
struct ListStatements<'c> {
    inclusive: rusqlite::CachedStatement<'c>,
    exclusive: rusqlite::CachedStatement<'c>,
    versioned: rusqlite::CachedStatement<'c>,
}

impl<'c> ListStatements<'c> {
    fn prepare(
        connection: &'c Connection,
        source: ListSource,
        upper: UpperBound,
    ) -> rusqlite::Result<Self> {
        Ok(Self {
            inclusive: connection.prepare_cached(list_seek_sql(
                source,
                SeekStart::Inclusive,
                upper,
            ))?,
            exclusive: connection.prepare_cached(list_seek_sql(
                source,
                SeekStart::Exclusive,
                upper,
            ))?,
            versioned: connection.prepare_cached(list_versioned_sql(source, upper))?,
        })
    }
}

/// Where the next seek starts, derived from the previous page entry.
struct SeekPosition {
    after: bool,
    after_generation: Option<i64>,
    trailing_prefix: bool,
}

/// Advances `lower` past the previous entry; `None` means the listing is exhausted.
fn seek_position(
    connection: &Connection,
    query: &ObjectListQuery,
    now: i64,
    cursor: Option<&ObjectListCursor>,
    lower: &mut Vec<u8>,
    scratch: &mut Vec<u8>,
) -> Result<Option<SeekPosition>, ObjectError> {
    let mut position = SeekPosition {
        after: false,
        after_generation: None,
        trailing_prefix: false,
    };
    let Some(previous) = cursor else {
        return Ok(Some(position));
    };
    if previous.is_prefix {
        if !prefix_end_into(&previous.name, scratch) {
            return Ok(None);
        }
    } else {
        scratch.clear();
        scratch.extend_from_slice(previous.name.as_bytes());
    }
    if *scratch < *lower {
        return Ok(Some(position));
    }
    std::mem::swap(lower, scratch);
    // A trailing marker emits an item and prefix as separate page entries.
    position.after = !previous.is_prefix && !trailing_marker(&previous.name, query);
    if (query.versions || query.soft_deleted) && !previous.is_prefix {
        position.after_generation = previous.generation;
        if trailing_marker(&previous.name, query) {
            let more: bool = connection
                .prepare_cached(later_generation_sql(ListSource::of(query)))
                .and_then(|mut statement| {
                    let none: Option<Vec<u8>> = None;
                    query_list_row(
                        &mut statement,
                        [
                            &query.bucket,
                            &previous.name,
                            &none,
                            &previous.generation,
                            &now,
                        ],
                        |row| row.get(0),
                    )
                })?;
            if !more {
                position.after_generation = None;
                position.trailing_prefix = true;
            }
        }
    }
    Ok(Some(position))
}

/// The exclusive upper name bound: the prefix's end or the end offset, whichever is lower.
fn list_upper(query: &ObjectListQuery) -> Option<Vec<u8>> {
    let upper = prefix_end(&query.prefix);
    if !query.end_offset.is_empty()
        && upper
            .as_deref()
            .is_none_or(|end| query.end_offset.as_bytes() < end)
    {
        return Some(query.end_offset.as_bytes().to_vec());
    }
    upper
}

pub(super) fn list_page(
    connection: &Connection,
    query: &ObjectListQuery,
    limit: usize,
) -> Result<ObjectListPage, ObjectError> {
    list_page_at(connection, query, limit, 0)
}

fn list_page_at(
    connection: &Connection,
    query: &ObjectListQuery,
    limit: usize,
    now: i64,
) -> Result<ObjectListPage, ObjectError> {
    list_page_within(connection, query, limit, now, LIST_SCAN_BUDGET)
}

/// Unmatched candidates one page may examine before yielding the catalog worker.
const LIST_SCAN_BUDGET: usize = 4096;

fn list_page_within(
    connection: &Connection,
    query: &ObjectListQuery,
    limit: usize,
    now: i64,
    budget: usize,
) -> Result<ObjectListPage, ObjectError> {
    let mut page = ObjectListPage {
        items: Vec::new(),
        prefixes: Vec::new(),
        next: None,
        bucket_metageneration: None,
    };
    let mut cursor = query.cursor.clone();
    let mut lower = query
        .prefix
        .as_bytes()
        .max(query.start_offset.as_bytes())
        .to_vec();
    let upper = list_upper(query);
    let bound = if upper.is_some() {
        UpperBound::Bounded
    } else {
        UpperBound::Open
    };
    let mut statements = ListStatements::prepare(connection, ListSource::of(query), bound)?;
    let mut scratch = Vec::new();
    let mut examined = 0;
    loop {
        let Some(position) = seek_position(
            connection,
            query,
            now,
            cursor.as_ref(),
            &mut lower,
            &mut scratch,
        )?
        else {
            break;
        };
        if upper
            .as_deref()
            .is_some_and(|upper| lower.as_slice() >= upper)
        {
            break;
        }
        let statement = if position.after_generation.is_some() {
            &mut statements.versioned
        } else if position.after {
            &mut statements.exclusive
        } else {
            &mut statements.inclusive
        };
        let record = query_list_row(
            statement,
            [
                &query.bucket,
                &lower,
                &upper,
                &position.after_generation,
                &now,
            ],
            read_object,
        )
        .optional()?;
        let Some(StoredObject { record, .. }) = record else {
            break;
        };
        let group = group_prefix(&record.name, query);
        let is_prefix = group.is_some_and(|_| {
            !trailing_marker(&record.name, query)
                || cursor.as_ref().is_some_and(|cursor| {
                    !cursor.is_prefix
                        && cursor.name == record.name
                        && (!(query.versions || query.soft_deleted) || position.trailing_prefix)
                })
        });
        let name = match group {
            Some(group) if is_prefix => group.to_owned(),
            _ => record.name.clone(),
        };
        let matches = query
            .match_glob
            .as_ref()
            .is_none_or(|glob| glob.matches(&name));
        if matches && page.items.len() + page.prefixes.len() == limit {
            page.next = cursor;
            break;
        }
        cursor = Some(ObjectListCursor {
            name: name.clone(),
            is_prefix,
            generation: ((query.versions || query.soft_deleted) && !is_prefix)
                .then_some(record.generation),
        });
        if !matches {
            examined += 1;
            if examined >= budget {
                page.next = cursor;
                break;
            }
            continue;
        }
        if is_prefix {
            page.prefixes.push(name);
        } else {
            page.items.push(record);
        }
    }
    Ok(page)
}

fn group_prefix<'a>(name: &'a str, query: &ObjectListQuery) -> Option<&'a str> {
    if query.delimiter.is_empty() {
        return None;
    }
    let remainder = name.strip_prefix(&query.prefix)?;
    let index = remainder.find(&query.delimiter)?;
    Some(&name[..query.prefix.len() + index + query.delimiter.len()])
}

fn trailing_marker(name: &str, query: &ObjectListQuery) -> bool {
    query.include_trailing_delimiter
        && group_prefix(name, query) == Some(name)
        && name
            .strip_suffix(&query.delimiter)
            .is_some_and(|stem| !stem.ends_with(&query.delimiter))
}

/// ACL metadata rejections keep their client-input status across publication paths.
pub(super) fn acl_update_error(error: CatalogError) -> ObjectError {
    match error {
        CatalogError::InvalidMetadata(reason) => ObjectError::InvalidFixedMetadata(reason),
        CatalogError::UnsupportedMetadata(reason) => ObjectError::UnsupportedMetadata(reason),
        error => error.into(),
    }
}

/// Resolves the bucket and re-validates `access` against the owning project observed on the
/// same connection, before any object state is read or changed.
///
/// Returns the storage class and whether the caller may delete objects, which replacing a
/// live object also requires. A missing bucket is reported before authorization, matching the
/// adapter's missing-bucket rule.
pub(super) fn authorized_bucket(
    connection: &Connection,
    bucket: &str,
    access: &BucketAccess,
) -> Result<(StorageClass, DeletePermission), ObjectError> {
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

/// The current metageneration of a bucket that `authorized_bucket` has just found.
fn bucket_metageneration(connection: &Connection, bucket: &str) -> Result<i64, ObjectError> {
    sql::optional_scalar(
        connection,
        "SELECT metageneration FROM buckets WHERE name = ?1",
        [bucket],
    )?
    .ok_or(ObjectError::BucketNotFound)
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
    if let Some(access) = object_acl {
        let granted = record
            .as_ref()
            .map(|stored| {
                super::acl::object_granted(
                    connection,
                    bucket,
                    name,
                    stored.record.generation,
                    access,
                )
            })
            .transpose()?
            .unwrap_or(false);
        if !granted {
            return Err(ObjectError::Forbidden);
        }
    }
    let stored = record.ok_or(ObjectError::ObjectNotFound)?;
    check_conditions(Some(&stored.record), conditions, request)?;
    Ok(stored)
}

pub(super) fn object_acl_authority<'a>(
    connection: &Connection,
    bucket: &str,
    access: &'a BucketAccess,
) -> Result<Option<&'a BucketAccess>, ObjectError> {
    match authorized_bucket(connection, bucket, access) {
        Ok(_) => Ok(None),
        Err(ObjectError::Forbidden)
            if matches!(
                access,
                BucketAccess::Policy { permission, .. } if crate::acl::object_permission(*permission)
            ) =>
        {
            Ok(Some(access))
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
    if let Some(access) = object_acl_authority(connection, bucket, access)?
        && !super::acl::object_granted(connection, bucket, name, generation, access)?
    {
        return Err(ObjectError::Forbidden);
    }
    Ok(())
}

/// Preconditions are evaluated first: a failing precondition means nothing would be replaced.
pub(super) fn check_publication(
    previous: Option<&StoredObject>,
    conditions: ObjectConditions,
    delete_permission: DeletePermission,
    now: std::time::SystemTime,
) -> Result<(), ObjectError> {
    check_conditions(
        previous.map(|stored| &stored.record),
        conditions,
        RequestKind::Write,
    )?;
    if previous.is_some() && delete_permission == DeletePermission::Denied {
        return Err(ObjectError::OverwriteForbidden);
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

/// Whether the caller holds `storage.objects.delete`, which replacing a live object requires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeletePermission {
    Granted,
    Denied,
}

/// Whether the bucket keeps replaced live generations as noncurrent versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Versioning {
    Enabled,
    Disabled,
}

/// What happens to a live generation that is replaced or deleted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Retirement {
    /// The generation stays readable as a noncurrent version.
    Noncurrent,
    /// The generation is soft deleted when the bucket retains deletions, or removed.
    Removed,
}

/// Which failure an unmet not-match precondition reports: reads answer 304.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RequestKind {
    Read,
    Write,
}

#[derive(Clone, Copy)]
pub(super) enum GenerationAssignment {
    Automatic,
    Imported(super::admin::Generation),
}

/// The caller owns protocol-specific completion and commits before reclaiming retired bytes.
pub(super) fn publish_in_transaction<T>(
    transaction: &Transaction<'_>,
    previous: Option<&StoredObject>,
    bucket: &BucketPublication,
    version: NewVersion<'_>,
    prepare: impl FnOnce(i64) -> Result<(ObjectRecord, T), ObjectError>,
) -> Result<(ObjectRecord, Option<BlobId>, T), ObjectError> {
    let NewVersion {
        blob_id,
        assignment,
        micros,
        origin,
        notification_mode,
        owner_profile,
        acl,
    } = version;
    let last: i64 = sql::scalar(
        transaction,
        "SELECT last_generation FROM object_generation WHERE singleton = 1",
        [],
    )?;
    let generation = match assignment {
        GenerationAssignment::Automatic => micros.max(
            last.checked_add(1)
                .ok_or_else(|| object_unavailable("generation exhausted"))?,
        ),
        GenerationAssignment::Imported(generation) => generation.get(),
    };
    let (mut record, prepared) = prepare(generation)?;
    if let Some(owner) = &mut record.owner {
        super::object_owner::canonicalize(owner, owner_profile);
    }
    record.retention_expiration_time = bucket.retention_expiration(record.time_created)?;
    let now = system_time_micros(micros)?;
    // A retained live version may become noncurrent, but it cannot be replaced outright.
    if bucket.versioning == Versioning::Disabled
        && let Some(StoredObject {
            record: previous, ..
        }) = previous
    {
        previous.check_object_retention(now)?;
    }
    retention::validate_publication(transaction, &record, now)?;
    validate_publication_timestamps(&record)?;
    let (retention_mode, retain_until, retain_sub_micro) =
        retention::columns(record.retention.as_ref())?;
    let (event_hold, event_released) = hold_columns(record.event_based_hold);
    let (temporary_hold, temporary_released) = hold_columns(record.temporary_hold);
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
            origin,
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
    transaction.prepare_cached(
        "INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,content_encoding,content_disposition,content_language,cache_control,metadata,event_based_hold,component_count,temporary_hold,custom_time,retention_mode,retain_until_time,customer_key_sha256,md5_available,owner,retain_until_sub_micro_nanos,temporary_hold_released,event_based_hold_released) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?19,?11,?12,?13,?14,?15,?16,?17,?18,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29)",
    ).and_then(|mut statement| statement.execute(params![record.bucket, record.name, generation, record.metageneration,
            record.content_type, size,
            &record.md5[..], record.crc32c, record.storage_class, time_created, blob_id,
            record.content_encoding, record.content_disposition, record.content_language,
            record.cache_control, metadata, event_hold, record.component_count,
            updated, temporary_hold, record.custom_time,
            retention_mode, retain_until,
            record.customer_key_sha256.as_ref().map(|sha256| &sha256[..]),
            record.md5_availability == crate::object::Md5Availability::Available, owner,
            retain_sub_micro, temporary_released, event_released]))?;
    super::acl::publish_object_acl(transaction, &record, acl)?;
    if notification_mode == NotificationMode::Emit {
        super::notification_outbox::published(transaction, origin, &record, previous, now)?;
    }
    super::change_journal::object(
        transaction,
        origin,
        super::change_journal::ChangeKind::Publish,
        &record,
    )?;
    Ok((record, retired, prepared))
}

fn validate_publication_timestamps(record: &ObjectRecord) -> Result<(), ObjectError> {
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

impl Catalog {
    pub(super) async fn reclaim_if(&self, journaled: Journaled) {
        if journaled.any() {
            self.reclaim_after_commit().await;
        }
    }

    pub(super) async fn reclaim_after_commit(&self) {
        if let Err(error) = self.reclaim_pending().await {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "blob_reclaim_deferred",
                serde_json::json!({"message":format!("deferred blob reclamation: {error}")}),
            );
        }
    }

    pub(super) async fn reclaim_pending(&self) -> Result<(), ObjectError> {
        let activity = self.activity()?;
        let permit = self
            .0
            .reclamation
            .clone()
            .acquire_owned()
            .await
            .map_err(object_unavailable)?;
        let catalog = self.clone();
        // Detached callers retain the cleanup slot until filesystem work and SQL settle.
        let task = crate::task::spawn_supervised("blob-reclamation", async move {
            let (_permit, _activity) = (permit, activity);
            let (ids, conflict) = catalog
                .run_upload(|connection| {
                    let ids = connection
                        .prepare_cached(
                            // Cancelled sessions need their marker until staging cleanup also succeeds.
                            "SELECT blob_id FROM pending_blob_deletions AS pending WHERE NOT EXISTS (SELECT 1 FROM resumable_sessions AS session WHERE session.blob_id = pending.blob_id AND session.state != 'COMPLETE') AND NOT EXISTS (SELECT 1 FROM objects WHERE objects.blob_id = pending.blob_id) ORDER BY blob_id LIMIT 128",
                        )?
                        .query_map([], |row| row.get::<_, BlobId>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    let conflict: Option<BlobId> = sql::optional_scalar(connection, JOURNAL_CONFLICT, [])?;
                    Ok((ids, conflict))
                })
                .await?;
            if ids.is_empty() {
                return conflict.map_or(Ok(()), |id| Err(live_journal_error(id)));
            }
            let backend = catalog.0.backend.clone();
            let (removed, failure) =
                crate::task::run_blocking("blob-reclamation", move || backend.reclaim_batch(ids))
                    .await?
                    .map_err(object_unavailable)?;
            catalog
                .run_upload(move |connection| {
                    let transaction = connection.transaction()?;
                    for id in removed {
                        sql::exec(
                            &transaction,
                            "DELETE FROM pending_blob_deletions WHERE blob_id = ?1 AND NOT EXISTS (SELECT 1 FROM objects WHERE blob_id = ?1)",
                            [&id],
                        )?;
                        sql::exec(
                            &transaction,
                            "DELETE FROM derived_deletions WHERE blob_id = ?1 AND NOT EXISTS (SELECT 1 FROM objects WHERE blob_id = ?1)",
                            [&id],
                        )?;
                    }
                    #[cfg(test)]
                    fault::hit(&fault::database_dir(&transaction), Step::ReclaimForget).map_err(object_unavailable)?;
                    transaction.commit().map_err(object_unavailable)
                })
                .await?;
            failure.map_or_else(
                || conflict.map_or(Ok(()), |id| Err(live_journal_error(id))),
                Err,
            )
        });
        task.await?
    }
}

#[derive(Clone, Copy)]
struct RetirementContext {
    micros: i64,
    origin: super::admin::Origin,
    replacement: Option<i64>,
    notification_mode: NotificationMode,
}

fn retire_previous(
    connection: &Connection,
    previous: Option<&StoredObject>,
    retirement: Retirement,
    bucket: &BucketPublication,
    context: RetirementContext,
) -> Result<Option<BlobId>, ObjectError> {
    let RetirementContext {
        micros,
        origin,
        replacement,
        notification_mode,
    } = context;
    let Some(StoredObject { record, blob: id }) = previous else {
        return Ok(None);
    };
    if origin != super::admin::Origin::Import && notification_mode == NotificationMode::Emit {
        super::notification_outbox::enqueue(
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
    if retirement == Retirement::Noncurrent {
        sql::exec(
            connection,
            "UPDATE objects SET time_deleted = ?2 WHERE blob_id = ?1",
            params![id, micros],
        )?;
        Ok(None)
    } else {
        if let Some(deadline) = bucket.soft_delete_deadline(micros)? {
            sql::exec(
                connection,
                "UPDATE objects SET time_deleted = COALESCE(time_deleted, ?2), soft_delete_time = ?2, hard_delete_time = ?3 WHERE blob_id = ?1",
                params![id, micros, deadline],
            )?;
            return Ok(None);
        }
        sql::exec(connection, "DELETE FROM objects WHERE blob_id = ?1", [id])?;
        sql::exec(
            connection,
            "INSERT INTO pending_blob_deletions VALUES (?1)",
            [id],
        )?;
        Ok(Some(*id))
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
        return Err(if request == RequestKind::Read {
            ObjectError::NotModified
        } else {
            ObjectError::PreconditionFailed
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_fixtures::{PERMISSIVE, bucket, live, permissive_upload};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::UNIX_EPOCH;

    #[cfg(unix)]
    #[test]
    fn publish_refuses_to_replace_an_existing_blob() {
        let data = crate::test_support::TestDir::new();
        fs::create_dir_all(data.0.join("staging")).unwrap();
        let id = BlobId::generate().unwrap();
        let stage = id.staging_path(&data.0);
        let destination = id.blob_path(&data.0);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&stage, b"replacement").unwrap();
        fs::write(&destination, b"live bytes").unwrap();
        assert_eq!(
            publish_blob(&stage, &destination).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&stage).unwrap(), b"replacement");
        assert_eq!(fs::read(&destination).unwrap(), b"live bytes");
    }

    #[tokio::test]
    async fn glob_listing_pages_through_non_matching_rows() {
        for shape in ["items", "prefixes", "versions"] {
            let data = crate::test_support::TestDir::new();
            let catalog = Catalog::open(&data.0).await.unwrap();
            create_bucket(&catalog, "project").await;
            catalog
                .run(|connection| {
                    sql::exec(
                        connection,
                        "UPDATE buckets SET versioning_enabled = 1 WHERE name = 'bucket'",
                        [],
                    )?;
                    Ok::<_, ObjectError>(())
                })
                .await
                .unwrap();
            let mut created = Vec::new();
            for index in 0..10 {
                let name = if index == 9 {
                    "z".into()
                } else if shape == "versions" {
                    "a".into()
                } else {
                    format!("a{index:02}")
                };
                let name = if shape == "prefixes" {
                    format!("{name}/file")
                } else {
                    name
                };
                created.push(put(&catalog, &name, b"body").await);
            }
            let mut query = ObjectListQuery {
                bucket: "bucket".into(),
                delimiter: if shape == "prefixes" {
                    "/".into()
                } else {
                    String::new()
                },
                versions: shape == "versions",
                match_glob: Some(crate::match_glob::Glob::parse("z**").unwrap()),
                ..Default::default()
            };
            let control_query = query.clone();
            let control = catalog
                .run(move |connection| {
                    list_page_within(connection, &control_query, 1000, 0, usize::MAX)
                })
                .await
                .unwrap();
            assert_eq!(control.items.len() + control.prefixes.len(), 1, "{shape}");
            let mut items = Vec::new();
            let mut prefixes = Vec::new();
            let mut pages = 0;
            loop {
                let page_query = query.clone();
                let page = catalog
                    .run(move |connection| list_page_within(connection, &page_query, 1000, 0, 3))
                    .await
                    .unwrap();
                if pages == 0 {
                    assert!(page.items.is_empty() && page.prefixes.is_empty(), "{shape}");
                    let cursor = page.next.as_ref().unwrap();
                    assert_eq!(
                        cursor.name,
                        if shape == "prefixes" {
                            "a02/"
                        } else {
                            &created[2].name
                        }
                    );
                    assert_eq!(
                        cursor.generation,
                        (shape == "versions").then_some(created[2].generation)
                    );
                }
                items.extend(page.items);
                prefixes.extend(page.prefixes);
                pages += 1;
                assert!(pages <= 4, "continuation must advance: {shape}");
                query.cursor = page.next;
                if query.cursor.is_none() {
                    break;
                }
            }
            assert_eq!(pages, 4, "{shape}");
            assert_eq!(items, control.items, "{shape}");
            assert_eq!(prefixes, control.prefixes, "{shape}");
            query.match_glob = None;
            query.cursor = None;
            let page = catalog
                .run(move |connection| list_page_within(connection, &query, 1, 0, 3))
                .await
                .unwrap();
            assert_eq!(page.items.len() + page.prefixes.len(), 1, "{shape}");
            assert!(page.next.is_some(), "{shape}");
            catalog.close().await.unwrap();
        }
    }

    #[test]
    fn acl_update_errors_keep_client_input_and_authorization_status() {
        for (error, expected) in [
            (
                CatalogError::InvalidMetadata("invalid".into()),
                ObjectError::InvalidFixedMetadata("invalid".into()),
            ),
            (
                CatalogError::UnsupportedMetadata("unsupported".into()),
                ObjectError::UnsupportedMetadata("unsupported".into()),
            ),
            (CatalogError::Forbidden, ObjectError::Forbidden),
            (
                CatalogError::PublicAccessPrevention,
                ObjectError::PublicAccessPrevention,
            ),
        ] {
            assert_eq!(acl_update_error(error), expected);
        }
        assert!(matches!(
            acl_update_error(CatalogError::NotFound),
            ObjectError::Unavailable(_)
        ));
    }

    #[tokio::test]
    async fn retention_expiration_matches_between_publication_and_reads() {
        for period in [0, 1, 3_155_760_000] {
            let data = crate::test_support::TestDir::new();
            let catalog = Catalog::open(&data.0).await.unwrap();
            create_bucket(&catalog, "project").await;
            catalog.run(move |connection| {
                sql::exec(connection,
                    "UPDATE buckets SET retention_policy = json_object('retention_period', ?1, 'is_locked', json('false')) WHERE name = 'bucket'", [period])?;
                Ok::<_, ObjectError>(())
            }).await.unwrap();
            let published = put(&catalog, "object", b"body").await;
            let expected = Some(expiration_after(published.time_created, period).unwrap());
            assert_eq!(published.retention_expiration_time, expected);
            let loaded = catalog
                .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
                .await
                .unwrap();
            assert_eq!(loaded.retention_expiration_time, expected);
            let page = catalog
                .list_objects(
                    ObjectListQuery {
                        bucket: "bucket".into(),
                        ..Default::default()
                    },
                    1000,
                    PERMISSIVE,
                )
                .await
                .unwrap();
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.items[0].retention_expiration_time, expected);
            catalog.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn publication_settings_reject_missing_buckets_and_corrupt_autoclass() {
        let data = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        create_bucket(&catalog, "project").await;
        catalog
            .run(|connection| {
                assert!(matches!(
                    BucketPublication::load(connection, "missing"),
                    Err(ObjectError::BucketNotFound)
                ));
                sql::exec(
                    connection,
                    "UPDATE buckets SET autoclass = 'broken' WHERE name = 'bucket'",
                    [],
                )?;
                let error = BucketPublication::load(connection, "bucket").err().unwrap();
                assert!(matches!(error, ObjectError::Corrupt(_)));
                assert!(crate::error::causes(&error).any(|cause| cause.is::<serde_json::Error>()));
                Ok::<_, ObjectError>(())
            })
            .await
            .unwrap();
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn staging_recovery_keeps_entry_context_and_the_io_failure_kind() {
        let data = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        catalog.close().await.unwrap();
        let stage = data.0.join("staging").join("abandoned");
        fs::write(&stage, b"body").unwrap();
        fault::arm(
            &data.0,
            Step::RecoveryStagingUnlink,
            0,
            fault::Fault::Io(io::ErrorKind::PermissionDenied),
        );
        let error = Catalog::open(&data.0).await.err().unwrap();
        assert!(
            matches!(&error, CatalogError::Io(cause) if cause.kind() == io::ErrorKind::PermissionDenied)
        );
        assert!(error.to_string().contains("staging entry"));
        assert!(error.to_string().contains("abandoned"));
        assert!(
            crate::error::causes(&error).any(|error| error.downcast_ref::<io::Error>().is_some())
        );
        assert!(stage.is_file());
        assert_eq!(fault::pending(&data.0), 0);
        let recovered = Catalog::open(&data.0).await.unwrap();
        assert!(!stage.exists());
        recovered.close().await.unwrap();
    }

    #[tokio::test]
    async fn replayed_upload_keeps_its_record_when_discard_fails() {
        let data = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        create_bucket(&catalog, "project").await;
        let mut first = None;
        for replay in [false, true] {
            let mut permit = catalog.admit_object_upload().unwrap();
            catalog
                .check_upload_replay(
                    "bucket",
                    "object",
                    Default::default(),
                    PERMISSIVE,
                    super::super::upload_replay::ReplayInvocation {
                        token: Some("retry"),
                        request: "media-upload",
                    },
                    &mut permit,
                )
                .await
                .unwrap();
            if replay {
                fault::arm(&data.0, Step::DiscardProbe, 0, fault::Fault::Busy);
            }
            let record = catalog
                .put_object_prechecked(
                    permissive_upload("bucket", "object", 1000),
                    body(b"body"),
                    permit,
                )
                .await
                .unwrap();
            if let Some(first) = &first {
                assert_eq!(&record, first);
            }
            first = Some(record);
        }
        assert_eq!(fault::pending(&data.0), 0);
        assert_eq!(fault::catalog_thread_io(&data.0), 0);
        assert_eq!(catalog.0.uploads.lock().unwrap().active, 0);
        catalog.close().await.unwrap();
        let recovered = Catalog::open(&data.0).await.unwrap();
        recovered.close().await.unwrap();
    }

    #[tokio::test]
    async fn skip_mode_publishes_changed_bytes_without_a_discard_job() {
        let data = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        create_bucket(&catalog, "project").await;
        let original = put(&catalog, "object", b"old").await;
        fault::arm(&data.0, Step::DiscardProbe, 0, fault::Fault::Busy);
        let record = catalog
            .put_object_with_mode(
                permissive_upload("bucket", "object", 1000),
                body(b"new"),
                catalog.admit_object_upload().unwrap(),
                super::super::admin::PublishMode::SkipIfSameHash,
                PublishIdentity::Assigned,
                OwnerAssignment::Request,
            )
            .await
            .unwrap();
        assert_ne!(record.generation, original.generation);
        assert_eq!(fault::pending(&data.0), 1);
        assert!(fault::hit(&data.0, Step::DiscardProbe).is_err());
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn invalid_body_keeps_its_error_when_discard_fails() {
        let data = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        create_bucket(&catalog, "project").await;
        for too_large in [false, true] {
            let mut upload =
                permissive_upload("bucket", "object", if too_large { 1 } else { 1000 });
            if !too_large {
                upload.expected.md5 = Some([0; 16]);
            }
            fault::arm(&data.0, Step::DiscardProbe, 0, fault::Fault::Busy);
            let error = catalog
                .put_object_prechecked(
                    upload,
                    body(b"body"),
                    catalog.admit_object_upload().unwrap(),
                )
                .await
                .unwrap_err();
            assert!(if too_large {
                error == ObjectError::TooLarge
            } else {
                matches!(error, ObjectError::HashMismatch(_))
            });
            assert_eq!(fault::pending(&data.0), 0);
        }
        assert_eq!(catalog.0.uploads.lock().unwrap().active, 0);
        catalog.close().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn upload_cleanup_failure_preserves_success_and_precondition_error() {
        let data = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        create_bucket(&catalog, "project").await;
        let original = put(&catalog, "object", b"body").await;
        fault::arm(
            &data.0,
            Step::DiscardProbe,
            0,
            fault::Fault::Io(io::ErrorKind::Other),
        );
        let result = catalog
            .put_object_with_mode(
                permissive_upload("bucket", "object", 1000),
                body(b"body"),
                catalog.admit_object_upload().unwrap(),
                super::super::admin::PublishMode::SkipIfSameHash,
                PublishIdentity::Assigned,
                OwnerAssignment::Request,
            )
            .await
            .unwrap();
        assert_eq!(result, original);
        assert_eq!(fault::pending(&data.0), 0);
        let mut upload = permissive_upload("bucket", "object", 1000);
        upload.conditions.if_generation_match = Some(0);
        fault::arm(
            &data.0,
            Step::DiscardProbe,
            0,
            fault::Fault::Io(io::ErrorKind::Other),
        );
        assert_eq!(
            catalog
                .put_object_prechecked(upload, body(b"new"), catalog.admit_object_upload().unwrap())
                .await,
            Err(ObjectError::PreconditionFailed)
        );
        assert_eq!(fault::pending(&data.0), 0);
        for step in [Step::BodyWrite, Step::BodyFlush, Step::BodySync] {
            fault::arm(
                &data.0,
                step,
                0,
                fault::Fault::Io(io::ErrorKind::StorageFull),
            );
            fault::arm(
                &data.0,
                Step::DiscardProbe,
                0,
                fault::Fault::Io(io::ErrorKind::PermissionDenied),
            );
            let error = catalog
                .put_object_prechecked(
                    permissive_upload("bucket", "object", 1000),
                    body(b"new"),
                    catalog.admit_object_upload().unwrap(),
                )
                .await
                .unwrap_err();
            assert!(
                matches!(error, ObjectError::Io(ref cause) if cause.kind() == io::ErrorKind::StorageFull),
                "{step:?}: {error}"
            );
            assert_eq!(fault::pending(&data.0), 0, "{step:?}");
        }
        assert_eq!(
            catalog
                .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
                .await
                .unwrap(),
            original
        );
        assert_eq!(fault::catalog_thread_io(&data.0), 0);
        catalog.close().await.unwrap();
        let recovered = Catalog::open(&data.0).await.unwrap();
        assert_eq!(fs::read_dir(data.0.join("staging")).unwrap().count(), 0);
        recovered.close().await.unwrap();
    }

    #[tokio::test]
    async fn upload_publication_reads_the_clock_once() {
        struct CountingClock(AtomicUsize);
        impl Clock for CountingClock {
            fn now(&self) -> std::time::SystemTime {
                UNIX_EPOCH
                    + std::time::Duration::from_secs(
                        1 + self.0.fetch_add(1, Ordering::Relaxed) as u64,
                    )
            }
        }
        let data = crate::test_support::TestDir::new();
        let clock = Arc::new(CountingClock(AtomicUsize::new(0)));
        let catalog = Catalog::open_with_clock(&data.0, clock.clone())
            .await
            .unwrap();
        create_bucket(&catalog, "project").await;
        clock.0.store(0, Ordering::Relaxed);
        let record = catalog
            .put_object_prechecked(
                permissive_upload("bucket", "object", 1000),
                body(b"body"),
                catalog.admit_object_upload().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(clock.0.load(Ordering::Relaxed), 1);
        assert_eq!(
            record.time_created,
            UNIX_EPOCH + std::time::Duration::from_secs(1)
        );
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn poisoned_upload_counter_remains_usable() {
        let data = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let other = catalog.clone();
        let _ = std::thread::spawn(move || {
            let _guard = other.0.uploads.lock().unwrap();
            panic!("injected upload counter poison");
        })
        .join();
        drop(catalog.admit_object_upload().unwrap());
        assert!(catalog.is_healthy());
        catalog.close().await.unwrap();
    }

    /// # Safety
    /// `counter` must point to a live, aligned AtomicUsize whenever SQLite can invoke this callback.
    #[expect(
        unsafe_code,
        reason = "Test-only SQLite callbacks avoid enabling the hooks feature in the production dependency; callers keep the counter live."
    )]
    unsafe extern "C" fn count_progress(counter: *mut std::ffi::c_void) -> std::ffi::c_int {
        // SAFETY: The caller guarantees a live, aligned AtomicUsize whenever SQLite can call us.
        let counter = unsafe { &*counter.cast::<AtomicUsize>() };
        i32::from(counter.fetch_add(1, Ordering::Relaxed) >= 100)
    }

    #[test]
    fn publishing_recovers_after_a_shard_creator_panicked() {
        let data = crate::test_support::TestDir::new();
        let _ = std::thread::spawn(|| {
            let _creation = SHARD_CREATION.lock();
            panic!("injected shard creation panic");
        })
        .join();
        let blob = crate::storage::blob_id::BlobId::generate().unwrap();
        let stage = blob.staging_path(&data.0);
        let published = blob.blob_path(&data.0);
        fs::create_dir_all(stage.parent().unwrap()).unwrap();
        fs::create_dir_all(published.parent().unwrap().parent().unwrap()).unwrap();
        fs::write(&stage, b"bytes").unwrap();
        publish_blob(&stage, &published).unwrap();
        assert_eq!(fs::read(&published).unwrap(), b"bytes");
        assert!(!SHARD_CREATION.is_poisoned());
    }

    #[test]
    fn versioning_migration_copies_named_columns_from_permuted_legacy_table() {
        let mut connection = Connection::open_in_memory().unwrap();
        // Catalog validation rejects altered schemas; exercise migration mapping independently.
        let legacy = SCHEMA.replace(
            "bucket TEXT NOT NULL COLLATE BINARY,\n            name TEXT NOT NULL COLLATE BINARY,",
            "name TEXT NOT NULL COLLATE BINARY,\n            bucket TEXT NOT NULL COLLATE BINARY,",
        );
        assert_ne!(legacy, SCHEMA);
        connection.execute_batch(&legacy).unwrap();
        connection.execute_batch(METADATA_SCHEMA).unwrap();
        let columns = "bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,content_encoding,content_disposition,content_language,cache_control,metadata";
        connection.execute_batch(&format!(
            "INSERT INTO objects ({columns}) VALUES ('bucket','object',7,3,'text/plain',5,zeroblob(16),123,'NEARLINE',1234,5678,'opaque-blob','identity','inline','en','private','{{\"team\":\"storage\"}}');"
        )).unwrap();
        let selected = format!("SELECT {columns} FROM objects");
        let read_values = |row: &Row<'_>| {
            (0..17)
                .map(|index| row.get::<_, rusqlite::types::Value>(index))
                .collect::<rusqlite::Result<Vec<_>>>()
        };
        let before = connection.query_row(&selected, [], read_values).unwrap();
        migrate_versioning(&mut connection).unwrap();
        assert_eq!(
            connection.query_row(&selected, [], read_values).unwrap(),
            before
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT time_deleted, event_based_hold FROM objects",
                    [],
                    |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, bool>(1)?))
                )
                .unwrap(),
            (None, false)
        );
    }

    #[tokio::test]
    async fn object_range_reader_seeks_sparse_blob_and_bounds_every_chunk() {
        use std::io::{Seek, Write};

        struct SparseBlob(PathBuf);
        impl Drop for SparseBlob {
            fn drop(&mut self) {
                fs::remove_file(&self.0).unwrap();
            }
        }
        let path = temp_catalog_dir("range-sparse");
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let _blob = SparseBlob(path);
        let size = 64_u64 * 1024 * 1024;
        file.set_len(size).unwrap();
        file.seek(io::SeekFrom::Start(size - 3)).unwrap();
        file.write_all(b"end").unwrap();
        let mut reader = ObjectReader {
            file: ReaderData::Disk(tokio::fs::File::from_std(file)),
            remaining: size,
            position: size - 3,
            publicly_readable: false,
        };
        let offset = size - CHUNK_SIZE as u64 - 3;
        reader
            .set_range(offset, CHUNK_SIZE as u64 + 3)
            .await
            .unwrap();
        assert_eq!(reader.file.stream_position().await.unwrap(), offset);
        let chunk = reader.next_chunk().await.unwrap().unwrap();
        assert_eq!(chunk.len(), CHUNK_SIZE);
        assert!(chunk.iter().all(|byte| *byte == 0));
        assert_eq!(reader.peek_prefix(2).await.unwrap(), b"en"[..]);
        assert_eq!(reader.file.stream_position().await.unwrap(), size - 3);
        assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"end"[..]);
        assert!(reader.next_chunk().await.unwrap().is_none());
        assert_eq!(reader.file.stream_position().await.unwrap(), size);
        reader.set_range(size - 3, 2).await.unwrap();
        assert_eq!(reader.peek_prefix(1).await.unwrap(), b"e"[..]);
        assert_eq!(reader.file.stream_position().await.unwrap(), size - 3);
        assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"en"[..]);
        assert!(reader.next_chunk().await.unwrap().is_none());
        assert_eq!(reader.file.stream_position().await.unwrap(), size - 1);
        reader.set_range(size, 1).await.unwrap();
        assert_eq!(
            reader.next_chunk().await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        drop(reader);
    }

    #[tokio::test]
    async fn upload_reports_a_failed_final_write_instead_of_a_short_blob() {
        struct Blob(PathBuf);
        impl Drop for Blob {
            fn drop(&mut self) {
                fs::remove_file(&self.0).unwrap();
            }
        }
        let blob = Blob(temp_catalog_dir("final-write"));
        let context = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&context.0).await.unwrap();
        fs::write(&blob.0, b"").unwrap();
        // tokio completes a write in the background after write_all returns, so a
        // read-only handle makes exactly that final, unreported write fail.
        let mut file = UploadSink::Disk(tokio::fs::File::open(&blob.0).await.unwrap());
        let body = crate::storage::test_fixtures::body(vec![Bytes::from_static(b"tail")]);
        let (_cancel, cancellation) = oneshot::channel();
        let result = write_stream(
            &mut file,
            ExpectedHashes::default(),
            1024,
            body,
            cancellation,
            &catalog,
        )
        .await;
        assert!(
            matches!(
                result,
                Err(ObjectError::Unavailable(_) | ObjectError::Io(_))
            ),
            "{result:?}"
        );
        assert_eq!(fs::metadata(&blob.0).unwrap().len(), 0);
        catalog.close().await.unwrap();
    }

    #[test]
    fn object_list_seeks_past_large_prefixes_with_bounded_sql_work() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(super::super::BUCKET_SCHEMA)
            .unwrap();
        connection
            .execute_batch(super::super::BUCKET_RETENTION_SCHEMA)
            .unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection.execute_batch("WITH RECURSIVE entries(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM entries WHERE n < 20000)
            INSERT INTO objects SELECT 'bucket', printf('a/%05d', n), n, 1, 'text/plain', 0, zeroblob(16), 0, 'STANDARD', 0, 0, printf('%032x', n) FROM entries;
            INSERT INTO objects VALUES('bucket', 'z', 20001, 1, 'text/plain', 0, zeroblob(16), 0, 'STANDARD', 0, 0, '00000000000000000000000000004e21');").unwrap();
        connection.execute_batch(METADATA_SCHEMA).unwrap();
        connection.execute_batch(VERSIONING_SCHEMA).unwrap();
        connection
            .execute_batch(super::super::derived::SCHEMA)
            .unwrap();
        connection
            .execute_batch(super::super::OBJECT_HOLDS_CUSTOM_TIME_SCHEMA)
            .unwrap();
        connection
            .execute_batch(super::super::SOFT_DELETE_SCHEMA)
            .unwrap();
        connection.execute_batch(retention::SCHEMA).unwrap();
        connection
            .execute_batch(super::super::customer_keys::SCHEMA)
            .unwrap();
        connection
            .execute_batch(super::super::md5_availability::SCHEMA)
            .unwrap();
        connection
            .execute_batch(super::super::object_owner::SCHEMA)
            .unwrap();
        connection
            .execute_batch(retention::SUB_MICRO_SCHEMA)
            .unwrap();
        connection
            .execute_batch(super::super::object_hold::SCHEMA)
            .unwrap();
        let counter = AtomicUsize::new(0);
        // Interrupt after 10,000 VM instructions so a row scan cannot masquerade as a seek.
        #[expect(
            unsafe_code,
            reason = "Install the synchronous test-only progress callback while its counter is live."
        )]
        // SAFETY: This live connection invokes the callback synchronously with the live counter.
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                connection.handle(),
                100,
                Some(count_progress),
                std::ptr::from_ref(&counter).cast_mut().cast(),
            );
        }
        let mut query = ObjectListQuery {
            bucket: "bucket".into(),
            delimiter: "/".into(),
            ..Default::default()
        };
        let page = list_page(&connection, &query, 1).unwrap();
        assert_eq!(page.prefixes, ["a/"]);
        assert!(page.items.is_empty());
        query.cursor = page.next;
        let page = list_page(&connection, &query, 1).unwrap();
        assert_eq!(page.items[0].name, "z");
        assert!(page.next.is_none());
        query.cursor = None;
        query.versions = true;
        let page = list_page(&connection, &query, 1).unwrap();
        assert_eq!(page.prefixes, ["a/"]);
        query.cursor = page.next;
        let page = list_page(&connection, &query, 1).unwrap();
        assert_eq!(page.items[0].name, "z");
        assert!(page.next.is_none());
        #[expect(
            unsafe_code,
            reason = "Unregister the test-only progress callback before its counter is dropped."
        )]
        // SAFETY: Unregister on the live connection before the callback's counter is dropped.
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                connection.handle(),
                0,
                None,
                std::ptr::null_mut(),
            );
        }
        assert!(counter.load(Ordering::Relaxed) < 100);
    }

    #[test]
    fn object_list_skips_noncurrent_rows_with_bounded_sql_work() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(super::super::BUCKET_SCHEMA)
            .unwrap();
        connection
            .execute_batch(super::super::BUCKET_RETENTION_SCHEMA)
            .unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection.execute_batch(METADATA_SCHEMA).unwrap();
        connection.execute_batch(VERSIONING_SCHEMA).unwrap();
        connection
            .execute_batch(super::super::derived::SCHEMA)
            .unwrap();
        connection
            .execute_batch(super::super::OBJECT_HOLDS_CUSTOM_TIME_SCHEMA)
            .unwrap();
        connection
            .execute_batch(super::super::SOFT_DELETE_SCHEMA)
            .unwrap();
        connection.execute_batch(retention::SCHEMA).unwrap();
        connection
            .execute_batch(super::super::customer_keys::SCHEMA)
            .unwrap();
        connection
            .execute_batch(super::super::md5_availability::SCHEMA)
            .unwrap();
        connection
            .execute_batch(super::super::object_owner::SCHEMA)
            .unwrap();
        connection
            .execute_batch(retention::SUB_MICRO_SCHEMA)
            .unwrap();
        connection
            .execute_batch(super::super::object_hold::SCHEMA)
            .unwrap();
        connection.execute_batch("WITH RECURSIVE entries(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM entries WHERE n < 20000)
            INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,time_deleted)
            SELECT 'bucket', CASE WHEN n <= 10000 THEN 'a/history' ELSE printf('y/%05d',n) END, n,1,'text/plain',0,zeroblob(16),0,'STANDARD',0,0,printf('%032x',n),1 FROM entries;
            INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id)
            VALUES ('bucket','c',20001,1,'text/plain',0,zeroblob(16),0,'STANDARD',0,0,printf('%032x',20001)),
            ('bucket','d/file',20002,1,'text/plain',0,zeroblob(16),0,'STANDARD',0,0,printf('%032x',20002)),
            ('bucket','z',20003,1,'text/plain',0,zeroblob(16),0,'STANDARD',0,0,printf('%032x',20003));").unwrap();
        for source in [ListSource::Live, ListSource::Versions] {
            for start in [SeekStart::Inclusive, SeekStart::Exclusive] {
                for upper in [UpperBound::Open, UpperBound::Bounded] {
                    let sql = format!("EXPLAIN QUERY PLAN {}", list_seek_sql(source, start, upper));
                    let mut statement = connection.prepare(&sql).unwrap();
                    let read = |row: &Row<'_>| row.get::<_, String>(3);
                    let plan: Vec<String> = if matches!(upper, UpperBound::Bounded) {
                        statement
                            .query_map(params!["bucket", "", "zz"], read)
                            .unwrap()
                            .collect::<Result<_, _>>()
                            .unwrap()
                    } else {
                        statement
                            .query_map(params!["bucket", ""], read)
                            .unwrap()
                            .collect::<Result<_, _>>()
                            .unwrap()
                    };
                    assert!(
                        plan.iter().any(|step| step.starts_with("SEARCH objects")),
                        "{plan:?}"
                    );
                    assert!(
                        plan.iter().all(|step| !step.contains("TEMP B-TREE")),
                        "{plan:?}"
                    );
                    if matches!(source, ListSource::Live) {
                        assert!(
                            plan.iter()
                                .any(|step| step.contains("USING INDEX objects_live")),
                            "{plan:?}"
                        );
                    }
                }
            }
        }
        let counter = AtomicUsize::new(0);
        // One budget covers multiple pages and history-only prefixes, independent of history size.
        #[expect(
            unsafe_code,
            reason = "Install the synchronous test-only progress callback while its counter is live."
        )]
        // SAFETY: This live connection invokes the callback synchronously with the live counter.
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                connection.handle(),
                100,
                Some(count_progress),
                std::ptr::from_ref(&counter).cast_mut().cast(),
            );
        }
        for delimiter in ["", "/"] {
            let mut query = ObjectListQuery {
                bucket: "bucket".into(),
                delimiter: delimiter.into(),
                ..Default::default()
            };
            for expected in ["c", if delimiter.is_empty() { "d/file" } else { "d/" }, "z"] {
                let page = list_page(&connection, &query, 1).unwrap();
                let names: Vec<_> = page
                    .items
                    .iter()
                    .map(|record| record.name.as_str())
                    .chain(page.prefixes.iter().map(String::as_str))
                    .collect();
                assert_eq!(names, [expected]);
                query.cursor = page.next;
            }
            assert!(query.cursor.is_none());
        }
        let mut query = ObjectListQuery {
            bucket: "bucket".into(),
            delimiter: "/".into(),
            versions: true,
            ..Default::default()
        };
        for expected in ["a/", "c", "d/", "y/", "z"] {
            let page = list_page(&connection, &query, 1).unwrap();
            let names: Vec<_> = page
                .items
                .iter()
                .map(|record| record.name.as_str())
                .chain(page.prefixes.iter().map(String::as_str))
                .collect();
            assert_eq!(names, [expected]);
            query.cursor = page.next;
        }
        query.delimiter.clear();
        query.prefix = "a/".into();
        query.cursor = Some(ObjectListCursor {
            name: "a/history".into(),
            is_prefix: false,
            generation: Some(9999),
        });
        let page = list_page(&connection, &query, 1).unwrap();
        assert_eq!(page.items[0].generation, 10000);
        assert!(page.next.is_none());
        #[expect(
            unsafe_code,
            reason = "Unregister the test-only progress callback before its counter is dropped."
        )]
        // SAFETY: Unregister on the live connection before the callback's counter is dropped.
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                connection.handle(),
                0,
                None,
                std::ptr::null_mut(),
            );
        }
        assert!(counter.load(Ordering::Relaxed) < 100);
    }

    #[test]
    fn metadata_migration_rolls_back_all_columns_and_version_after_late_failure() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection.execute_batch(GENERATION_SCHEMA).unwrap();
        connection.execute_batch("INSERT INTO object_generation VALUES (1,7);
            INSERT INTO objects VALUES ('bucket','object',7,1,'text/plain',3,zeroblob(16),0,'STANDARD',0,0,'a0000000000000000000000000000000');
            ALTER TABLE objects ADD COLUMN metadata TEXT;
            PRAGMA user_version = 3;").unwrap();
        let before: String = connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name='objects'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // The final ALTER fails after four successful statements, exercising transaction rollback.
        assert!(migrate_metadata(&mut connection).is_err());
        let after: String = connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name='objects'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, before);
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        let data: (i64, String, i64) = connection
            .query_row(
                "SELECT generation,content_type,size FROM objects",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(data, (7, "text/plain".into(), 3));
    }

    #[tokio::test]
    async fn prepared_upload_skips_advisory_repeat_but_rechecks_publication() {
        let directory = std::env::temp_dir().join(format!(
            "gcs-mimic-prechecked-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let catalog = Catalog::open(&directory).await.unwrap();
        for checksum_mismatch in [true, false] {
            catalog
                .insert_bucket(bucket("bucket", "project"))
                .await
                .unwrap();
            let conditions = ObjectConditions::default();
            let permit = catalog
                .prepare_object_upload("bucket", "object", conditions, PERMISSIVE)
                .await
                .unwrap();
            catalog
                .delete_bucket("bucket", Default::default(), PERMISSIVE)
                .await
                .unwrap();
            let body = crate::storage::test_fixtures::body(vec![Bytes::from_static(b"new")]);
            let expected = ExpectedHashes {
                md5: checksum_mismatch.then_some([0; 16]),
                crc32c: None,
            };
            let error = catalog
                .put_object_prechecked(
                    ObjectUpload {
                        bucket: "bucket",
                        name: "object",
                        content_type: "text/plain".into(),
                        expected,
                        conditions,
                        max_bytes: 1000,
                        metadata: ObjectMetadataUpdate::default(),
                        access: BucketAccess::Permissive,
                    },
                    body,
                    permit,
                )
                .await
                .unwrap_err();
            if checksum_mismatch {
                assert!(matches!(error, ObjectError::HashMismatch(_)), "{error:?}");
            } else {
                assert_eq!(error, ObjectError::BucketNotFound);
            }
            assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
        }
        catalog.close().await.unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn forbidden_overwrite_is_rechecked_atomically_after_a_concurrent_create() {
        let directory = std::env::temp_dir().join(format!(
            "gcs-mimic-no-overwrite-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let catalog = Catalog::open(&directory).await.unwrap();
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        let upload = |allow_overwrite, bytes: &'static [u8]| {
            let catalog = catalog.clone();
            async move {
                let permit = catalog
                    .prepare_object_upload(
                        "bucket",
                        "object",
                        ObjectConditions::default(),
                        &project_access(allow_overwrite),
                    )
                    .await?;
                let body = crate::storage::test_fixtures::body(vec![Bytes::from_static(bytes)]);
                Ok::<_, ObjectError>((permit, body))
            }
        };
        // The advisory check passes while the object is absent; another writer then creates it.
        let (permit, body) = upload(false, b"late").await.unwrap();
        let (winner_permit, winner_body) = upload(true, b"winner").await.unwrap();
        let publish = |body, permit, allow_overwrite| {
            catalog.put_object_prechecked(
                ObjectUpload {
                    bucket: "bucket",
                    name: "object",
                    content_type: "text/plain".into(),
                    expected: ExpectedHashes::default(),
                    conditions: ObjectConditions::default(),
                    max_bytes: 1000,
                    metadata: ObjectMetadataUpdate::default(),
                    access: project_access(allow_overwrite),
                },
                body,
                permit,
            )
        };
        let winner = publish(winner_body, winner_permit, true).await.unwrap();
        assert_eq!(
            publish(body, permit, false).await.unwrap_err(),
            ObjectError::OverwriteForbidden
        );
        let current = catalog
            .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        assert_eq!(current, winner);
        assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
        // A failing precondition takes precedence: nothing would have been replaced.
        let conditions = ObjectConditions {
            if_generation_match: Some(0),
            ..Default::default()
        };
        let mismatch = ObjectConditions {
            if_generation_match: Some(winner.generation + 1),
            ..Default::default()
        };
        for (conditions, error) in [
            (conditions, ObjectError::PreconditionFailed),
            (mismatch, ObjectError::PreconditionFailed),
            (Default::default(), ObjectError::OverwriteForbidden),
        ] {
            assert_eq!(
                catalog
                    .check_upload("bucket", "object", conditions, &project_access(false))
                    .await
                    .unwrap_err(),
                error
            );
        }
        assert!(
            catalog
                .check_upload(
                    "bucket",
                    "fresh",
                    Default::default(),
                    &project_access(false)
                )
                .await
                .is_ok()
        );
        catalog.close().await.unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    fn project_access(overwrite: bool) -> BucketAccess {
        policy_access("project", Some("project"), overwrite)
    }

    fn policy_access(project: &str, observed: Option<&str>, overwrite: bool) -> BucketAccess {
        use crate::auth::{Auth, Registry};
        let roles = if overwrite {
            vec!["roles/storage.admin"]
        } else {
            vec!["roles/storage.objectViewer", "roles/storage.objectCreator"]
        };
        let auth = Auth::strict(
            Registry::parse(
                serde_json::json!({"identities":[{
                    "id":"local", "tokens":["token"], "project_roles":{project:roles}
                }]})
                .to_string()
                .as_bytes(),
            )
            .unwrap(),
        );
        let mut headers = hyper::HeaderMap::new();
        headers.insert("authorization", "Bearer token".parse().unwrap());
        let identity = auth
            .authenticate(&headers, None)
            .unwrap()
            .unwrap()
            .principal()
            .unwrap();
        BucketAccess::Policy {
            identity: Some(identity.clone()),
            permission: crate::auth::Permission::ObjectsCreate,
            project: observed.map(str::to_owned),
        }
    }

    fn temp_catalog_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gcs-mimic-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    async fn create_bucket(catalog: &Catalog, project: &str) {
        catalog
            .insert_bucket(bucket("bucket", project))
            .await
            .unwrap();
    }

    fn body(bytes: &'static [u8]) -> mpsc::Receiver<Result<UploadChunk, io::Error>> {
        crate::storage::test_fixtures::body(vec![Bytes::from_static(bytes)])
    }

    async fn put(catalog: &Catalog, name: &str, bytes: &'static [u8]) -> ObjectRecord {
        catalog
            .put_object(permissive_upload("bucket", name, 1000), body(bytes))
            .await
            .unwrap()
    }

    async fn publish(
        catalog: &Catalog,
        name: &str,
        permit: UploadGuard,
        access: BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        catalog
            .put_object_prechecked(
                ObjectUpload {
                    bucket: "bucket",
                    name,
                    content_type: "text/plain".into(),
                    expected: ExpectedHashes::default(),
                    conditions: ObjectConditions::default(),
                    max_bytes: 1000,
                    metadata: ObjectMetadataUpdate::default(),
                    access,
                },
                body(b"late"),
                permit,
            )
            .await
    }

    /// Recreate "bucket" under `project` holding one object named "object".
    async fn recreate_bucket(catalog: &Catalog, project: &str) -> ObjectRecord {
        for name in ["object", "fresh"] {
            let _ = catalog
                .delete_object(live("bucket", name), Default::default(), PERMISSIVE)
                .await;
        }
        let _ = catalog
            .delete_bucket("bucket", Default::default(), PERMISSIVE)
            .await;
        create_bucket(catalog, project).await;
        put(catalog, "object", b"other project").await
    }

    /// Every bucket and object operation must fail with `Forbidden` and change nothing.
    async fn assert_all_forbidden(
        catalog: &Catalog,
        access: &BucketAccess,
        current: &ObjectRecord,
    ) {
        let conditions = ObjectConditions::default();
        assert_eq!(
            catalog
                .check_upload("bucket", "object", conditions, access)
                .await,
            Err(ObjectError::Forbidden)
        );
        assert_eq!(
            catalog
                .get_object(live("bucket", "object"), conditions, access)
                .await,
            Err(ObjectError::Forbidden)
        );
        assert_eq!(
            catalog
                .open_object(live("bucket", "object"), conditions, access)
                .await
                .map(|(record, _)| record),
            Err(ObjectError::Forbidden)
        );
        let query = ObjectListQuery {
            bucket: "bucket".into(),
            ..Default::default()
        };
        assert_eq!(
            catalog
                .list_objects(query, 10, access)
                .await
                .map(|page| page.items),
            Err(ObjectError::Forbidden)
        );
        let update = ObjectMetadataUpdate {
            content_type: Some(Some("application/json".into())),
            ..Default::default()
        };
        assert_eq!(
            catalog
                .update_object_metadata(
                    live("bucket", "object"),
                    conditions,
                    update,
                    |_| Ok(()),
                    access,
                )
                .await,
            Err(ObjectError::Forbidden)
        );
        assert_eq!(
            catalog
                .delete_object(live("bucket", "object"), conditions, access)
                .await,
            Err(ObjectError::Forbidden)
        );
        assert_eq!(
            catalog.get_bucket("bucket", access).await,
            Err(CatalogError::Forbidden)
        );
        // Authorization precedes the non-empty check, so a non-empty bucket also reports 403.
        assert_eq!(
            catalog
                .delete_bucket("bucket", Default::default(), access)
                .await,
            Err(CatalogError::Forbidden)
        );
        assert_eq!(
            catalog
                .get_object(live("bucket", "object"), conditions, PERMISSIVE)
                .await
                .as_ref(),
            Ok(current)
        );
        assert!(
            catalog
                .get_bucket("bucket", PERMISSIVE)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn pap_is_rechecked_after_upload_admission_and_before_reads() {
        use crate::auth::{Binding, OBJECTS_CREATE, OBJECTS_GET, Policy};
        for parent in [false, true] {
            let directory = temp_catalog_dir("pap-recheck");
            let catalog = Catalog::open(&directory).await.unwrap();
            create_bucket(&catalog, "project").await;
            let current = put(&catalog, "object", b"original").await;
            let policy = Policy {
                bindings: vec![Binding {
                    role: "roles/storage.admin".into(),
                    members: vec!["allUsers".into()],
                }],
                etag: None,
            };
            let stored = catalog
                .set_policy("bucket", policy, PERMISSIVE)
                .await
                .unwrap();
            let read = catalog
                .bucket_access("bucket", None, OBJECTS_GET)
                .await
                .unwrap();
            let create = catalog
                .bucket_access("bucket", None, OBJECTS_CREATE)
                .await
                .unwrap();
            let permit = catalog
                .prepare_object_upload("bucket", "fresh", Default::default(), &create)
                .await
                .unwrap();
            if parent {
                catalog
                    .configure_public_access_prevention(&["project".into(), "project".into()])
                    .await
                    .unwrap();
            } else {
                catalog
                    .patch_bucket(
                        "bucket",
                        crate::bucket::BucketMetadataUpdate {
                            public_access_prevention_enforced: Some(true),
                            ..Default::default()
                        },
                        Default::default(),
                        PERMISSIVE,
                    )
                    .await
                    .unwrap();
            }
            assert_all_forbidden(&catalog, &read, &current).await;
            assert_eq!(
                publish(&catalog, "fresh", permit, create).await,
                Err(ObjectError::Forbidden)
            );
            assert_eq!(
                catalog.get_policy("bucket", PERMISSIVE).await.unwrap(),
                stored
            );
            assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
            catalog.close().await.unwrap();
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test]
    async fn object_acl_revocation_rechecks_admitted_reads_and_keeps_opened_snapshot() {
        use crate::{
            acl::AclTarget,
            auth::{Auth, OBJECTS_GET, Registry},
            storage::catalog::acl::AclChange,
        };
        for memory in [false, true] {
            let directory = temp_catalog_dir("revoked-object-acl");
            let catalog = if memory {
                Catalog::open_memory_for_server(
                    1024 * 1024,
                    4 * 1024 * 1024,
                    Arc::new(crate::clock::SystemClock),
                    crate::storage::admin::LocalAdmin::Disabled,
                )
                .await
                .unwrap()
            } else {
                Catalog::open(&directory).await.unwrap()
            };
            create_bucket(&catalog, "project").await;
            let current = put(&catalog, "object", b"selected snapshot").await;
            let auth = Auth::strict(
                Registry::parse(br#"{"identities":[{"id":"reader","tokens":["token"]}]}"#).unwrap(),
            );
            let mut headers = hyper::HeaderMap::new();
            headers.insert("authorization", "Bearer token".parse().unwrap());
            let identity = auth
                .authenticate(&headers, None)
                .unwrap()
                .unwrap()
                .principal()
                .unwrap();
            let access = catalog
                .bucket_access("bucket", Some(identity), OBJECTS_GET)
                .await
                .unwrap();
            assert_eq!(
                catalog
                    .get_object(live("bucket", "object"), Default::default(), &access)
                    .await,
                Err(ObjectError::Forbidden)
            );
            let target = AclTarget::Object {
                bucket: "bucket".into(),
                name: "object".into(),
                generation: Some(current.generation),
            };
            catalog
                .change_acl(
                    target.clone(),
                    AclChange::Upsert {
                        entity: "user-reader".into(),
                        role: Some(crate::acl::AclRole::Reader),
                    },
                    PERMISSIVE,
                )
                .await
                .unwrap();
            let (_, reader) = catalog
                .open_object(live("bucket", "object"), Default::default(), &access)
                .await
                .unwrap();
            let revoke = catalog.change_acl(
                target,
                AclChange::Delete {
                    entity: "user-reader".into(),
                },
                PERMISSIVE,
            );
            let read = catalog.get_object(live("bucket", "object"), Default::default(), &access);
            let (revoked, concurrent) = tokio::join!(revoke, read);
            revoked.unwrap();
            assert!(
                concurrent
                    .as_ref()
                    .is_ok_and(|record| record.generation == current.generation)
                    || concurrent == Err(ObjectError::Forbidden)
            );
            assert_eq!(
                catalog
                    .get_object(
                        live("bucket", "object"),
                        ObjectConditions {
                            if_generation_match: Some(0),
                            ..Default::default()
                        },
                        &access
                    )
                    .await,
                Err(ObjectError::Forbidden)
            );
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut reader.into_reader().await, &mut bytes).unwrap();
            assert_eq!(bytes, b"selected snapshot");
            catalog.close().await.unwrap();
            if !memory {
                fs::remove_dir_all(directory).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn revoked_bucket_binding_is_rechecked_inside_operations() {
        use crate::auth::{
            Auth, BUCKETS_GET_IAM, BUCKETS_SET_IAM, Binding, OBJECTS_CREATE, OBJECTS_GET, Policy,
            Registry,
        };
        for public in [false, true] {
            let directory = temp_catalog_dir("revoked-policy");
            let catalog = Catalog::open(&directory).await.unwrap();
            create_bucket(&catalog, "project").await;
            let current = put(&catalog, "object", b"original").await;
            let auth = Auth::strict(
                Registry::parse(br#"{"identities":[{"id":"local", "tokens":["token"]}]}"#).unwrap(),
            );
            let mut headers = hyper::HeaderMap::new();
            headers.insert("authorization", "Bearer token".parse().unwrap());
            let identity = auth
                .authenticate(&headers, None)
                .unwrap()
                .unwrap()
                .principal()
                .unwrap();
            let identity = if public { None } else { Some(identity) };
            let granted = Policy {
                bindings: vec![Binding {
                    role: "roles/storage.admin".into(),
                    members: vec![if public { "allUsers" } else { "user:local" }.into()],
                }],
                etag: None,
            };
            catalog
                .set_policy("bucket", granted, PERMISSIVE)
                .await
                .unwrap();
            let read = catalog
                .bucket_access("bucket", identity, OBJECTS_GET)
                .await
                .unwrap();
            let create = catalog
                .bucket_access("bucket", identity, OBJECTS_CREATE)
                .await
                .unwrap();
            let get_iam = catalog
                .bucket_access("bucket", identity, BUCKETS_GET_IAM)
                .await
                .unwrap();
            let set_iam = catalog
                .bucket_access("bucket", identity, BUCKETS_SET_IAM)
                .await
                .unwrap();
            let permit = catalog
                .prepare_object_upload("bucket", "fresh", Default::default(), &create)
                .await
                .unwrap();
            // These accesses passed advisory authorization; transaction rechecks must observe
            // revocation before selecting a blob or publishing an upload.
            catalog
                .set_policy(
                    "bucket",
                    Policy {
                        bindings: vec![],
                        etag: None,
                    },
                    PERMISSIVE,
                )
                .await
                .unwrap();
            assert_all_forbidden(&catalog, &read, &current).await;
            assert_eq!(
                catalog.get_policy("bucket", &get_iam).await,
                Err(CatalogError::Forbidden)
            );
            assert_eq!(
                catalog
                    .set_policy(
                        "bucket",
                        Policy {
                            bindings: vec![],
                            etag: None
                        },
                        &set_iam
                    )
                    .await,
                Err(CatalogError::Forbidden)
            );
            assert_eq!(
                publish(&catalog, "fresh", permit, create).await,
                Err(ObjectError::Forbidden)
            );
            assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
            catalog.close().await.unwrap();
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test]
    async fn operations_recheck_the_authorized_project_after_bucket_recreation() {
        let directory = temp_catalog_dir("recreated-bucket");
        let catalog = Catalog::open(&directory).await.unwrap();
        create_bucket(&catalog, "project").await;
        let access = project_access(true);
        // The advisory check passes in the authorized project; the bucket is then deleted and
        // recreated under another project before the body completes.
        let permit = catalog
            .prepare_object_upload("bucket", "object", Default::default(), &access)
            .await
            .unwrap();
        let current = recreate_bucket(&catalog, "other-project").await;
        assert_eq!(
            publish(&catalog, "object", permit, access.clone()).await,
            Err(ObjectError::Forbidden)
        );
        let permit = catalog.admit_object_upload().unwrap();
        assert_eq!(
            publish(&catalog, "fresh", permit, access.clone()).await,
            Err(ObjectError::Forbidden)
        );
        assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
        assert_all_forbidden(&catalog, &access, &current).await;
        let page = catalog
            .list_objects(
                ObjectListQuery {
                    bucket: "bucket".into(),
                    ..Default::default()
                },
                10,
                PERMISSIVE,
            )
            .await
            .unwrap();
        assert_eq!(page.items, vec![current.clone()]);
        // An access authorized against the bucket's actual project still succeeds.
        let actual = policy_access("other-project", Some("other-project"), false);
        assert_eq!(
            catalog
                .get_object(live("bucket", "object"), Default::default(), &actual)
                .await,
            Ok(current)
        );
        catalog.close().await.unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn missing_bucket_access_reauthorizes_against_the_actual_project() {
        let directory = temp_catalog_dir("missing-bucket-access");
        let catalog = Catalog::open(&directory).await.unwrap();
        let BucketAccess::Policy {
            identity,
            permission,
            ..
        } = policy_access("project", None, false)
        else {
            unreachable!()
        };
        let access = catalog
            .bucket_access("bucket", identity.as_ref(), permission.as_str())
            .await
            .unwrap();
        let conditions = ObjectConditions::default();
        // Still missing: the caller holds the permission somewhere, so it learns 404.
        assert_eq!(
            catalog
                .check_upload("bucket", "object", conditions, &access)
                .await,
            Err(ObjectError::BucketNotFound)
        );
        assert_eq!(
            catalog
                .get_object(live("bucket", "object"), conditions, &access)
                .await,
            Err(ObjectError::BucketNotFound)
        );
        assert_eq!(
            catalog
                .delete_object(live("bucket", "object"), conditions, &access)
                .await,
            Err(ObjectError::BucketNotFound)
        );
        assert_eq!(catalog.get_bucket("bucket", &access).await, Ok(None));
        assert_eq!(
            catalog
                .delete_bucket("bucket", Default::default(), &access)
                .await,
            Err(CatalogError::NotFound)
        );
        // Created later in a project where the caller lacks the permission.
        create_bucket(&catalog, "project").await;
        let permit = catalog
            .prepare_object_upload("bucket", "object", conditions, &access)
            .await
            .unwrap();
        let current = recreate_bucket(&catalog, "other-project").await;
        assert_eq!(
            publish(&catalog, "object", permit, access.clone()).await,
            Err(ObjectError::Forbidden)
        );
        assert_all_forbidden(&catalog, &access, &current).await;
        // Created later in a permitted project: overwriting still needs storage.objects.delete
        // there and never defaults to allowed.
        recreate_bucket(&catalog, "other-project").await;
        let current = recreate_bucket(&catalog, "project").await;
        let permit = catalog.admit_object_upload().unwrap();
        assert_eq!(
            publish(&catalog, "object", permit, access.clone()).await,
            Err(ObjectError::OverwriteForbidden)
        );
        assert_eq!(
            catalog
                .get_object(live("bucket", "object"), conditions, &access)
                .await,
            Ok(current)
        );
        let permit = catalog.admit_object_upload().unwrap();
        assert!(publish(&catalog, "fresh", permit, access).await.is_ok());
        let deleter = policy_access("project", None, true);
        let permit = catalog.admit_object_upload().unwrap();
        assert!(publish(&catalog, "object", permit, deleter).await.is_ok());
        assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_v1_migration_never_commits_an_unreopenable_v2_catalog() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(super::super::BUCKET_SCHEMA)
            .unwrap();
        // Force the second DDL to fail after the object tables and v2 marker were written.
        connection.execute_batch(GENERATION_SCHEMA).unwrap();
        assert!(migrate(&mut connection).is_err());
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        let objects: i64 = connection.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('objects', 'pending_blob_deletions')", [], |row| row.get(0)).unwrap();
        assert_eq!(objects, 0);
    }
}
