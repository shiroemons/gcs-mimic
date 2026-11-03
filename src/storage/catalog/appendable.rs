//! Rapid appendable objects.
//!
//! An unfinalized appendable generation is a mutable state machine, so it lives outside the
//! immutable `objects` table: one row per object name plus one growing data file. The row is
//! the durability boundary. Bytes past its `size` are unacknowledged and are truncated by the
//! next writer; readers never look past it. Finalization streams
//! the persisted bytes through ordinary publication with the generation reserved at creation,
//! and that publication's transaction removes the row. Catalogs written before that change can
//! still hold a row whose generation was published, so startup recovery resolves every
//! legacy row before any request can delete the published generation: a published
//! generation retires the row, otherwise the row becomes appendable again. At runtime a row
//! whose generation is published is always treated as finalized.

use super::{
    Catalog,
    objects::{
        ObjectUpload, PublicationDefaults, authorized_bucket, check_conditions, lookup,
        object_unavailable, unlink,
    },
    resumable::{ResumableOptions, decode_options, encode_options},
    system_time_micros, unix_micros,
};
use crate::storage::catalog::{bucket_incarnation, sql};
use crate::{
    bucket::{BucketAccess, CatalogError},
    object::{ObjectConditions, ObjectError, ObjectRecord, ObjectTarget, UploadChunk},
    storage::blob_id::BlobId,
};
use bytes::Bytes;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::HashMap,
    fmt, fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, PoisonError},
    time::SystemTime,
};
use tokio::sync::mpsc;

#[cfg(test)]
use super::objects::ObjectReader;

mod data;
mod readable;
pub(crate) use readable::Finality;

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
const COPY_READ_LIMIT: usize = 1024 * 1024;

/// Only appendable state can construct an identity reserved before its bytes arrived.
#[derive(Clone, Copy, Debug)]
pub(super) struct ReservedGeneration {
    generation: super::admin::Generation,
    time_created: SystemTime,
    updated: SystemTime,
}

impl ReservedGeneration {
    fn from_record(record: &ObjectRecord) -> Result<Self, ObjectError> {
        Ok(Self {
            generation: super::admin::Generation::new(record.generation)
                .map_err(object_unavailable)?,
            time_created: record.time_created,
            updated: record.updated,
        })
    }

    pub(super) fn generation(self) -> super::admin::Generation {
        self.generation
    }
    pub(super) fn time_created(self) -> SystemTime {
        self.time_created
    }
    pub(super) fn updated(self) -> SystemTime {
        self.updated
    }
}

/// Runs at catalog open, before requests: resolves interrupted finalizations, rows of deleted
/// buckets and data files that never received a row.
#[expect(
    clippy::disallowed_methods,
    reason = "open-time recovery probes and resets each kind of interrupted state once"
)]
pub(super) fn recover(connection: &Connection, data_dir: &Path) -> Result<(), CatalogError> {
    let directory = data_dir.join("appendable");
    if !directory.try_exists()? {
        fs::create_dir(&directory)?;
        #[cfg(all(test, unix))]
        super::fault::dir_created(&directory);
        #[cfg(unix)]
        super::objects::sync_dir(data_dir)?;
    }
    let retired = connection
        .prepare_cached(
            "SELECT a.blob_id FROM appendable_objects a WHERE EXISTS(SELECT 1 FROM objects o WHERE o.bucket = a.bucket AND o.name = a.name AND o.generation = a.generation) OR NOT EXISTS(SELECT 1 FROM bucket_incarnations i WHERE i.bucket = a.bucket AND i.incarnation = a.incarnation)",
        )?
        .query_map([], |row| row.get::<_, BlobId>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut removed = !retired.is_empty();
    for id in retired {
        connection.execute("DELETE FROM appendable_objects WHERE blob_id = ?1", [&id])?;
        unlink(&data_path(data_dir, &id))?;
    }
    // Legacy column: always zero after open. Avoid taking a write lock without work.
    let interrupted: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM appendable_objects WHERE finalizing = 1)",
        [],
        |row| row.get(0),
    )?;
    if interrupted {
        connection.execute(
            "UPDATE appendable_objects SET finalizing = 0 WHERE finalizing = 1",
            [],
        )?;
    }
    for entry in fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // A creation that crashed before its row committed leaves an unreferenced file.
        if BlobId::parse(&name).is_none() {
            continue;
        }
        let referenced: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM appendable_objects WHERE blob_id = ?1)",
            [&name],
            |row| row.get(0),
        )?;
        if !referenced {
            unlink(&entry.path())?;
            removed = true;
        }
    }
    #[cfg(unix)]
    if removed {
        super::objects::sync_dir(&directory)?;
    }
    #[cfg(not(unix))]
    let _ = removed;
    Ok(())
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
    blob_id: BlobId,
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

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum Namespace {
    Disk(PathBuf),
    Memory(crate::storage::memory::StoreId),
}

