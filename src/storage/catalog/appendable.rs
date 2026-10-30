//! Rapid appendable objects.
//!
//! An unfinalized appendable generation is a mutable state machine, so it lives outside the
//! immutable `objects` table: one row per object name plus one growing data file. The row is
//! the durability boundary. Bytes past its `size` are unacknowledged and are truncated by the
//! next writer; readers never look past it. Finalization marks the row `finalizing`, streams
//! the persisted bytes through ordinary publication with the generation reserved at creation,
//! and that publication's transaction removes the row. Catalogs written before that change can
//! still hold a row whose generation was published, so startup recovery resolves every
//! `finalizing` row before any request can delete the published generation: a published
//! generation retires the row, otherwise the row becomes appendable again. At runtime a row
//! whose generation is published is always treated as finalized.

use super::{
    Catalog,
    objects::{
        ObjectReader, ObjectUpload, authorized_bucket, check_conditions, lookup,
        object_unavailable, unlink,
    },
    resumable::{ResumableOptions, decode_options, encode_options},
    system_time_micros, unix_micros,
};
use crate::{
    bucket::{BucketAccess, CatalogError},
    object::{ObjectConditions, ObjectError, ObjectRecord, UploadChunk},
    storage::blob_id::BlobId,
};
use bytes::Bytes;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{
    collections::HashMap,
    fmt, fs,
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::SystemTime,
};
use tokio::sync::mpsc;

pub(super) const SCHEMA: &str = "CREATE TABLE appendable_objects (
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND 1024),
    generation INTEGER NOT NULL CHECK(generation > 0),
    incarnation TEXT NOT NULL CHECK(length(incarnation) = 32),
    blob_id TEXT NOT NULL UNIQUE CHECK(length(blob_id) = 32),
    options TEXT NOT NULL CHECK(length(CAST(options AS BLOB)) <= 65536),
    size INTEGER NOT NULL CHECK(size >= 0),
    crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
    time_created INTEGER NOT NULL,
    updated INTEGER NOT NULL,
    write_handle TEXT NOT NULL CHECK(length(write_handle) = 32),
    finalizing INTEGER NOT NULL DEFAULT 0 CHECK(finalizing IN (0, 1)),
    PRIMARY KEY (bucket, name)
) WITHOUT ROWID;
PRAGMA user_version = 34;";

const COPY_CHUNK: usize = 64 * 1024;

/// Runs at catalog open, before requests: resolves interrupted finalizations, rows of deleted
/// buckets and data files that never received a row.
pub(super) fn recover(connection: &Connection, data_dir: &Path) -> Result<(), CatalogError> {
    let retired = connection
        .prepare_cached(
            "SELECT a.blob_id FROM appendable_objects a WHERE EXISTS(SELECT 1 FROM objects o WHERE o.bucket = a.bucket AND o.name = a.name AND o.generation = a.generation) OR NOT EXISTS(SELECT 1 FROM bucket_incarnations i WHERE i.bucket = a.bucket AND i.incarnation = a.incarnation)",
        )
        .map_err(super::unavailable)?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(super::unavailable)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(super::unavailable)?;
    for id in retired {
        connection
            // xtask-lint: allow(text::uncached_statement) -- retired rows are removed only during open-time recovery
            .execute("DELETE FROM appendable_objects WHERE blob_id = ?1", [&id])
            .map_err(super::unavailable)?;
        unlink(&data_path(data_dir, &id).map_err(super::unavailable)?)
            .map_err(super::unavailable)?;
    }
    // Startup takes no write lock unless there is work: an UPDATE matching nothing still would.
    let interrupted: bool = connection
        // xtask-lint: allow(text::uncached_statement) -- interrupted finalizations are probed once at open
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM appendable_objects WHERE finalizing = 1)",
            [],
            |row| row.get(0),
        )
        .map_err(super::unavailable)?;
    if interrupted {
        connection
            // xtask-lint: allow(text::uncached_statement) -- interrupted finalizations are reset once at open
            .execute(
                "UPDATE appendable_objects SET finalizing = 0 WHERE finalizing = 1",
                [],
            )
            .map_err(super::unavailable)?;
    }
    let directory = data_dir.join("appendable");
    if !directory.try_exists().map_err(super::unavailable)? {
        return Ok(());
    }
    for entry in fs::read_dir(&directory).map_err(super::unavailable)? {
        let entry = entry.map_err(super::unavailable)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // A creation that crashed before its row committed leaves an unreferenced file.
        if BlobId::parse(&name).is_none() {
            continue;
        }
        let referenced: bool = connection
            // xtask-lint: allow(text::uncached_statement) -- orphaned data files are checked only during open-time recovery
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM appendable_objects WHERE blob_id = ?1)",
                [&name],
                |row| row.get(0),
            )
            .map_err(super::unavailable)?;
        if !referenced {
            unlink(&entry.path()).map_err(super::unavailable)?;
        }
    }
    Ok(())
}

pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(super::unavailable)?;
    transaction
        .execute_batch(SCHEMA)
        .map_err(super::unavailable)?;
    transaction.commit().map_err(super::unavailable)
}

/// Whether finalization of an appendable generation started or was abandoned before publishing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Finalizing {
    Started,
    Abandoned,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AppendError {
    Object(ObjectError),
    /// Another writer took over, or the object was finalized or deleted, after this writer opened.
    Superseded,
    /// The selected generation exists only as a finalized object.
    Finalized,
    /// Replacing an existing live or appendable object with a new appendable generation.
    ReplaceUnsupported,
    /// A write offset leaves a gap after the bytes this writer has received.
    OffsetGap {
        expected: u64,
    },
}

impl fmt::Display for AppendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Object(error) => error.fmt(f),
            Self::Superseded => f.write_str("appendable writer was superseded"),
            Self::Finalized => f.write_str("appendable object is finalized"),
            Self::ReplaceUnsupported => f.write_str("replacing an existing object"),
            Self::OffsetGap { expected } => write!(f, "write offset exceeds {expected}"),
        }
    }
}

impl From<ObjectError> for AppendError {
    fn from(error: ObjectError) -> Self {
        Self::Object(error)
    }
}

struct Row {
    generation: i64,
    blob_id: String,
    options: ResumableOptions,
    size: u64,
    crc32c: u32,
    time_created: SystemTime,
    updated: SystemTime,
    write_handle: String,
}

/// The single in-process owner of each appendable object's data file.
#[derive(Default)]
struct Slot {
    owner: Option<String>,
}

type SlotKey = (PathBuf, String, String);

struct SlotEntry {
    /// Live `SlotHandle`s; counted under the map lock, unlike `Arc::strong_count`, whose
    /// decrement in a dropped handle would otherwise race with another handle's check.
    holders: usize,
    slot: Arc<tokio::sync::Mutex<Slot>>,
}

/// Process-local because file descriptors are; the row's write handle remains authoritative
/// across restarts, when no writer survives.
fn slots() -> &'static Mutex<HashMap<SlotKey, SlotEntry>> {
    static SLOTS: OnceLock<Mutex<HashMap<SlotKey, SlotEntry>>> = OnceLock::new();
    SLOTS.get_or_init(Mutex::default)
}

/// Every in-process user of one appendable object's slot, writers included, holds a handle.
/// The last handle to drop removes the entry, so the map holds only objects in use, and
/// every concurrent holder of a key shares one slot.
struct SlotHandle {
    key: SlotKey,
    slot: Arc<tokio::sync::Mutex<Slot>>,
}

impl SlotHandle {
    fn new(data_dir: &Path, bucket: &str, name: &str) -> Self {
        let key = (data_dir.to_owned(), bucket.to_owned(), name.to_owned());
        // The map only counts holders, and each update completes before the lock is
        // released, so a panic elsewhere cannot leave it inconsistent.
        let mut slots = slots().lock().unwrap_or_else(PoisonError::into_inner);
        let entry = slots.entry(key.clone()).or_insert_with(|| SlotEntry {
            holders: 0,
            slot: Arc::default(),
        });
        entry.holders += 1;
        let slot = entry.slot.clone();
        Self { key, slot }
    }
}

impl std::ops::Deref for SlotHandle {
    type Target = tokio::sync::Mutex<Slot>;

    fn deref(&self) -> &Self::Target {
        &self.slot
    }
}

impl Drop for SlotHandle {
    fn drop(&mut self) {
        let mut slots = slots().lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = slots.get_mut(&self.key) {
            entry.holders = entry.holders.saturating_sub(1);
            if entry.holders == 0 {
                slots.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
fn slot_count(data_dir: &Path) -> usize {
    slots()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .keys()
        .filter(|(dir, _, _)| dir == data_dir)
        .count()
}

fn data_path(data_dir: &Path, blob_id: &str) -> Result<PathBuf, ObjectError> {
    BlobId::parse(blob_id)
        .map(|_| data_dir.join("appendable").join(blob_id))
        .ok_or_else(|| ObjectError::Corrupt("invalid appendable blob ID".into()))
}

fn incarnation(connection: &Connection, bucket: &str) -> Result<Option<String>, ObjectError> {
    connection
        .prepare_cached("SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1")
        .and_then(|mut statement| statement.query_row([bucket], |row| row.get(0)))
        .optional()
        .map_err(object_unavailable)
}

enum Lookup {
    Absent,
    /// A row from a deleted bucket incarnation or an already published generation.
    Stale(String),
    Live(Box<Row>),
}

fn select(connection: &Connection, bucket: &str, name: &str) -> Result<Lookup, ObjectError> {
    let row = connection
        .prepare_cached(
            "SELECT generation, incarnation, blob_id, options, size, crc32c, time_created, updated, write_handle FROM appendable_objects WHERE bucket = ?1 AND name = ?2",
        )
        .and_then(|mut statement| {
            statement.query_row(params![bucket, name], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, u32>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                ))
            })
        })
        .optional()
        .map_err(object_unavailable)?;
    let Some((generation, stored, blob_id, options, size, crc32c, created, updated, handle)) = row
    else {
        return Ok(Lookup::Absent);
    };
    if published_generation(connection, bucket, name, generation)?
        || incarnation(connection, bucket)?.as_deref() != Some(stored.as_str())
    {
        return Ok(Lookup::Stale(blob_id));
    }
    let options = serde_json::from_str(&options)
        .map_err(object_unavailable)
        .and_then(|value| {
            decode_options(&value).map_err(|error| {
                ObjectError::Corrupt(crate::storage::failure::BackendCause::from_error(error))
            })
        })?;
    Ok(Lookup::Live(Box::new(Row {
        generation,
        blob_id,
        options,
        size: u64::try_from(size).map_err(object_unavailable)?,
        crc32c,
        time_created: system_time_micros(created).map_err(object_unavailable)?,
        updated: system_time_micros(updated).map_err(object_unavailable)?,
        write_handle: handle,
    })))
}

fn published_generation(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: i64,
) -> Result<bool, ObjectError> {
    connection
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3)",
        )
        .and_then(|mut statement| {
            statement.query_row(params![bucket, name, generation], |row| row.get(0))
        })
        .map_err(object_unavailable)
}

/// The adapter-visible view of an unfinalized generation. It has no MD5 and no finalize time.
/// The storage class is resolved as finalization's publication will resolve it, including
/// Autoclass.
fn record(
    connection: &Connection,
    bucket: &str,
    name: &str,
    bucket_class: String,
    row: &Row,
) -> Result<ObjectRecord, ObjectError> {
    let storage_class = super::bucket_autoclass::object_storage_class(
        connection,
        bucket,
        row.options.metadata.storage_class.clone(),
        bucket_class,
    )
    .map_err(object_unavailable)?;
    let mut record = ObjectRecord {
        bucket: bucket.to_owned(),
        name: name.to_owned(),
        generation: row.generation,
        metageneration: 1,
        content_type: row.options.content_type.clone(),
        content_encoding: None,
        content_disposition: None,
        content_language: None,
        cache_control: None,
        metadata: Default::default(),
        size: row.size,
        md5: [0; 16],
        crc32c: row.crc32c,
        component_count: None,
        storage_class,
        time_created: row.time_created,
        updated: row.updated,
        time_deleted: None,
        soft_delete_time: None,
        hard_delete_time: None,
        event_based_hold: false,
        temporary_hold: false,
        custom_time: None,
        retention_expiration_time: None,
        // Creation rejects object retention and customer-supplied keys.
        retention: None,
        customer_key_sha256: None,
    };
    row.options.metadata.clone().apply(&mut record)?;
    Ok(record)
}

/// An exclusive writer for one appendable generation.
pub(crate) struct AppendWriter {
    catalog: Catalog,
    bucket: String,
    name: String,
    path: PathBuf,
    handle: String,
    /// Keeps the slot, and so this writer's ownership claim, registered while it lives.
    slot: SlotHandle,
    record: ObjectRecord,
    /// Bytes received on this writer, including those not yet flushed.
    end: u64,
    crc32c: u32,
}