type SlotKey = (Namespace, String, String);

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
    fn new(catalog: &Catalog, bucket: &str, name: &str) -> Self {
        let namespace = match &catalog.0.backend {
            super::BlobBackend::Memory(memory) => Namespace::Memory(memory.id()),
            super::BlobBackend::Disk(root) => Namespace::Disk(root.clone()),
        };
        let key = (namespace, bucket.to_owned(), name.to_owned());
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

impl Clone for SlotHandle {
    fn clone(&self) -> Self {
        let mut slots = slots().lock().unwrap_or_else(PoisonError::into_inner);
        let entry = slots.entry(self.key.clone()).or_insert_with(|| SlotEntry {
            holders: 0,
            slot: self.slot.clone(),
        });
        entry.holders += 1;
        Self {
            key: self.key.clone(),
            slot: self.slot.clone(),
        }
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

/// A paused append write: `entered` reports that the write job runs, and the job waits on
/// `release` before it touches the file.
#[cfg(test)]
struct WritePause {
    data_dir: PathBuf,
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

#[cfg(test)]
static WRITE_PAUSES: Mutex<Vec<WritePause>> = Mutex::new(Vec::new());

/// Pauses the next append write of the catalog in `data_dir` inside its blocking job.
#[cfg(test)]
fn arm_write_pause(
    data_dir: &Path,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (entered, on_entered) = tokio::sync::oneshot::channel();
    let (release, on_release) = std::sync::mpsc::channel();
    WRITE_PAUSES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(WritePause {
            data_dir: data_dir.to_owned(),
            entered,
            release: on_release,
        });
    (on_entered, release)
}

#[cfg(test)]
fn pause_append_write(data_dir: &Path) {
    let pause = {
        let mut pauses = WRITE_PAUSES.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(index) = pauses.iter().position(|pause| pause.data_dir == data_dir) else {
            return;
        };
        pauses.remove(index)
    };
    let _ = pause.entered.send(());
    let _ = pause.release.recv();
}

#[cfg(test)]
fn slot_count(data_dir: &Path) -> usize {
    slots()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .keys()
        .filter(|(namespace, _, _)| matches!(namespace, Namespace::Disk(dir) if dir == data_dir))
        .count()
}

fn data_path(data_dir: &Path, blob_id: &BlobId) -> PathBuf {
    data_dir.join("appendable").join(blob_id.to_hex())
}

enum Lookup {
    Absent,
    /// A row from a deleted bucket incarnation or an already published generation.
    Stale(BlobId),
    Live(Box<Row>),
}

fn select(connection: &Connection, bucket: &str, name: &str) -> Result<Lookup, ObjectError> {
    const SELECT: &str = "SELECT generation, incarnation, blob_id, options, size, crc32c, time_created, updated, write_handle FROM appendable_objects WHERE bucket = ?1 AND name = ?2";
    let row = sql::row(connection, SELECT, params![bucket, name], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, BlobId>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, u32>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
            row.get::<_, String>(8)?,
        ))
    })
    .optional()?;
    let Some((generation, stored, blob_id, options, size, crc32c, created, updated, handle)) = row
    else {
        return Ok(Lookup::Absent);
    };
    if published_generation(connection, bucket, name, generation)?
        || bucket_incarnation(connection, bucket)
            .optional()?
            .as_deref()
            != Some(stored.as_str())
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
        time_created: system_time_micros(created)?,
        updated: system_time_micros(updated)?,
        write_handle: handle,
    })))
}

fn published_generation(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: i64,
) -> Result<bool, ObjectError> {
    sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3)",
        params![bucket, name, generation],
    )
    .map_err(object_unavailable)
}

/// The adapter-visible view of an unfinalized generation. It has no MD5 and no finalize time.
/// The storage class is resolved as finalization's publication will resolve it, including
/// Autoclass.
fn record(
    connection: &Connection,
    bucket: &str,
    name: &str,
    bucket_class: crate::bucket::StorageClass,
    row: &Row,
    profile: Option<&super::principals::Profile>,
) -> Result<ObjectRecord, ObjectError> {
    let storage_class = super::bucket_autoclass::object_storage_class(
        connection,
        bucket,
        row.options.metadata.storage_class,
        bucket_class,
    )?;
    let defaults = PublicationDefaults {
        storage_class,
        event_based_hold: false,
        time: row.time_created,
        owner: Some(super::object_owner::for_publication(
            connection,
            bucket,
            &row.options.access,
            profile,
        )?),
    };
    // Creation rejects object retention and customer-supplied keys.
    let mut record = defaults.record(
        ObjectTarget::new(bucket, name, row.generation),
        row.options.content_type.clone(),
        row.size,
        crate::hashing::Hashes {
            md5: [0; 16],
            crc32c: row.crc32c,
        },
    );
    record.updated = row.updated;
    record.md5_availability = crate::object::Md5Availability::Unavailable;
    row.options.metadata.clone().apply(&mut record)?;
    Ok(record)
}