impl AppendWriter {
    pub(crate) fn record(&self) -> &ObjectRecord {
        &self.record
    }

    pub(crate) fn persisted_size(&self) -> u64 {
        self.record.size
    }

    pub(crate) fn write_handle(&self) -> &[u8] {
        self.handle.as_bytes()
    }

    fn slot(&self) -> Arc<tokio::sync::Mutex<Slot>> {
        self.slot.slot.clone()
    }

    /// Writes bytes at `offset`; bytes this writer already received are skipped.
    pub(crate) async fn append(
        &mut self,
        offset: u64,
        data: Bytes,
        max_bytes: u64,
    ) -> Result<(), AppendError> {
        if offset > self.end {
            return Err(AppendError::OffsetGap { expected: self.end });
        }
        let skip = usize::try_from(self.end - offset)
            .unwrap_or(usize::MAX)
            .min(data.len());
        let added = data.slice(skip..);
        if added.is_empty() {
            return Ok(());
        }
        let end = self
            .end
            .checked_add(added.len() as u64)
            .filter(|end| *end <= max_bytes)
            .ok_or(ObjectError::TooLarge)?;
        let slot = self.slot();
        let guard = slot.lock().await;
        if guard.owner.as_deref() != Some(self.handle.as_str()) {
            return Err(AppendError::Superseded);
        }
        let path = self.path.clone();
        let start = self.end;
        let bytes = added.clone();
        tokio::task::spawn_blocking(move || -> io::Result<()> {
            let mut file = fs::OpenOptions::new().write(true).open(path)?;
            file.seek(io::SeekFrom::Start(start))?;
            file.write_all(&bytes)
        })
        .await
        .map_err(object_unavailable)?
        .map_err(object_unavailable)?;
        drop(guard);
        self.crc32c = crc32c::crc32c_append(self.crc32c, &added);
        self.end = end;
        Ok(())
    }

    /// Makes every received byte durable, then publishes the new size to readers.
    pub(crate) async fn flush(&mut self) -> Result<u64, AppendError> {
        let slot = self.slot();
        let guard = slot.lock().await;
        if guard.owner.as_deref() != Some(self.handle.as_str()) {
            return Err(AppendError::Superseded);
        }
        if self.end == self.record.size {
            return Ok(self.end);
        }
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            fs::OpenOptions::new().write(true).open(path)?.sync_data()
        })
        .await
        .map_err(object_unavailable)?
        .map_err(object_unavailable)?;
        let (bucket, name, generation, handle) = (
            self.bucket.clone(),
            self.name.clone(),
            self.record.generation,
            self.handle.clone(),
        );
        let (size, crc32c) = (
            i64::try_from(self.end).map_err(object_unavailable)?,
            self.crc32c,
        );
        let clock = self.catalog.0.clock.clone();
        let updated = self
            .catalog
            .run_upload(move |connection| {
                let micros = unix_micros(clock.now()).map_err(object_unavailable)?;
                let changed = connection
                    .prepare_cached(
                        // A deleted or recreated bucket invalidates the writer as well.
                        "UPDATE appendable_objects SET size = ?1, crc32c = ?2, updated = ?3 WHERE bucket = ?4 AND name = ?5 AND generation = ?6 AND write_handle = ?7 AND incarnation = (SELECT incarnation FROM bucket_incarnations WHERE bucket = ?4)",
                    )
                    .and_then(|mut statement| {
                        statement.execute(params![size, crc32c, micros, bucket, name, generation, handle])
                    })
                    .map_err(object_unavailable)?;
                Ok((changed == 1).then_some(micros))
            })
            .await?;
        drop(guard);
        let micros = updated.ok_or(AppendError::Superseded)?;
        self.record.size = self.end;
        self.record.crc32c = self.crc32c;
        self.record.updated = system_time_micros(micros).map_err(object_unavailable)?;
        Ok(self.end)
    }

    /// Publishes the persisted bytes as an ordinary immutable generation with the reserved
    /// generation number, then retires the appendable state.
    pub(crate) async fn finalize(
        &mut self,
        expected_crc32c: Option<u32>,
        access: BucketAccess,
        max_bytes: u64,
    ) -> Result<ObjectRecord, AppendError> {
        self.flush().await?;
        if expected_crc32c.is_some_and(|crc| crc != self.crc32c) {
            return Err(ObjectError::ChecksumMismatch.into());
        }
        let slot = self.slot();
        let mut guard = slot.lock().await;
        if guard.owner.as_deref() != Some(self.handle.as_str()) {
            return Err(AppendError::Superseded);
        }
        let catalog = self.catalog.clone();
        let identity = super::admin::ImportedIdentity::new(
            super::admin::Generation::new(self.record.generation).map_err(object_unavailable)?,
            self.record.time_created,
            self.record.updated,
        )
        .map_err(object_unavailable)?;
        let permit = catalog.admit_object_upload()?;
        let (tx, rx) = mpsc::channel(2);
        let path = self.path.clone();
        let size = self.record.size;
        // Bounded copy: one 64 KiB chunk is read per send, never the whole object.
        let producer = async move {
            let mut file = tokio::task::spawn_blocking(move || fs::File::open(path))
                .await
                .map_err(io::Error::other)??;
            let mut remaining = size;
            while remaining > 0 {
                let length = remaining.min(COPY_CHUNK as u64) as usize;
                let (returned, chunk) = tokio::task::spawn_blocking(move || {
                    let mut buffer = vec![0; length];
                    let result = file.read_exact(&mut buffer).map(|()| buffer);
                    (file, result)
                })
                .await
                .map_err(io::Error::other)?;
                file = returned;
                let chunk = chunk?;
                remaining -= chunk.len() as u64;
                if tx
                    .send(Ok(UploadChunk::Data(Bytes::from(chunk))))
                    .await
                    .is_err()
                {
                    return Ok(());
                }
            }
            let _ = tx.send(Ok(UploadChunk::End)).await;
            Ok::<(), io::Error>(())
        };
        let publish = catalog.put_object_with_mode(
            ObjectUpload {
                bucket: &self.bucket,
                name: &self.name,
                content_type: self.record.content_type.clone(),
                expected: Default::default(),
                // Ordinary writes may have created the name since the appendable generation began.
                conditions: ObjectConditions {
                    if_generation_match: Some(0),
                    ..Default::default()
                },
                max_bytes,
                metadata: self.stored_metadata().await?,
                access,
            },
            rx,
            permit,
            super::admin::PublishMode::Replace,
            Some(identity),
        );
        self.mark_finalizing(Finalizing::Started).await?;
        let (copied, published) = tokio::join!(producer, publish);
        let published = match published {
            Ok(published) => published,
            Err(error) => {
                // Nothing was published, so the generation stays appendable.
                self.mark_finalizing(Finalizing::Abandoned).await?;
                return Err(error.into());
            }
        };
        copied.map_err(object_unavailable)?;
        guard.owner = None;
        self.retire().await;
        drop(guard);
        Ok(published)
    }

    async fn mark_finalizing(&self, state: Finalizing) -> Result<(), AppendError> {
        let finalizing = state == Finalizing::Started;
        let (bucket, name, generation, handle) = (
            self.bucket.clone(),
            self.name.clone(),
            self.record.generation,
            self.handle.clone(),
        );
        let changed = self
            .catalog
            .run_upload(move |connection| {
                connection
                    .prepare_cached(
                        "UPDATE appendable_objects SET finalizing = ?1 WHERE bucket = ?2 AND name = ?3 AND generation = ?4 AND write_handle = ?5 AND incarnation = (SELECT incarnation FROM bucket_incarnations WHERE bucket = ?2)",
                    )
                    .and_then(|mut statement| {
                        statement.execute(params![finalizing, bucket, name, generation, handle])
                    })
                    .map_err(object_unavailable)
            })
            .await?;
        if changed == 1 {
            Ok(())
        } else {
            Err(AppendError::Superseded)
        }
    }

    async fn stored_metadata(&self) -> Result<crate::object::ObjectMetadataUpdate, ObjectError> {
        let (bucket, name) = (self.bucket.clone(), self.name.clone());
        let row = self
            .catalog
            .run_upload(
                move |connection| match select(connection, &bucket, &name)? {
                    Lookup::Live(row) => Ok(*row),
                    _ => Err(ObjectError::ObjectNotFound),
                },
            )
            .await?;
        Ok(row.options.metadata)
    }

    /// The publication commit already removed the row, so the data file is unreferenced and
    /// only its removal remains.
    async fn retire(&self) {
        let path = self.path.clone();
        let removed = self
            .catalog
            .run_upload(move |_| unlink(&path).map_err(object_unavailable))
            .await;
        if let Err(error) = removed {
            deferred_cleanup(&error);
        }
    }
}