/// An exclusive writer for one appendable generation.
pub(crate) struct AppendWriter {
    catalog: Catalog,
    activity: Arc<super::Activity>,
    bucket: String,
    name: String,
    blob_id: BlobId,
    /// Appends and flushes share this descriptor. Linux tracks writeback errors per open file:
    /// a descriptor opened after a failure reports it only while no other descriptor has seen
    /// it (and never before Linux 4.16), so a sync through a fresh descriptor could
    /// acknowledge lost bytes as durable.
    file: data::Data,
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
        let end = self
            .end
            .checked_add(added.len() as u64)
            .filter(|end| *end <= max_bytes)
            .ok_or(ObjectError::TooLarge)?;
        let guard = self.slot().lock_owned().await;
        if guard.owner.as_deref() != Some(self.handle.as_str()) {
            return Err(AppendError::Superseded);
        }
        if added.is_empty() {
            return Ok(());
        }
        let file = self.file.clone();
        let slot = self.slot.clone();
        let start = self.end;
        let bytes = added.clone();
        let activity = self.activity.clone();
        #[cfg(test)]
        let data_dir = self.catalog.0.data_dir.clone();
        crate::task::run_blocking("append-write", move || -> Result<(), ObjectError> {
            // The guard lives in the blocking job, not the future: a dropped append still
            // holds the slot until its write ends, so a takeover cannot truncate the file and
            // accept new bytes that this late write through the old descriptor overwrites.
            let (_guard, _activity, _slot) = (guard, activity, slot);
            #[cfg(test)]
            pause_append_write(&data_dir);
            #[cfg(test)]
            super::fault::hit(&data_dir, super::fault::Step::AppendWrite)
                .map_err(object_unavailable)?;
            file.write(start, &bytes).map_err(object_unavailable)
        })
        .await
        .map_err(object_unavailable)??;
        self.crc32c = crc32c::crc32c_append(self.crc32c, &added);
        self.end = end;
        Ok(())
    }

    /// Makes every received byte durable, then publishes the new size to readers.
    pub(crate) async fn flush(&mut self) -> Result<u64, AppendError> {
        let guard = self.slot().lock_owned().await;
        if guard.owner.as_deref() != Some(self.handle.as_str()) {
            return Err(AppendError::Superseded);
        }
        if self.end == self.record.size {
            return Ok(self.end);
        }
        let guard = (guard, self.slot.clone());
        let file = self.file.clone();
        let activity = self.activity.clone();
        #[cfg(test)]
        let data_dir = self.catalog.0.data_dir.clone();
        // As in `append`, the blocking job owns the guard while it touches the file, so a
        // dropped flush cannot let a takeover truncate the file under a sync in flight. The
        // size update below runs on the catalog executor, which finishes a queued job even
        // when its caller is dropped, and its write-handle check rejects a superseded writer.
        let (guard, synced) = crate::task::run_blocking("append-sync", move || {
            let _activity = activity;
            #[cfg(test)]
            if let Err(injected) = super::fault::hit(&data_dir, super::fault::Step::AppendSync) {
                return (Some(guard), Err(object_unavailable(injected)));
            }
            (Some(guard), file.sync().map_err(object_unavailable))
        })
        .await
        .map_err(object_unavailable)
        .unwrap_or_else(|error| (None, Err(error)));
        let checkpoint = match synced {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                // After a failed sync the unflushed bytes may be lost even though a retried sync
                // would succeed, so they must be received and written again before any flush.
                self.end = self.record.size;
                self.crc32c = self.record.crc32c;
                return Err(error.into());
            }
        };
        #[cfg(test)]
        super::fault::hit(&self.catalog.0.data_dir, super::fault::Step::AppendCommit)
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
                let _guard = guard;
                let micros = unix_micros(clock.now())?;
                let changed = sql::exec(connection,
                        // A deleted or recreated bucket invalidates the writer as well.
                        "UPDATE appendable_objects SET size = ?1, crc32c = ?2, updated = ?3 WHERE bucket = ?4 AND name = ?5 AND generation = ?6 AND write_handle = ?7 AND incarnation = (SELECT incarnation FROM bucket_incarnations WHERE bucket = ?4)", params![size, crc32c, micros, bucket, name, generation, handle])?;
                if changed == 1 && let Some(checkpoint) = checkpoint { checkpoint.commit(); }
                Ok((changed == 1).then_some(micros))
            })
            .await?;
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
        // Unlike `append` and `flush`, the guard may stay in this future: finalization only
        // reads the data file, and a dropped publication either commits on the catalog
        // executor, which removes the appendable row, or leaves the generation appendable.
        let slot = self.slot();
        let mut guard = slot.lock().await;
        if guard.owner.as_deref() != Some(self.handle.as_str()) {
            return Err(AppendError::Superseded);
        }
        let catalog = self.catalog.clone();
        let identity = ReservedGeneration::from_record(&self.record)?;
        let permit = catalog.admit_object_upload()?;
        let (tx, rx) = mpsc::channel(2);
        let data = self.file.clone();
        let size = self.record.size;
        // Return blocking threads before waiting for the consumer, whose file writes need them too.
        let producer = async move {
            let mut file = crate::task::run_blocking("append-reader-open", move || data.reader())
                .await
                .map_err(io::Error::other)??;
            let mut remaining = size;
            while remaining > 0 {
                let length = remaining.min(COPY_READ_LIMIT as u64) as usize;
                let (reader, mut bytes) = crate::task::run_blocking("append-reader", move || {
                    let mut bytes = vec![0; length];
                    file.read_exact(&mut bytes)?;
                    Ok::<_, io::Error>((file, Bytes::from(bytes)))
                })
                .await
                .map_err(io::Error::other)??;
                file = reader;
                remaining -= length as u64;
                while !bytes.is_empty() {
                    let chunk = bytes.split_to(bytes.len().min(COPY_CHUNK));
                    if tx.send(Ok(UploadChunk::Data(chunk))).await.is_err() {
                        return Ok(());
                    }
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
            super::objects::PublishIdentity::Reserved(identity),
            super::objects::OwnerAssignment::Preserve(self.record.owner.clone()),
        );
        let (copied, published) = tokio::join!(producer, publish);
        let published = published?;
        copied.map_err(object_unavailable)?;
        guard.owner = None;
        self.retire().await;
        drop(guard);
        Ok(published)
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
        let data = if self.catalog.0.backend.is_memory() {
            Unreferenced::Journaled
        } else {
            Unreferenced::DiskFile(self.blob_id)
        };
        self.catalog.release_appendable_data(data).await;
    }
}