/// Removes the appendable row of a generation in the transaction that publishes it. Were
/// the row removed later, a failure in between would leave it behind, and once the published
/// generation was deleted that row would look unfinalized and serve the old bytes again.
pub(super) fn retire_published(
    connection: &Connection,
    record: &ObjectRecord,
) -> Result<(), ObjectError> {
    connection
        .prepare_cached(
            "DELETE FROM appendable_objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
        )
        .and_then(|mut statement| {
            statement.execute(params![record.bucket, record.name, record.generation])
        })
        .map_err(object_unavailable)?;
    Ok(())
}

/// A data file that no row references is never read, and startup recovery removes every such
/// file, so a failed removal only delays reclaiming its space.
fn deferred_cleanup(error: &dyn fmt::Display) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        "appendable_cleanup_deferred",
        serde_json::json!({
            "message": format!("unreferenced appendable data is removed at the next startup: {error}"),
        }),
    );
}

impl Catalog {
    /// Starts a new appendable generation. The generation is reserved from the shared
    /// allocator so finalization can publish it without renumbering.
    pub(crate) async fn create_appendable(
        &self,
        bucket: &str,
        name: &str,
        options: ResumableOptions,
    ) -> Result<AppendWriter, AppendError> {
        // Rapid buckets support neither; finalization would otherwise publish them silently.
        if options.metadata.retention.is_some() || options.metadata.customer_key_sha256.is_some() {
            return Err(ObjectError::UnsupportedMetadata(
                "object retention or customer-supplied encryption on appendable objects".into(),
            )
            .into());
        }
        let blob = BlobId::generate().map_err(object_unavailable)?;
        let handle = BlobId::generate().map_err(object_unavailable)?.to_hex();
        let blob_id = blob.to_hex();
        let path = data_path(&self.0.data_dir, &blob_id)?;
        let directory = self.0.data_dir.join("appendable");
        let file = path.clone();
        tokio::task::spawn_blocking(move || -> io::Result<()> {
            fs::create_dir_all(&directory)?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file)?;
            #[cfg(unix)]
            crate::fsutil::sync_dir(&directory)?;
            Ok(())
        })
        .await
        .map_err(object_unavailable)?
        .map_err(object_unavailable)?;
        let slot = SlotHandle::new(&self.0.data_dir, bucket, name);
        let mut guard = slot.lock().await;
        let result = self
            .insert_appendable(bucket, name, &blob_id, &handle, options)
            .await;
        let (stale, created) = match result {
            Ok(value) => value,
            Err(error) => {
                // No row was committed, so the new file stays unreferenced.
                if let Err(cleanup) = unlink(&path) {
                    deferred_cleanup(&cleanup);
                }
                return Err(error);
            }
        };
        if let Some(stale) = stale {
            // The insert removed the stale row, so its file is unreferenced.
            if let Err(cleanup) = unlink(&data_path(&self.0.data_dir, &stale)?) {
                deferred_cleanup(&cleanup);
            }
        }
        guard.owner = Some(handle.clone());
        drop(guard);
        Ok(AppendWriter {
            catalog: self.clone(),
            bucket: bucket.to_owned(),
            name: name.to_owned(),
            path,
            handle,
            slot,
            end: 0,
            crc32c: 0,
            record: created,
        })
    }

    async fn insert_appendable(
        &self,
        bucket: &str,
        name: &str,
        blob_id: &str,
        handle: &str,
        options: ResumableOptions,
    ) -> Result<(Option<String>, ObjectRecord), AppendError> {
        let (bucket, name, blob_id, handle) = (
            bucket.to_owned(),
            name.to_owned(),
            blob_id.to_owned(),
            handle.to_owned(),
        );
        let clock = self.0.clock.clone();
        self.run_upload(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(object_unavailable)?;
            let (storage_class, _) = authorized_bucket(&transaction, &bucket, &options.access)?;
            let (current, stale) = match select(&transaction, &bucket, &name)? {
                Lookup::Live(row) => (Some(record(&transaction, &bucket, &name, storage_class.clone(), &row)?), None),
                Lookup::Stale(id) => (None, Some(id)),
                Lookup::Absent => (None, None),
            };
            if let Some(id) = &stale {
                transaction
                    .prepare_cached("DELETE FROM appendable_objects WHERE blob_id = ?1")
                    .and_then(|mut statement| statement.execute([id]))
                    .map_err(object_unavailable)?;
            }
            let current = match current {
                Some(record) => Some(record),
                None => lookup(&transaction, &bucket, &name)?.map(|(record, _)| record),
            };
            check_conditions(current.as_ref(), options.conditions, super::objects::RequestKind::Write)?;
            if current.is_some() {
                return Ok(Err(AppendError::ReplaceUnsupported));
            }
            let incarnation = incarnation(&transaction, &bucket)?.ok_or(ObjectError::BucketNotFound)?;
            let micros = unix_micros(clock.now()).map_err(object_unavailable)?;
            let last: i64 = transaction
                .prepare_cached("SELECT last_generation FROM object_generation WHERE singleton = 1")
                .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
                .map_err(object_unavailable)?;
            let generation = micros.max(
                last.checked_add(1)
                    .ok_or_else(|| object_unavailable("generation exhausted"))?,
            );
            transaction
                .prepare_cached("UPDATE object_generation SET last_generation = ?1 WHERE singleton = 1")
                .and_then(|mut statement| statement.execute([generation]))
                .map_err(object_unavailable)?;
            let encoded = encode_options(&options).to_string();
            transaction
                .prepare_cached(
                    "INSERT INTO appendable_objects VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, ?7, ?7, ?8, 0)",
                )
                .and_then(|mut statement| {
                    statement.execute(params![bucket, name, generation, incarnation, blob_id, encoded, micros, handle])
                })
                .map_err(object_unavailable)?;
            let time = system_time_micros(micros).map_err(object_unavailable)?;
            let row = Row {
                generation,
                blob_id: blob_id.clone(),
                options,
                size: 0,
                crc32c: 0,
                time_created: time,
                updated: time,
                write_handle: handle.clone(),
            };
            let created = record(&transaction, &bucket, &name, storage_class, &row)?;
            transaction.commit().map_err(object_unavailable)?;
            Ok(Ok((stale, created)))
        })
        .await
        .map_err(AppendError::Object)?
    }

    /// Takes over an unfinalized generation. A matching write handle resumes the caller's own
    /// stream; a stale handle means another writer has taken over since, so it is rejected.
    pub(crate) async fn take_over_appendable(
        &self,
        bucket: &str,
        name: &str,
        generation: i64,
        write_handle: Option<&[u8]>,
        conditions: ObjectConditions,
        access: BucketAccess,
    ) -> Result<AppendWriter, AppendError> {
        let new_handle = BlobId::generate().map_err(object_unavailable)?.to_hex();
        let slot = SlotHandle::new(&self.0.data_dir, bucket, name);
        let mut guard = slot.lock().await;
        let (owned_bucket, owned_name, handle) =
            (bucket.to_owned(), name.to_owned(), new_handle.clone());
        let presented = write_handle.map(<[u8]>::to_vec);
        let selected = self
            .run_upload(move |connection| {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(object_unavailable)?;
                let (storage_class, _) = authorized_bucket(&transaction, &owned_bucket, &access)?;
                let row = match select(&transaction, &owned_bucket, &owned_name)? {
                    Lookup::Live(row) if row.generation == generation => row,
                    _ => {
                        return Ok(Err(
                            if published_generation(&transaction, &owned_bucket, &owned_name, generation)? {
                                AppendError::Finalized
                            } else {
                                AppendError::Object(ObjectError::ObjectNotFound)
                            },
                        ));
                    }
                };
                let record = record(&transaction, &owned_bucket, &owned_name, storage_class, &row)?;
                match presented {
                    Some(presented) if presented != row.write_handle.as_bytes() => {
                        return Ok(Err(AppendError::Superseded));
                    }
                    Some(_) => {}
                    None => {
                        if let Err(error) = check_conditions(Some(&record), conditions, super::objects::RequestKind::Write) {
                            return Ok(Err(error.into()));
                        }
                    }
                }
                transaction
                    .prepare_cached(
                        "UPDATE appendable_objects SET write_handle = ?1 WHERE bucket = ?2 AND name = ?3",
                    )
                    .and_then(|mut statement| statement.execute(params![handle, owned_bucket, owned_name]))
                    .map_err(object_unavailable)?;
                transaction.commit().map_err(object_unavailable)?;
                Ok(Ok((row.blob_id, record)))
            })
            .await??;
        let (blob_id, record) = selected;
        let path = data_path(&self.0.data_dir, &blob_id)?;
        let file = path.clone();
        let size = record.size;
        // Unflushed bytes from the previous writer were never acknowledged.
        tokio::task::spawn_blocking(move || -> Result<(), ObjectError> {
            let file = fs::OpenOptions::new()
                .write(true)
                .open(file)
                .map_err(object_unavailable)?;
            if file.metadata().map_err(object_unavailable)?.len() < size {
                return Err(ObjectError::Corrupt(
                    "appendable data is shorter than its persisted size".into(),
                ));
            }
            file.set_len(size).map_err(object_unavailable)?;
            file.sync_data().map_err(object_unavailable)
        })
        .await
        .map_err(object_unavailable)??;
        guard.owner = Some(new_handle.clone());
        drop(guard);
        Ok(AppendWriter {
            catalog: self.clone(),
            bucket: bucket.to_owned(),
            name: name.to_owned(),
            path,
            handle: new_handle,
            slot,
            end: record.size,
            crc32c: record.crc32c,
            record,
        })
    }

    /// Opens the persisted prefix of an unfinalized generation for reading.
    pub(crate) async fn open_appendable(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(ObjectRecord, ObjectReader), ObjectError> {
        let (bucket, name, access) = (bucket.to_owned(), name.to_owned(), access.clone());
        let data_dir = self.0.data_dir.clone();
        let (record, file) = self
            .run_object(move |connection| {
                let transaction = connection.transaction().map_err(object_unavailable)?;
                let (storage_class, _) = authorized_bucket(&transaction, &bucket, &access)?;
                let Lookup::Live(row) = select(&transaction, &bucket, &name)? else {
                    return Err(ObjectError::ObjectNotFound);
                };
                if generation.is_some_and(|generation| generation != row.generation) {
                    return Err(ObjectError::ObjectNotFound);
                }
                let record = record(&transaction, &bucket, &name, storage_class, &row)?;
                check_conditions(Some(&record), conditions, super::objects::RequestKind::Read)?;
                // Opened inside the transaction so finalization cannot unlink it first.
                let file = fs::File::open(data_path(&data_dir, &row.blob_id)?)
                    .map_err(object_unavailable)?;
                Ok((record, file))
            })
            .await?;
        let size = record.size;
        Ok((
            record,
            ObjectReader::bounded(tokio::fs::File::from_std(file), size),
        ))
    }

    pub(crate) async fn delete_appendable(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        let slot = SlotHandle::new(&self.0.data_dir, bucket, name);
        let mut guard = slot.lock().await;
        let (owned_bucket, owned_name, access) =
            (bucket.to_owned(), name.to_owned(), access.clone());
        let data_dir = self.0.data_dir.clone();
        let clock = self.0.clock.clone();
        self.run_upload(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(object_unavailable)?;
            let (storage_class, _) = authorized_bucket(&transaction, &owned_bucket, &access)?;
            let Lookup::Live(row) = select(&transaction, &owned_bucket, &owned_name)? else {
                return Err(ObjectError::ObjectNotFound);
            };
            if generation.is_some_and(|generation| generation != row.generation) {
                return Err(ObjectError::ObjectNotFound);
            }
            let record = record(
                &transaction,
                &owned_bucket,
                &owned_name,
                storage_class,
                &row,
            )?;
            check_conditions(
                Some(&record),
                conditions,
                super::objects::RequestKind::Write,
            )?;
            record.check_retention(clock.now())?;
            transaction
                .prepare_cached("DELETE FROM appendable_objects WHERE bucket = ?1 AND name = ?2")
                .and_then(|mut statement| statement.execute(params![owned_bucket, owned_name]))
                .map_err(object_unavailable)?;
            transaction.commit().map_err(object_unavailable)?;
            unlink(&data_path(&data_dir, &row.blob_id)?).map_err(object_unavailable)
        })
        .await?;
        guard.owner = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