/// Removes the appendable row of a generation in the transaction that publishes it. Were
/// the row removed later, a failure in between would leave it behind, and once the published
/// generation was deleted that row would look unfinalized and serve the old bytes again.
pub(super) fn retire_published(
    connection: &Connection,
    record: &ObjectRecord,
    backend: crate::config::Backend,
) -> Result<super::objects::Journaled, ObjectError> {
    let mut journaled = super::objects::Journaled::NOTHING;
    if let Some(id) = sql::optional_scalar::<BlobId>(
        connection,
        "SELECT blob_id FROM appendable_objects WHERE bucket=?1 AND name=?2 AND generation=?3",
        params![record.bucket, record.name, record.generation],
    )? {
        // AppendWriter::retire releases disk data after this publication commits.
        if let Unreferenced::Journaled = unreference(connection, backend, &id)? {
            journaled = super::objects::Journaled::SOME;
        }
    }
    sql::exec(
        connection,
        "DELETE FROM appendable_objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
        params![record.bucket, record.name, record.generation],
    )?;
    Ok(journaled)
}

/// A data file that no row references is never read, and startup recovery removes every such
/// file, so a failed removal only delays reclaiming its space.
fn deferred_cleanup(error: &dyn fmt::Display) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        "appendable_cleanup_deferred",
        serde_json::json!({
            "message": format!("unreferenced appendable data is reclaimed later: {error}"),
        }),
    );
}

/// Data no committed row references; failed release only delays reclamation.
#[must_use = "a committed removal must release its data"]
enum Unreferenced {
    Journaled,
    DiskFile(BlobId),
}

fn unreference(
    connection: &Connection,
    backend: crate::config::Backend,
    id: &BlobId,
) -> Result<Unreferenced, ObjectError> {
    match backend {
        crate::config::Backend::Memory => {
            sql::exec(
                connection,
                "INSERT OR IGNORE INTO pending_blob_deletions VALUES (?1)",
                [id],
            )?;
            Ok(Unreferenced::Journaled)
        }
        crate::config::Backend::Disk => Ok(Unreferenced::DiskFile(*id)),
    }
}

impl Catalog {
    async fn release_appendable_data(&self, data: Unreferenced) {
        #[cfg(test)]
        if let Err(error) = super::fault::hit(&self.0.data_dir, super::fault::Step::AppendCleanup) {
            deferred_cleanup(&object_unavailable(error));
            return;
        }
        match data {
            Unreferenced::Journaled => self.reclaim_after_commit().await,
            Unreferenced::DiskFile(id) => {
                let catalog = self.clone();
                match crate::task::run_blocking("append-cleanup", move || {
                    data::remove(&catalog, &id)
                })
                .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => deferred_cleanup(&error),
                    Err(error) => deferred_cleanup(&error),
                }
            }
        }
    }

    /// Starts a new appendable generation. The generation is reserved from the shared
    /// allocator so finalization can publish it without renumbering.
    pub(crate) async fn create_appendable(
        &self,
        bucket: &str,
        name: &str,
        options: ResumableOptions,
    ) -> Result<AppendWriter, AppendError> {
        let activity = Arc::new(self.activity().map_err(object_unavailable)?);
        // Rapid buckets support neither; finalization would otherwise publish them silently.
        if options.metadata.retention.is_some() || options.metadata.customer_key_sha256.is_some() {
            return Err(ObjectError::UnsupportedMetadata(
                "object retention or customer-supplied encryption on appendable objects".into(),
            )
            .into());
        }
        let blob = BlobId::generate().map_err(object_unavailable)?;
        let handle = BlobId::generate().map_err(object_unavailable)?.to_hex();
        let blob_id = blob;
        let catalog = self.clone();
        let id = blob_id;
        let file_activity = activity.clone();
        let file = crate::task::run_blocking("append-create", move || {
            let _activity = file_activity;
            data::Data::create(&catalog.0.backend, &id)
        })
        .await
        .map_err(object_unavailable)?
        .map_err(object_unavailable)?;
        let slot = SlotHandle::new(self, bucket, name);
        let mut guard = slot.lock().await;
        let result = self
            .insert_appendable(bucket, name, &blob_id, &handle, options)
            .await;
        let (stale, created) = match result {
            Ok(value) => value,
            Err(error) => {
                // No row was committed, so the new file stays unreferenced.
                if !self.0.backend.is_memory() {
                    self.release_appendable_data(Unreferenced::DiskFile(blob_id))
                        .await;
                }
                return Err(error);
            }
        };
        guard.owner = Some(handle.clone());
        drop(guard);
        if let Some(stale) = stale {
            self.release_appendable_data(stale).await;
        }
        Ok(AppendWriter {
            catalog: self.clone(),
            activity,
            bucket: bucket.to_owned(),
            name: name.to_owned(),
            blob_id,
            file,
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
        blob_id: &BlobId,
        handle: &str,
        options: ResumableOptions,
    ) -> Result<(Option<Unreferenced>, ObjectRecord), AppendError> {
        let (bucket, name, blob_id, handle) = (
            bucket.to_owned(),
            name.to_owned(),
            *blob_id,
            handle.to_owned(),
        );
        let clock = self.0.clock.clone();
        let backend = self.0.backend.clone();
        let principals = self.0.principals.get().cloned();
        self.run_upload(move |connection| {
            let transaction = sql::immediate(connection)?;
            let (storage_class, _) = authorized_bucket(&transaction, &bucket, &options.access)?;
            let (current, stale) = match select(&transaction, &bucket, &name)? {
                Lookup::Live(row) => (Some(record(&transaction, &bucket, &name, storage_class, &row, principals.as_ref())?), None),
                Lookup::Stale(id) => (None, Some(id)),
                Lookup::Absent => (None, None),
            };
            if let Some(id) = &stale {
                sql::exec(&transaction, "DELETE FROM appendable_objects WHERE blob_id = ?1", [id])?;
            }
            let stale = stale.map(|id| unreference(&transaction, backend.kind(), &id)).transpose()?;
            let current = match current {
                Some(record) => Some(record),
                None => lookup(&transaction, &bucket, &name)?.map(|stored| stored.record),
            };
            check_conditions(current.as_ref(), options.conditions, super::objects::RequestKind::Write)?;
            if current.is_some() {
                return Ok(Err(AppendError::ReplaceUnsupported));
            }
            let incarnation = bucket_incarnation(&transaction, &bucket).optional()?.ok_or(ObjectError::BucketNotFound)?;
            let micros = unix_micros(clock.now())?;
            let last: i64 = sql::scalar(&transaction, "SELECT last_generation FROM object_generation WHERE singleton = 1", [])?;
            let generation = micros.max(
                last.checked_add(1)
                    .ok_or_else(|| object_unavailable("generation exhausted"))?,
            );
            sql::exec(&transaction, "UPDATE object_generation SET last_generation = ?1 WHERE singleton = 1", [generation])?;
            let encoded = encode_options(&options).to_string();
            sql::exec(&transaction,
                    "INSERT INTO appendable_objects VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, ?7, ?7, ?8, 0)", params![bucket, name, generation, incarnation, blob_id, encoded, micros, handle])?;
            let time = system_time_micros(micros)?;
            let row = Row {
                generation,
                blob_id,
                options,
                size: 0,
                crc32c: 0,
                time_created: time,
                updated: time,
                write_handle: handle.clone(),
            };
            let created = record(&transaction, &bucket, &name, storage_class, &row, principals.as_ref())?;
            let checkpoint = backend.checkpoint_draft(&blob_id)?;
            let committed = transaction.commit().map_err(object_unavailable);
            if let Some(checkpoint) = checkpoint {
                let referenced = sql::scalar::<bool>(connection, "SELECT EXISTS(SELECT 1 FROM appendable_objects WHERE blob_id=?1)", [&blob_id]);
                // An ambiguous commit or failed probe must never discard possibly committed bytes.
                if committed.is_ok() || !matches!(referenced, Ok(false)) { checkpoint.commit(); }
            }
            committed?;
            Ok(Ok((stale, created)))
        })
        .await?
    }

    /// Takes over an unfinalized generation. A matching write handle resumes the caller's own
    /// stream; a stale handle means another writer has taken over since, so it is rejected.
    pub(crate) async fn take_over_appendable(
        &self,
        target: ObjectTarget<i64>,
        write_handle: Option<&[u8]>,
        conditions: ObjectConditions,
        access: BucketAccess,
    ) -> Result<AppendWriter, AppendError> {
        let activity = Arc::new(self.activity().map_err(object_unavailable)?);
        let new_handle = BlobId::generate().map_err(object_unavailable)?.to_hex();
        let slot = SlotHandle::new(self, &target.bucket, &target.name);
        let guard = slot.slot.clone().lock_owned().await;
        let ObjectTarget {
            bucket,
            name,
            generation,
        } = target;
        let handle = new_handle.clone();
        let presented = write_handle.map(<[u8]>::to_vec);
        let principals = self.0.principals.get().cloned();
        let selected = self
            .run_upload(move |connection| {
                let transaction = sql::immediate(connection)?;
                let (storage_class, _) = authorized_bucket(&transaction, &bucket, &access)?;
                let row = match select(&transaction, &bucket, &name)? {
                    Lookup::Live(row) if row.generation == generation => row,
                    _ => {
                        return Ok(Err(
                            if published_generation(&transaction, &bucket, &name, generation)? {
                                AppendError::Finalized
                            } else {
                                AppendError::Object(ObjectError::ObjectNotFound)
                            },
                        ));
                    }
                };
                let record = record(&transaction, &bucket, &name, storage_class, &row, principals.as_ref())?;
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
                sql::exec(&transaction,
                        "UPDATE appendable_objects SET write_handle = ?1 WHERE bucket = ?2 AND name = ?3", params![handle, bucket, name])?;
                transaction.commit()?;
                Ok(Ok((row.blob_id, record)))
            })
            .await??;
        let (blob_id, record) = selected;
        let catalog = self.clone();
        let id = blob_id;
        let size = record.size;
        let file_activity = activity.clone();
        // Unflushed bytes from the previous writer were never acknowledged.
        let (mut guard, slot, file) = crate::task::run_blocking("append-takeover", move || {
            let _activity = file_activity;
            #[cfg(test)]
            pause_append_write(&catalog.0.data_dir);
            let result = data::Data::take_over(&catalog.0.backend, &id, size);
            (guard, slot, result)
        })
        .await
        .map_err(object_unavailable)?;
        let file = file?;
        guard.owner = Some(new_handle.clone());
        drop(guard);
        Ok(AppendWriter {
            catalog: self.clone(),
            activity,
            bucket: record.bucket.clone(),
            name: record.name.clone(),
            blob_id,
            file,
            handle: new_handle,
            slot,
            end: record.size,
            crc32c: record.crc32c,
            record,
        })
    }

    /// Opens the persisted prefix of an unfinalized generation for reading.
    #[cfg(test)]
    pub(crate) async fn open_appendable(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(ObjectRecord, ObjectReader), ObjectError> {
        let access = access.clone();
        let catalog = self.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            readable::open_appendable_in(&transaction, &catalog, &target, conditions, &access)
        })
        .await
    }

    #[cfg(test)]
    pub(crate) async fn delete_appendable(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        let slot = SlotHandle::new(self, &target.bucket, &target.name);
        let mut guard = slot.lock().await;
        self.delete_appendable_locked(target, conditions, access, &mut guard)
            .await
    }

    async fn delete_appendable_locked(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
        guard: &mut Slot,
    ) -> Result<(), ObjectError> {
        let ObjectTarget {
            bucket,
            name,
            generation,
        } = target;
        let access = access.clone();
        let backend = self.0.backend.kind();
        let clock = self.0.clock.clone();
        let principals = self.0.principals.get().cloned();
        let id = self
            .run_upload(move |connection| {
                let transaction = sql::immediate(connection)?;
                let (storage_class, _) = authorized_bucket(&transaction, &bucket, &access)?;
                let Lookup::Live(row) = select(&transaction, &bucket, &name)? else {
                    return Err(ObjectError::ObjectNotFound);
                };
                if generation.is_some_and(|generation| generation != row.generation) {
                    return Err(ObjectError::ObjectNotFound);
                }
                let record = record(
                    &transaction,
                    &bucket,
                    &name,
                    storage_class,
                    &row,
                    principals.as_ref(),
                )?;
                check_conditions(
                    Some(&record),
                    conditions,
                    super::objects::RequestKind::Write,
                )?;
                record.check_retention(clock.now())?;
                sql::exec(
                    &transaction,
                    "DELETE FROM appendable_objects WHERE bucket = ?1 AND name = ?2",
                    params![bucket, name],
                )?;
                let data = unreference(&transaction, backend, &row.blob_id)?;
                transaction.commit()?;
                Ok(data)
            })
            .await?;
        guard.owner = None;
        drop(guard);
        self.release_appendable_data(id).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
