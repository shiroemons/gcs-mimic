//! Generation-pinned, resumable byte derivation shared by copy, rewrite and compose.

use super::{Catalog, objects, resumable::WriterGuard, system_time, unix_millis};
use crate::{
    bucket::BucketAccess,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate, ObjectRecord},
};
use md5::{
    Digest, Md5,
    digest::common::hazmat::{SerializableState, SerializedState},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

const BUFFER_SIZE: usize = 64 * 1024;
const MAX_TRANSFERS: usize = 128;
const LIFETIME_MILLIS: i64 = 604_800_000;
pub(super) const SCHEMA: &str = "ALTER TABLE objects ADD COLUMN component_count INTEGER CHECK(component_count IS NULL OR component_count > 0);
CREATE TABLE derived_transfers (
    id TEXT PRIMARY KEY CHECK(length(id) = 32),
    total INTEGER NOT NULL CHECK(total >= 0),
    offset INTEGER NOT NULL CHECK(offset >= 0 AND offset <= total),
    md5_state BLOB NOT NULL,
    crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
    ephemeral INTEGER NOT NULL CHECK(ephemeral IN (0, 1)),
    created INTEGER NOT NULL,
    deadline INTEGER NOT NULL CHECK(deadline = created + 604800000),
    publishing INTEGER NOT NULL DEFAULT 0 CHECK(publishing IN (0, 1))
) WITHOUT ROWID;
CREATE TABLE derived_deletions (
    blob_id TEXT PRIMARY KEY CHECK(length(blob_id) = 32),
    transfer_id TEXT NOT NULL CHECK(length(transfer_id) = 32)
) WITHOUT ROWID;
CREATE TABLE derived_inputs (
    transfer_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 31),
    blob_id TEXT NOT NULL UNIQUE CHECK(length(blob_id) = 32),
    start INTEGER NOT NULL CHECK(start >= 0),
    length INTEGER NOT NULL CHECK(length >= 0),
    size INTEGER NOT NULL CHECK(size >= start AND length <= size - start),
    PRIMARY KEY(transfer_id, position)
) WITHOUT ROWID;
PRAGMA user_version = 10;";

fn unavailable(error: impl std::fmt::Display) -> ObjectError {
    ObjectError::Unavailable(error.to_string())
}

fn random_id() -> Result<String, ObjectError> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(unavailable)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn stage_path(data_dir: &Path, id: &str) -> Result<PathBuf, ObjectError> {
    objects::blob_path(data_dir, id)?;
    Ok(data_dir.join("staging").join(id))
}

/// A metadata snapshot and independent link to immutable bytes, including after source deletion.
/// Unconsumed handles clean their staging link; `begin_derived` transfers ownership to SQLite.
pub struct DerivedSource {
    pub object: ObjectRecord,
    path: Option<PathBuf>,
    id: String,
}

impl Drop for DerivedSource {
    fn drop(&mut self) {
        if let Some(path) = &self.path
            && let Err(error) = objects::unlink(path)
        {
            eprintln!("deferred derived source cleanup: {error}");
        }
    }
}

pub struct DerivedSegment {
    pub source: DerivedSource,
    pub offset: u64,
    pub length: u64,
}

#[derive(Clone, Debug)]
pub struct DerivedSourceSpec {
    pub bucket: String,
    pub name: String,
    pub generation: Option<i64>,
    pub conditions: ObjectConditions,
    pub access: BucketAccess,
}

impl DerivedSource {
    pub fn whole(self) -> DerivedSegment {
        let length = self.object.size;
        DerivedSegment {
            source: self,
            offset: 0,
            length,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivedTransfer {
    pub id: String,
    pub total: u64,
    pub offset: u64,
    pub created: std::time::SystemTime,
    pub deadline: std::time::SystemTime,
}

pub struct DerivedRequestGuard {
    catalog: Catalog,
    id: Option<String>,
}

impl DerivedRequestGuard {
    pub fn disarm(&mut self) {
        self.id = None;
    }
}

impl Drop for DerivedRequestGuard {
    fn drop(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        let catalog = self.catalog.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                loop {
                    let notified = catalog.0.derived_wake.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    match catalog.derived_guard(&id) {
                        Ok(_guard) => {
                            if let Err(error) = catalog.cleanup_derived(&id).await
                                && error != ObjectError::DerivedTransferNotFound
                            {
                                eprintln!("deferred abandoned derived cleanup: {error}");
                            }
                            return;
                        }
                        Err(ObjectError::DerivedTransferBusy) => notified.await,
                        Err(error) => {
                            eprintln!("deferred abandoned derived cleanup: {error}");
                            return;
                        }
                    }
                }
            });
        }
    }
}

/// Adapters supply inherited or replacement metadata explicitly; bytes and destination defaults
/// are transport independent. An explicit class is used by rewrite, otherwise the bucket default.
#[derive(Clone, Debug, Default)]
pub struct DerivedDestination {
    pub bucket: String,
    pub name: String,
    pub content_type: String,
    pub metadata: ObjectMetadataUpdate,
    pub storage_class: Option<String>,
    pub expected: ExpectedHashes,
    pub conditions: ObjectConditions,
    pub access: BucketAccess,
    pub component_count: Option<i64>,
}

struct Input {
    id: String,
    start: u64,
    length: u64,
    size: u64,
}

fn inputs(connection: &Connection, id: &str) -> Result<Vec<Input>, ObjectError> {
    connection.prepare("SELECT blob_id, start, length, size FROM derived_inputs WHERE transfer_id = ?1 ORDER BY position LIMIT 33")
        .map_err(unavailable)?.query_map([id], |row| Ok(Input { id: row.get(0)?, start: row.get::<_, i64>(1)? as u64, length: row.get::<_, i64>(2)? as u64, size: row.get::<_, i64>(3)? as u64 }))
        .map_err(unavailable)?.collect::<rusqlite::Result<_>>().map_err(unavailable)
}

fn transfer(
    connection: &Connection,
    id: &str,
    now: i64,
) -> Result<(DerivedTransfer, Md5, u32), ObjectError> {
    let (total, offset, state, crc, created, deadline): (i64, i64, Vec<u8>, u32, i64, i64) = connection
        .query_row(
            "SELECT total, offset, md5_state, crc32c, created, deadline FROM derived_transfers WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .optional()
        .map_err(unavailable)?
        .ok_or(ObjectError::DerivedTransferNotFound)?;
    if now >= deadline {
        return Err(ObjectError::DerivedTransferExpired);
    }
    let state = SerializedState::<Md5>::try_from(state.as_slice()).map_err(unavailable)?;
    let md5 = Md5::deserialize(&state).map_err(unavailable)?;
    Ok((
        DerivedTransfer {
            id: id.into(),
            total: total as u64,
            offset: offset as u64,
            created: system_time(created).map_err(unavailable)?,
            deadline: system_time(deadline).map_err(unavailable)?,
        },
        md5,
        crc,
    ))
}

impl Catalog {
    pub fn guard_derived_request(&self, id: &str) -> DerivedRequestGuard {
        DerivedRequestGuard {
            catalog: self.clone(),
            id: Some(id.into()),
        }
    }
    /// Call only from a publication callback: the caller's transaction owns all checks/mutations.
    pub fn check_derived_source_in_transaction(
        connection: &Connection,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        objects::selected(
            connection, bucket, name, generation, conditions, false, access,
        )
        .map(|value| value.0)
    }

    /// Hard deletion is intentionally separate from versioning retirement. Any later callback
    /// failure rolls this deletion back together with the destination and deletion journal.
    pub fn delete_derived_source_in_transaction(
        connection: &Connection,
        bucket: &str,
        name: &str,
        generation: i64,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        let (object, id) = objects::selected(
            connection,
            bucket,
            name,
            Some(generation),
            conditions,
            false,
            access,
        )?;
        if object.event_based_hold {
            return Err(ObjectError::ActiveHold);
        }
        connection
            .execute("DELETE FROM objects WHERE blob_id = ?1", [&id])
            .map_err(unavailable)?;
        connection
            .execute("INSERT INTO pending_blob_deletions VALUES (?1)", [&id])
            .map_err(unavailable)?;
        connection.execute("INSERT INTO derived_deletions SELECT ?1, id FROM derived_transfers WHERE publishing = 1", [&id]).map_err(unavailable)?;
        Ok(())
    }

    /// Source NotMatch conditions are mutation conditions (412), not conditional GETs (304).
    pub async fn resolve_derived_source(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<DerivedSource, ObjectError> {
        self.resolve_derived_sources(vec![DerivedSourceSpec {
            bucket: bucket.into(),
            name: name.into(),
            generation,
            conditions,
            access: access.clone(),
        }])
        .await
        .map(|mut sources| sources.remove(0))
    }

    /// A compose request observes one catalog snapshot for all source generations and metadata.
    pub async fn resolve_derived_sources(
        &self,
        sources: Vec<DerivedSourceSpec>,
    ) -> Result<Vec<DerivedSource>, ObjectError> {
        if sources.is_empty() || sources.len() > 32 {
            return Err(ObjectError::InvalidDerivedArgument(
                "derived request requires 1 to 32 sources".into(),
            ));
        }
        let data_dir = self.0.data_dir.clone();
        self.run_object(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            sources
                .into_iter()
                .map(|source| {
                    let (object, original) = objects::selected(
                        &transaction,
                        &source.bucket,
                        &source.name,
                        source.generation,
                        source.conditions,
                        false,
                        &source.access,
                    )?;
                    let id = random_id()?;
                    let path = stage_path(&data_dir, &id)?;
                    fs::hard_link(objects::blob_path(&data_dir, &original)?, &path)
                        .map_err(unavailable)?;
                    Ok(DerivedSource {
                        object,
                        path: Some(path),
                        id,
                    })
                })
                .collect()
        })
        .await
    }

    /// Input handles are consumed only after durable ownership commits. No source catalog row
    /// is needed on later calls, so a token can safely outlive source overwrite or deletion.
    pub async fn begin_derived(
        &self,
        segments: Vec<DerivedSegment>,
        max_bytes: u64,
    ) -> Result<DerivedTransfer, ObjectError> {
        self.begin_derived_with_kind(segments, max_bytes, false)
            .await
    }

    /// Single-call requests have no continuation owner after a process crash.
    pub async fn begin_derived_ephemeral(
        &self,
        segments: Vec<DerivedSegment>,
        max_bytes: u64,
    ) -> Result<DerivedTransfer, ObjectError> {
        self.begin_derived_with_kind(segments, max_bytes, true)
            .await
    }

    async fn begin_derived_with_kind(
        &self,
        segments: Vec<DerivedSegment>,
        max_bytes: u64,
        ephemeral: bool,
    ) -> Result<DerivedTransfer, ObjectError> {
        self.begin_derived_with_binding(segments, max_bytes, ephemeral, |_, _| Ok(()))
            .await
    }

    /// Request/session bindings commit with transfer ownership, preventing untracked tokens.
    pub async fn begin_derived_with_binding(
        &self,
        mut segments: Vec<DerivedSegment>,
        max_bytes: u64,
        ephemeral: bool,
        bind: impl FnOnce(&Connection, &DerivedTransfer) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<DerivedTransfer, ObjectError> {
        if segments.is_empty() || segments.len() > 32 {
            return Err(ObjectError::InvalidDerivedArgument(
                "derived transfer requires 1 to 32 sources".into(),
            ));
        }
        let total = segments.iter().try_fold(0_u64, |total, segment| {
            if segment.offset > segment.source.object.size
                || segment.length > segment.source.object.size - segment.offset
            {
                return Err(ObjectError::InvalidDerivedArgument(
                    "derived source range is out of bounds".into(),
                ));
            }
            total
                .checked_add(segment.length)
                .filter(|total| *total <= max_bytes && *total <= i64::MAX as u64)
                .ok_or(ObjectError::TooLarge)
        })?;
        let _permit = self.admit_object_upload()?;
        let data_dir = self.0.data_dir.clone();
        let created = unix_millis(self.0.clock.now()).map_err(unavailable)?;
        let deadline = created
            .checked_add(LIFETIME_MILLIS)
            .ok_or_else(|| unavailable("derived deadline overflow"))?;
        let result = self
            .run_object(move |connection| {
                let count: i64 = connection
                    .query_row("SELECT count(*) FROM derived_transfers", [], |row| {
                        row.get(0)
                    })
                    .map_err(unavailable)?;
                if count >= MAX_TRANSFERS as i64 {
                    return Err(unavailable("derived transfer limit reached"));
                }
                let id = random_id()?;
                let path = stage_path(&data_dir, &id)?;
                fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)
                    .and_then(|file| file.sync_all())
                    .map_err(unavailable)?;
                let mut preserve = false;
                let result = (|| {
                    #[cfg(unix)]
                    objects::sync_dir(&data_dir.join("staging")).map_err(unavailable)?;
                    let transaction = connection
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .map_err(unavailable)?;
                    transaction
                    .execute(
                        "INSERT INTO derived_transfers VALUES (?1, ?2, 0, ?3, 0, ?4, ?5, ?6, 0)",
                        params![
                            id,
                            total as i64,
                            Md5::new().serialize().as_slice(),
                            ephemeral, created, deadline
                        ],
                    )
                    .map_err(unavailable)?;
                    for (position, segment) in segments.iter().enumerate() {
                        transaction
                            .execute(
                                "INSERT INTO derived_inputs VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                                params![
                                    id,
                                    position as i64,
                                    segment.source.id,
                                    segment.offset as i64,
                                    segment.length as i64,
                                    segment.source.object.size as i64
                                ],
                            )
                            .map_err(unavailable)?;
                    }
                    let transfer = DerivedTransfer {
                        id: id.clone(),
                        total,
                        offset: 0,
                        created: system_time(created).map_err(unavailable)?,
                        deadline: system_time(deadline).map_err(unavailable)?,
                    };
                    bind(&transaction, &transfer)?;
                    if let Err(error) = transaction.commit() {
                        // A failed commit can be ambiguous; retain every confirmed reference.
                        preserve = true;
                        let committed: bool = connection
                            .query_row(
                                "SELECT EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1)",
                                [&id],
                                |row| row.get(0),
                            )
                            .map_err(unavailable)?;
                        if !committed {
                            preserve = false;
                            return Err(unavailable(error));
                        }
                    }
                    Ok(transfer)
                })();
                match result {
                    Ok(transfer) => {
                        for segment in &mut segments {
                            segment.source.path = None;
                        }
                        Ok(transfer)
                    }
                    Err(error) => {
                        if preserve {
                            for segment in &mut segments {
                                segment.source.path = None;
                            }
                            return Err(error);
                        }
                        objects::unlink(&path).map_err(unavailable)?;
                        Err(error)
                    }
                }
            })
            .await;
        self.0.maintenance_wake.notify_one();
        result
    }

    fn derived_guard(&self, id: &str) -> Result<WriterGuard, ObjectError> {
        let id = format!("derived:{id}");
        let mut writers = self.0.session_writers.lock().map_err(unavailable)?;
        if !writers.insert(id.clone()) {
            return Err(ObjectError::DerivedTransferBusy);
        }
        Ok(WriterGuard {
            catalog: self.clone(),
            id,
        })
    }

    pub async fn get_derived(&self, id: &str) -> Result<DerivedTransfer, ObjectError> {
        let id = id.to_owned();
        let now = unix_millis(self.0.clock.now()).map_err(unavailable)?;
        self.run_object(move |connection| transfer(connection, &id, now).map(|value| value.0))
            .await
    }

    /// Every durable offset includes the matching incremental hash state. File fsync precedes
    /// SQLite commit; an interrupted tail is discarded before retry, never counted twice.
    pub async fn advance_derived(
        &self,
        id: &str,
        budget: u64,
    ) -> Result<DerivedTransfer, ObjectError> {
        let permit = self.admit_object_upload()?;
        let guard = self.derived_guard(id)?;
        let catalog = self.clone();
        let id = id.to_owned();
        // Detached ownership keeps cancellation from racing directory closure or cleanup.
        tokio::spawn(async move {
            let (_permit, _guard) = (permit, guard);
            let query = id.clone();
            let data_dir = catalog.0.data_dir.clone();
            let now = unix_millis(catalog.0.clock.now()).map_err(unavailable)?;
            let (mut progress, mut md5, mut crc, sources) = catalog.run_upload(move |connection| {
                let (progress, md5, crc) = transfer(connection, &query, now)?;
                restore_stage(&data_dir, &query)?;
                Ok((progress, md5, crc, inputs(connection, &query)?))
            }).await?;
            let path = stage_path(&catalog.0.data_dir, &id)?;
            let mut output = tokio::fs::OpenOptions::new().write(true).open(path).await.map_err(unavailable)?;
            output.set_len(progress.offset).await.map_err(unavailable)?;
            output.seek(io::SeekFrom::Start(progress.offset)).await.map_err(unavailable)?;
            let target = progress.offset.saturating_add(budget).min(progress.total);
            let mut skip = progress.offset;
            let mut buffer = [0_u8; BUFFER_SIZE];
            for source in sources {
                if skip >= source.length { skip -= source.length; continue; }
                if progress.offset == target { break; }
                let mut input = tokio::fs::File::open(stage_path(&catalog.0.data_dir, &source.id)?).await.map_err(unavailable)?;
                input.seek(io::SeekFrom::Start(source.start + skip)).await.map_err(unavailable)?;
                let mut remaining = (source.length - skip).min(target - progress.offset);
                skip = 0;
                while remaining > 0 {
                    if catalog.0.admission.is_closed() { return Err(unavailable("catalog is closing")); }
                    let length = remaining.min(BUFFER_SIZE as u64) as usize;
                    input.read_exact(&mut buffer[..length]).await.map_err(unavailable)?;
                    output.write_all(&buffer[..length]).await.map_err(unavailable)?;
                    md5.update(&buffer[..length]);
                    crc = crc32c::crc32c_append(crc, &buffer[..length]);
                    progress.offset += length as u64;
                    remaining -= length as u64;
                }
            }
            output.sync_all().await.map_err(unavailable)?;
            catalog.run_upload(move |connection| {
                connection.execute("UPDATE derived_transfers SET offset = ?2, md5_state = ?3, crc32c = ?4 WHERE id = ?1", params![id, progress.offset as i64, md5.serialize().as_slice(), crc]).map_err(unavailable)?;
                Ok(progress)
            }).await
        }).await.map_err(unavailable)?
    }

    /// The callback joins protocol-specific completion state to the same destination commit.
    pub async fn publish_derived(
        &self,
        id: &str,
        destination: DerivedDestination,
        complete: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<ObjectRecord, ObjectError> {
        self.publish_derived_with_validation(id, destination, |_, _| Ok(()), complete)
            .await
    }

    /// Validation precedes retirement so same-name sources remain visible in unversioned buckets.
    pub async fn publish_derived_with_validation(
        &self,
        id: &str,
        destination: DerivedDestination,
        validate: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        complete: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<ObjectRecord, ObjectError> {
        let permit = self.admit_object_upload()?;
        let guard = self.derived_guard(id)?;
        let catalog = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let (_permit, _guard) = (permit, guard);
            let query = id.clone();
            let data_dir = catalog.0.data_dir.clone();
            let clock = catalog.0.clock.clone();
            let result = catalog.run_upload(move |connection| {
                let (progress, md5, crc32c) = transfer(connection, &query, unix_millis(clock.now()).map_err(unavailable)?)?;
                let sources = inputs(connection, &query)?;
                if progress.offset != progress.total { return Err(ObjectError::DerivedTransferIncomplete); }
                let md5: [u8; 16] = md5.finalize().into();
                restore_stage(&data_dir, &query)?;
                let stage = stage_path(&data_dir, &query)?;
                objects::publish_blob(&stage, &objects::blob_path(&data_dir, &query)?).map_err(unavailable)?;
                let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
                let (default_class, allow_overwrite) = objects::authorized_bucket(&transaction, &destination.bucket, &destination.access)?;
                let (versioning, default_hold) = Catalog::bucket_settings_in_transaction(&transaction, &destination.bucket).map_err(unavailable)?;
                let previous = objects::lookup(&transaction, &destination.bucket, &destination.name)?;
                objects::check_publication(previous.as_ref(), destination.conditions, allow_overwrite)?;
                if destination.expected.md5.is_some_and(|hash| hash != md5) || destination.expected.crc32c.is_some_and(|hash| hash != crc32c) { return Err(ObjectError::ChecksumMismatch); }
                let now = clock.now();
                let micros = i64::try_from(now.duration_since(UNIX_EPOCH).map_err(unavailable)?.as_micros()).map_err(unavailable)?;
                let last: i64 = transaction.query_row("SELECT last_generation FROM object_generation WHERE singleton = 1", [], |row| row.get(0)).map_err(unavailable)?;
                let generation = micros.max(last.checked_add(1).ok_or_else(|| unavailable("generation exhausted"))?);
                let millis = unix_millis(now).map_err(unavailable)?;
                let time = system_time(millis).map_err(unavailable)?;
                let storage_class = destination.storage_class.unwrap_or(default_class);
                crate::bucket::validate_storage_class(&storage_class).map_err(|error| ObjectError::UnsupportedMetadata(error.to_string()))?;
                let mut record = ObjectRecord {
                    bucket: destination.bucket, name: destination.name, generation, metageneration: 1,
                    content_type: destination.content_type, size: progress.total, md5, crc32c, storage_class,
                    component_count: destination.component_count,
                    content_encoding: None, content_disposition: None, content_language: None,
                    cache_control: None, metadata: Default::default(), time_created: time, updated: time,
                    time_deleted: None, event_based_hold: default_hold,
                };
                destination.metadata.apply(&mut record)?;
                let metadata = serde_json::to_string(&record.metadata).map_err(unavailable)?;
                transaction.execute("UPDATE derived_transfers SET publishing = 1 WHERE id = ?1", [&query]).map_err(unavailable)?;
                validate(&transaction, &record)?;
                let retired = objects::retire_previous(&transaction, previous.as_ref(), versioning, millis)?;
                transaction.execute("UPDATE object_generation SET last_generation = ?1 WHERE singleton = 1", [generation]).map_err(unavailable)?;
                transaction.execute("INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,content_encoding,content_disposition,content_language,cache_control,metadata,event_based_hold,component_count) VALUES (?1,?2,?3,1,?4,?5,?6,?7,?8,?9,?9,?10,?11,?12,?13,?14,?15,?16,?17)", params![record.bucket, record.name, generation, record.content_type, record.size as i64, &md5[..], crc32c, record.storage_class, millis, query, record.content_encoding, record.content_disposition, record.content_language, record.cache_control, metadata, record.event_based_hold, record.component_count]).map_err(unavailable)?;
                complete(&transaction, &record)?;
                transaction.execute("DELETE FROM derived_inputs WHERE transfer_id = ?1", [&query]).map_err(unavailable)?;
                transaction.execute("DELETE FROM derived_transfers WHERE id = ?1", [&query]).map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
                for source in sources {
                    if let Err(error) = objects::unlink(&stage_path(&data_dir, &source.id)?) {
                        eprintln!("deferred derived source cleanup: {error}");
                    }
                }
                if let Some(old) = retired && let Err(error) = objects::reclaim(connection, &data_dir, &old) { eprintln!("deferred blob reclaim {old}: {error}"); }
                if let Err(error) = reclaim_derived_deletions(connection, &data_dir, Some(&query)) {
                    eprintln!("deferred derived source reclamation: {error}");
                }
                Ok(record)
            }).await;
            catalog.0.maintenance_wake.notify_one();
            // Check SQL references even after an ambiguous commit error before removing bytes.
            if matches!(&result, Err(error) if !matches!(error, ObjectError::Unavailable(_) | ObjectError::DerivedTransferBusy | ObjectError::DerivedTransferIncomplete | ObjectError::DerivedTransferNotFound))
                && let Err(error) = catalog.cleanup_derived(&id).await {
                eprintln!("deferred derived cleanup: {error}");
            }
            result
        }).await.map_err(unavailable)?
    }

    async fn cleanup_derived(&self, id: &str) -> Result<(), ObjectError> {
        let id = id.to_owned();
        let data_dir = self.0.data_dir.clone();
        self.run_upload(move |connection| cleanup_record(connection, &data_dir, &id))
            .await
    }

    pub async fn cancel_derived(&self, id: &str) -> Result<(), ObjectError> {
        let _guard = self.derived_guard(id)?;
        self.cleanup_derived(id).await
    }

    /// The existing maintenance scheduler includes these deadlines and sleeps until work is due.
    pub async fn maintain_derived_transfers(
        &self,
    ) -> Result<Option<std::time::SystemTime>, crate::bucket::CatalogError> {
        let now = unix_millis(self.0.clock.now())?;
        self.maintain_derived_transfers_at(now).await
    }

    pub(super) async fn maintain_derived_transfers_at(
        &self,
        now: i64,
    ) -> Result<Option<std::time::SystemTime>, crate::bucket::CatalogError> {
        let data_dir = self.0.data_dir.clone();
        self.run_object(move |connection| reclaim_derived_deletions(connection, &data_dir, None))
            .await
            .map_err(super::unavailable)?;
        let due = self.run_object(move |connection| {
            connection.prepare("SELECT id FROM derived_transfers WHERE deadline <= ?1 ORDER BY id LIMIT 128").map_err(unavailable)?
                .query_map([now], |row| row.get::<_, String>(0)).map_err(unavailable)?.collect::<rusqlite::Result<Vec<_>>>().map_err(unavailable)
        }).await.map_err(super::unavailable)?;
        for id in due {
            let _guard = self.derived_guard(&id).map_err(super::unavailable)?;
            self.cleanup_derived(&id)
                .await
                .map_err(super::unavailable)?;
        }
        self.run(move |connection| {
            let deadline: Option<i64> = connection
                .query_row("SELECT min(deadline) FROM derived_transfers", [], |row| {
                    row.get(0)
                })
                .map_err(super::unavailable)?;
            let pending: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM derived_deletions)",
                    [],
                    |row| row.get(0),
                )
                .map_err(super::unavailable)?;
            let deadline = if pending {
                Some(deadline.map_or(now, |deadline| deadline.min(now)))
            } else {
                deadline
            };
            deadline
                .map(system_time)
                .transpose()
                .map_err(super::unavailable)
        })
        .await
    }
}

fn reclaim_derived_deletions(
    connection: &Connection,
    data_dir: &Path,
    transfer_id: Option<&str>,
) -> Result<(), ObjectError> {
    let ids = connection.prepare("SELECT blob_id FROM derived_deletions WHERE (?1 IS NULL OR transfer_id = ?1) ORDER BY blob_id LIMIT 128").map_err(unavailable)?
        .query_map([transfer_id], |row| row.get::<_, String>(0)).map_err(unavailable)?.collect::<rusqlite::Result<Vec<_>>>().map_err(unavailable)?;
    let mut failure = None;
    for id in ids {
        match objects::reclaim(connection, data_dir, &id) {
            Ok(()) => {
                connection
                    .execute("DELETE FROM derived_deletions WHERE blob_id = ?1", [id])
                    .map_err(unavailable)?;
            }
            Err(error) => failure = Some(error),
        }
    }
    failure.map_or(Ok(()), Err)
}

fn cleanup_record(connection: &Connection, data_dir: &Path, id: &str) -> Result<(), ObjectError> {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1)",
            [id],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !exists {
        return Err(ObjectError::DerivedTransferNotFound);
    }
    for source in inputs(connection, id)? {
        objects::unlink(&stage_path(data_dir, &source.id)?).map_err(unavailable)?;
    }
    objects::unlink(&stage_path(data_dir, id)?).map_err(unavailable)?;
    let referenced: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)",
            [id],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if !referenced {
        objects::reclaim(connection, data_dir, id)?;
    }
    discard_record(connection, id)
}

fn discard_record(connection: &Connection, id: &str) -> Result<(), ObjectError> {
    let transaction = connection.unchecked_transaction().map_err(unavailable)?;
    transaction
        .execute("DELETE FROM derived_inputs WHERE transfer_id = ?1", [id])
        .map_err(unavailable)?;
    transaction
        .execute("DELETE FROM derived_transfers WHERE id = ?1", [id])
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

fn restore_stage(data_dir: &Path, id: &str) -> Result<PathBuf, ObjectError> {
    let stage = stage_path(data_dir, id)?;
    if !stage.try_exists().map_err(unavailable)? {
        fs::rename(objects::blob_path(data_dir, id)?, &stage).map_err(unavailable)?;
    }
    Ok(stage)
}

pub(super) fn recover(
    connection: &Connection,
    data_dir: &Path,
    now: i64,
) -> Result<(), ObjectError> {
    reclaim_derived_deletions(connection, data_dir, None)?;
    let ids = connection
        .prepare("SELECT id FROM derived_transfers ORDER BY id LIMIT 129")
        .map_err(unavailable)?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(unavailable)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(unavailable)?;
    if ids.len() > MAX_TRANSFERS {
        return Err(unavailable("too many derived transfers"));
    }
    for id in ids {
        let (ephemeral, deadline): (bool, i64) = connection
            .query_row(
                "SELECT ephemeral, deadline FROM derived_transfers WHERE id = ?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(unavailable)?;
        if ephemeral || deadline <= now {
            discard_record(connection, &id)?;
            continue;
        }
        let validity = (|| {
            let (progress, saved_md5, saved_crc) = transfer(connection, &id, now)?;
            let sources = inputs(connection, &id)?;
            if sources.is_empty()
                || sources.len() > 32
                || sources.iter().map(|source| source.length).sum::<u64>() != progress.total
            {
                return Err(unavailable("invalid derived sources"));
            }
            let stage = restore_stage(data_dir, &id)?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .read(true)
                .open(stage)
                .map_err(unavailable)?;
            if output.metadata().map_err(unavailable)?.len() < progress.offset {
                return Err(unavailable("derived prefix is shorter than its checkpoint"));
            }
            output.set_len(progress.offset).map_err(unavailable)?;
            output.sync_all().map_err(unavailable)?;
            // A same-length corrupted prefix must not inherit the acknowledged hash state.
            let mut md5 = Md5::new();
            let mut crc = 0;
            let mut remaining = progress.offset;
            let mut buffer = [0; BUFFER_SIZE];
            while remaining > 0 {
                let length = remaining.min(BUFFER_SIZE as u64) as usize;
                std::io::Read::read_exact(&mut output, &mut buffer[..length])
                    .map_err(unavailable)?;
                md5.update(&buffer[..length]);
                crc = crc32c::crc32c_append(crc, &buffer[..length]);
                remaining -= length as u64;
            }
            if md5.finalize() != saved_md5.finalize() || crc != saved_crc {
                return Err(unavailable(
                    "derived prefix checksum differs from checkpoint",
                ));
            }
            for source in sources {
                let metadata =
                    fs::symlink_metadata(stage_path(data_dir, &source.id)?).map_err(unavailable)?;
                if !metadata.is_file() || metadata.len() != source.size {
                    return Err(unavailable("invalid pinned derived source"));
                }
            }
            Ok::<_, ObjectError>(())
        })();
        if let Err(error) = validity {
            eprintln!("discarding invalid derived transfer {id}: {error}");
            discard_record(connection, &id)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bucket::BucketRecord, object::UploadChunk};
    use bytes::Bytes;
    use std::io::Write;
    use tokio::sync::mpsc;

    async fn setup() -> (Catalog, PathBuf) {
        let path = std::env::temp_dir().join(format!("gcs-mimic-derived-{}", random_id().unwrap()));
        let catalog = Catalog::open(&path).await.unwrap();
        catalog
            .insert_bucket(BucketRecord {
                name: "bucket".into(),
                project: "project".into(),
                location: "US".into(),
                location_type: "multi-region".into(),
                storage_class: "STANDARD".into(),
                labels: Default::default(),
                versioning_enabled: false,
                default_event_based_hold: false,
                metageneration: 1,
                time_created: UNIX_EPOCH,
                updated: UNIX_EPOCH,
            })
            .await
            .unwrap();
        (catalog, path)
    }

    async fn put(catalog: &Catalog, name: &str, bytes: &'static [u8]) -> ObjectRecord {
        let (sender, body) = mpsc::channel(2);
        sender
            .try_send(Ok(UploadChunk::Data(Bytes::from_static(bytes))))
            .unwrap();
        sender.try_send(Ok(UploadChunk::End)).unwrap();
        catalog
            .put_object(
                "bucket",
                name,
                "text/plain".into(),
                Default::default(),
                Default::default(),
                1_000_000,
                body,
            )
            .await
            .unwrap()
    }

    async fn source(catalog: &Catalog, name: &str) -> DerivedSource {
        catalog
            .resolve_derived_source(
                "bucket",
                name,
                None,
                Default::default(),
                &BucketAccess::Unchecked,
            )
            .await
            .unwrap()
    }

    fn destination(name: &str) -> DerivedDestination {
        DerivedDestination {
            bucket: "bucket".into(),
            name: name.into(),
            content_type: "text/plain".into(),
            ..Default::default()
        }
    }

    async fn ready(catalog: &Catalog, name: &str) -> DerivedTransfer {
        let transfer = catalog
            .begin_derived(vec![source(catalog, name).await.whole()], u64::MAX)
            .await
            .unwrap();
        catalog
            .advance_derived(&transfer.id, u64::MAX)
            .await
            .unwrap()
    }

    async fn bytes(catalog: &Catalog, name: &str) -> Vec<u8> {
        let (_, mut reader) = catalog
            .open_object("bucket", name, None, Default::default())
            .await
            .unwrap();
        let mut bytes = Vec::new();
        while let Some(chunk) = reader.next_chunk().await.unwrap() {
            bytes.extend_from_slice(&chunk);
        }
        bytes
    }

    #[tokio::test]
    async fn pinned_ranges_resume_after_source_deletion_restart_and_uncommitted_tail() {
        let (catalog, path) = setup().await;
        let first = put(&catalog, "first", b"abcdef").await;
        put(&catalog, "second", b"12345").await;
        let transfer = catalog
            .begin_derived(
                vec![
                    DerivedSegment {
                        source: source(&catalog, "first").await,
                        offset: 1,
                        length: 4,
                    },
                    DerivedSegment {
                        source: source(&catalog, "second").await,
                        offset: 2,
                        length: 3,
                    },
                ],
                7,
            )
            .await
            .unwrap();
        assert_eq!(
            catalog
                .advance_derived(&transfer.id, 3)
                .await
                .unwrap()
                .offset,
            3
        );
        catalog
            .delete_object(
                "bucket",
                "first",
                Some(first.generation),
                Default::default(),
            )
            .await
            .unwrap();
        put(&catalog, "second", b"changed").await;
        let output = stage_path(&path, &transfer.id).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&output)
            .unwrap()
            .write_all(b"uncommitted")
            .unwrap();
        catalog.close().await.unwrap();
        drop(catalog);
        let catalog = Catalog::open(&path).await.unwrap();
        assert_eq!(fs::metadata(&output).unwrap().len(), 3);
        assert_eq!(
            catalog
                .advance_derived(&transfer.id, 2)
                .await
                .unwrap()
                .offset,
            5
        );
        catalog
            .advance_derived(&transfer.id, u64::MAX)
            .await
            .unwrap();
        let record = catalog
            .publish_derived(&transfer.id, destination("copy"), |_, _| Ok(()))
            .await
            .unwrap();
        assert_eq!(bytes(&catalog, "copy").await, b"bcde345");
        assert_eq!(record.md5, <[u8; 16]>::from(Md5::digest(b"bcde345")));
        assert_eq!(record.crc32c, crc32c::crc32c(b"bcde345"));
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn recovery_restores_renamed_uncommitted_output_and_empty_sources() {
        let (catalog, path) = setup().await;
        put(&catalog, "empty", b"").await;
        let transfer = ready(&catalog, "empty").await;
        objects::publish_blob(
            &stage_path(&path, &transfer.id).unwrap(),
            &objects::blob_path(&path, &transfer.id).unwrap(),
        )
        .unwrap();
        catalog.close().await.unwrap();
        drop(catalog);
        let catalog = Catalog::open(&path).await.unwrap();
        let object = catalog
            .publish_derived(&transfer.id, destination("empty-copy"), |_, _| Ok(()))
            .await
            .unwrap();
        assert_eq!(object.size, 0);
        assert_eq!(object.md5, <[u8; 16]>::from(Md5::digest(b"")));
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn destination_race_has_one_winner_and_failed_publication_cleans_staging() {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"contents").await;
        let left = ready(&catalog, "source").await;
        let right = ready(&catalog, "source").await;
        let mut target = destination("target");
        target.conditions.if_generation_match = Some(0);
        let (left, right) = tokio::join!(
            catalog.publish_derived(&left.id, target.clone(), |_, _| Ok(())),
            catalog.publish_derived(&right.id, target, |_, _| Ok(())),
        );
        assert!(matches!(
            (&left, &right),
            (Ok(_), Err(ObjectError::PreconditionFailed))
                | (Err(ObjectError::PreconditionFailed), Ok(_))
        ));
        assert_eq!(bytes(&catalog, "target").await, b"contents");
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        let transfer = ready(&catalog, "source").await;
        let mut target = destination("target");
        target.expected.crc32c = Some(0);
        assert_eq!(
            catalog
                .publish_derived(&transfer.id, target, |_, _| Ok(()))
                .await,
            Err(ObjectError::ChecksumMismatch)
        );
        assert_eq!(bytes(&catalog, "target").await, b"contents");
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn callback_source_deletion_rolls_back_with_destination_and_checks_holds() {
        let (catalog, path) = setup().await;
        let original = put(&catalog, "source", b"source").await;
        put(&catalog, "target", b"original").await;
        let transfer = ready(&catalog, "source").await;
        let generation = original.generation;
        let result = catalog
            .publish_derived(&transfer.id, destination("target"), move |connection, _| {
                Catalog::delete_derived_source_in_transaction(
                    connection,
                    "bucket",
                    "source",
                    generation,
                    Default::default(),
                    &BucketAccess::Unchecked,
                )?;
                Err(ObjectError::PreconditionFailed)
            })
            .await;
        assert_eq!(result, Err(ObjectError::PreconditionFailed));
        assert_eq!(bytes(&catalog, "target").await, b"original");
        assert_eq!(bytes(&catalog, "source").await, b"source");
        catalog
            .update_object_metadata(
                "bucket",
                "source",
                None,
                Default::default(),
                ObjectMetadataUpdate {
                    event_based_hold: Some(true),
                    ..Default::default()
                },
                |_| Ok(()),
            )
            .await
            .unwrap();
        let transfer = ready(&catalog, "source").await;
        let result = catalog
            .publish_derived(&transfer.id, destination("target"), move |connection, _| {
                Catalog::delete_derived_source_in_transaction(
                    connection,
                    "bucket",
                    "source",
                    generation,
                    Default::default(),
                    &BucketAccess::Unchecked,
                )
            })
            .await;
        assert_eq!(result, Err(ObjectError::ActiveHold));
        assert_eq!(bytes(&catalog, "target").await, b"original");
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn all_source_conditions_are_checked_before_pinning_and_ranges_are_validated() {
        let (catalog, path) = setup().await;
        let original = put(&catalog, "source", b"source").await;
        for conditions in [
            ObjectConditions {
                if_generation_match: Some(original.generation + 1),
                ..Default::default()
            },
            ObjectConditions {
                if_generation_not_match: Some(original.generation),
                ..Default::default()
            },
            ObjectConditions {
                if_metageneration_match: Some(2),
                ..Default::default()
            },
            ObjectConditions {
                if_metageneration_not_match: Some(1),
                ..Default::default()
            },
        ] {
            assert!(matches!(
                catalog
                    .resolve_derived_source(
                        "bucket",
                        "source",
                        None,
                        conditions,
                        &BucketAccess::Unchecked
                    )
                    .await,
                Err(ObjectError::PreconditionFailed)
            ));
        }
        let denied = BucketAccess::Policy {
            identity: None,
            permission: crate::auth::OBJECTS_GET.into(),
            project: None,
        };
        assert!(matches!(
            catalog
                .resolve_derived_source("bucket", "source", None, Default::default(), &denied)
                .await,
            Err(ObjectError::Forbidden)
        ));
        assert!(
            catalog
                .begin_derived(
                    vec![DerivedSegment {
                        source: source(&catalog, "source").await,
                        offset: 5,
                        length: 2
                    }],
                    100
                )
                .await
                .is_err()
        );
        assert!(matches!(
            catalog
                .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
                .await,
            Err(ObjectError::TooLarge)
        ));
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn validation_sees_same_name_source_before_retirement_and_completion_sees_destination() {
        let (catalog, path) = setup().await;
        let original = put(&catalog, "same", b"same bytes").await;
        let transfer = ready(&catalog, "same").await;
        let generation = original.generation;
        let mut target = destination("same");
        target.component_count = Some(2);
        let record = catalog
            .publish_derived_with_validation(
                &transfer.id,
                target,
                move |connection, destination| {
                    let source = Catalog::check_derived_source_in_transaction(
                        connection,
                        "bucket",
                        "same",
                        Some(generation),
                        Default::default(),
                        &BucketAccess::Unchecked,
                    )?;
                    assert_eq!(source.generation, generation);
                    assert_ne!(source.generation, destination.generation);
                    Ok(())
                },
                move |connection, destination| {
                    let live = Catalog::check_derived_source_in_transaction(
                        connection,
                        "bucket",
                        "same",
                        None,
                        Default::default(),
                        &BucketAccess::Unchecked,
                    )?;
                    assert_eq!(live, *destination);
                    Ok(())
                },
            )
            .await
            .unwrap();
        assert_eq!(record.component_count, Some(2));
        let value = crate::json::object_resource(
            &record,
            "http://localhost",
            crate::json::Projection::NoAcl,
        )
        .unwrap();
        assert_eq!(value["componentCount"], 2);
        assert!(value.get("md5Hash").is_none());
        assert!(
            !crate::json::object_media_headers(&record).unwrap()["x-goog-hash"]
                .to_str()
                .unwrap()
                .contains("md5=")
        );
        catalog.close().await.unwrap();
        drop(catalog);
        let catalog = Catalog::open(&path).await.unwrap();
        assert_eq!(
            catalog
                .get_object("bucket", "same", None, Default::default())
                .await
                .unwrap(),
            record
        );
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn restart_reclaims_ephemeral_transfers_and_preserves_continuable_transfers() {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"bytes").await;
        let ephemeral = catalog
            .begin_derived_ephemeral(vec![source(&catalog, "source").await.whole()], 5)
            .await
            .unwrap();
        catalog.advance_derived(&ephemeral.id, 2).await.unwrap();
        let persistent = catalog
            .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
            .await
            .unwrap();
        catalog.close().await.unwrap();
        drop(catalog);
        let catalog = Catalog::open(&path).await.unwrap();
        assert_eq!(
            catalog.get_derived(&ephemeral.id).await,
            Err(ObjectError::DerivedTransferNotFound)
        );
        assert_eq!(
            catalog.get_derived(&persistent.id).await.unwrap(),
            persistent
        );
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 2);
        catalog.cancel_derived(&persistent.id).await.unwrap();
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn batch_source_resolution_checks_every_source_and_cleans_partial_pins() {
        let (catalog, path) = setup().await;
        let original = put(&catalog, "source", b"bytes").await;
        let spec = DerivedSourceSpec {
            bucket: "bucket".into(),
            name: "source".into(),
            generation: Some(original.generation),
            conditions: Default::default(),
            access: BucketAccess::Unchecked,
        };
        let mut missing = spec.clone();
        missing.name = "missing".into();
        assert!(matches!(
            catalog
                .resolve_derived_sources(vec![spec.clone(), missing])
                .await,
            Err(ObjectError::ObjectNotFound)
        ));
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        let sources = catalog
            .resolve_derived_sources(vec![spec; 32])
            .await
            .unwrap();
        assert_eq!(sources.len(), 32);
        assert!(sources.iter().all(|source| source.object == original));
        drop(sources);
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn unknown_transfer_ids_cannot_remove_another_transfers_source() {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"bytes").await;
        let source = source(&catalog, "source").await;
        let snapshot_id = source.id.clone();
        let transfer = catalog
            .begin_derived(vec![source.whole()], 5)
            .await
            .unwrap();
        assert_eq!(
            catalog.cancel_derived(&snapshot_id).await,
            Err(ObjectError::DerivedTransferNotFound)
        );
        assert_eq!(
            catalog
                .publish_derived(&snapshot_id, destination("copy"), |_, _| Ok(()))
                .await,
            Err(ObjectError::DerivedTransferNotFound)
        );
        catalog
            .advance_derived(&transfer.id, u64::MAX)
            .await
            .unwrap();
        catalog
            .publish_derived(&transfer.id, destination("copy"), |_, _| Ok(()))
            .await
            .unwrap();
        assert_eq!(bytes(&catalog, "copy").await, b"bytes");
        catalog.close().await.unwrap();
        drop(catalog);
        Catalog::open(&path).await.unwrap().close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn incomplete_and_transient_publication_failures_preserve_resumable_progress() {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"bytes").await;
        let transfer = catalog
            .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
            .await
            .unwrap();
        catalog.advance_derived(&transfer.id, 2).await.unwrap();
        assert_eq!(
            catalog
                .publish_derived(&transfer.id, destination("copy"), |_, _| Ok(()))
                .await,
            Err(ObjectError::DerivedTransferIncomplete)
        );
        assert_eq!(catalog.get_derived(&transfer.id).await.unwrap().offset, 2);
        catalog
            .advance_derived(&transfer.id, u64::MAX)
            .await
            .unwrap();
        assert!(matches!(
            catalog
                .publish_derived(&transfer.id, destination("copy"), |_, _| Err(unavailable(
                    "retryable callback failure"
                )))
                .await,
            Err(ObjectError::Unavailable(_))
        ));
        assert_eq!(catalog.get_derived(&transfer.id).await.unwrap().offset, 5);
        catalog.advance_derived(&transfer.id, 0).await.unwrap();
        catalog
            .publish_derived(&transfer.id, destination("copy"), |_, _| Ok(()))
            .await
            .unwrap();
        assert_eq!(bytes(&catalog, "copy").await, b"bytes");
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn request_binding_commits_or_rolls_back_with_transfer_ownership() {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"bytes").await;
        let result = catalog
            .begin_derived_with_binding(
                vec![source(&catalog, "source").await.whole()],
                5,
                false,
                |connection, transfer| {
                    assert_eq!(inputs(connection, &transfer.id)?.len(), 1);
                    connection
                        .execute("UPDATE startup_seed SET complete = 1", [])
                        .map_err(unavailable)?;
                    Err(ObjectError::PreconditionFailed)
                },
            )
            .await;
        assert_eq!(result, Err(ObjectError::PreconditionFailed));
        assert!(!catalog.seed_complete().await.unwrap());
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        let transfer = catalog
            .begin_derived_with_binding(
                vec![source(&catalog, "source").await.whole()],
                5,
                false,
                |connection, transfer| {
                    assert_eq!(inputs(connection, &transfer.id)?.len(), 1);
                    connection
                        .execute("UPDATE startup_seed SET complete = 1", [])
                        .map_err(unavailable)?;
                    Ok(())
                },
            )
            .await
            .unwrap();
        assert!(catalog.seed_complete().await.unwrap());
        catalog.cancel_derived(&transfer.id).await.unwrap();
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn abandoned_handler_guard_waits_for_writer_then_reclaims_transfer() {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"bytes").await;
        let transfer = catalog
            .begin_derived_ephemeral(vec![source(&catalog, "source").await.whole()], 5)
            .await
            .unwrap();
        let writer = catalog.derived_guard(&transfer.id).unwrap();
        let request = catalog.guard_derived_request(&transfer.id);
        drop(request);
        tokio::task::yield_now().await;
        assert!(catalog.get_derived(&transfer.id).await.is_ok());
        drop(writer);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if catalog.get_derived(&transfer.id).await
                    == Err(ObjectError::DerivedTransferNotFound)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn injected_clock_expiry_is_enforced_and_maintenance_reclaims_inputs() {
        use std::sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        };
        struct Clock(AtomicU64);
        impl crate::clock::Clock for Clock {
            fn now(&self) -> std::time::SystemTime {
                UNIX_EPOCH + std::time::Duration::from_secs(self.0.load(Ordering::SeqCst))
            }
        }
        let (catalog, path) = setup().await;
        catalog.close().await.unwrap();
        drop(catalog);
        let clock = Arc::new(Clock(AtomicU64::new(0)));
        let catalog = Catalog::open_with_clock(&path, clock.clone())
            .await
            .unwrap();
        put(&catalog, "source", b"bytes").await;
        let transfer = catalog
            .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
            .await
            .unwrap();
        assert_eq!(
            catalog.maintain_derived_transfers().await.unwrap(),
            Some(UNIX_EPOCH + std::time::Duration::from_secs(604800))
        );
        clock.0.store(604800, Ordering::SeqCst);
        assert_eq!(
            catalog.get_derived(&transfer.id).await,
            Err(ObjectError::DerivedTransferExpired)
        );
        assert_eq!(catalog.maintain_derived_transfers().await.unwrap(), None);
        assert_eq!(
            catalog.get_derived(&transfer.id).await,
            Err(ObjectError::DerivedTransferNotFound)
        );
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn corrupt_transfer_is_discarded_without_blocking_catalog_recovery() {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"bytes").await;
        let source = source(&catalog, "source").await;
        let snapshot = source.path.clone().unwrap();
        let transfer = catalog
            .begin_derived(vec![source.whole()], 5)
            .await
            .unwrap();
        fs::remove_file(snapshot).unwrap();
        catalog.close().await.unwrap();
        drop(catalog);
        let catalog = Catalog::open(&path).await.unwrap();
        assert_eq!(
            catalog.get_derived(&transfer.id).await,
            Err(ObjectError::DerivedTransferNotFound)
        );
        assert_eq!(bytes(&catalog, "source").await, b"bytes");
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn cleanup_failure_preserves_original_publication_error() {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"bytes").await;
        put(&catalog, "copy", b"old").await;
        let source = source(&catalog, "source").await;
        let snapshot = source.path.clone().unwrap();
        let transfer = catalog
            .begin_derived(vec![source.whole()], 5)
            .await
            .unwrap();
        catalog
            .advance_derived(&transfer.id, u64::MAX)
            .await
            .unwrap();
        fs::remove_file(&snapshot).unwrap();
        fs::create_dir(&snapshot).unwrap();
        let mut target = destination("copy");
        target.conditions.if_generation_match = Some(0);
        target.expected.crc32c = Some(0);
        assert_eq!(
            catalog
                .publish_derived(&transfer.id, target, |_, _| Ok(()))
                .await,
            Err(ObjectError::PreconditionFailed)
        );
        assert_eq!(bytes(&catalog, "copy").await, b"old");
        fs::remove_dir(&snapshot).unwrap();
        catalog.cancel_derived(&transfer.id).await.unwrap();
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn recovery_rejects_same_length_corrupted_acknowledged_prefix() {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"bytes").await;
        let transfer = catalog
            .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
            .await
            .unwrap();
        catalog.advance_derived(&transfer.id, 3).await.unwrap();
        fs::write(stage_path(&path, &transfer.id).unwrap(), b"bad").unwrap();
        catalog.close().await.unwrap();
        drop(catalog);
        let catalog = Catalog::open(&path).await.unwrap();
        assert_eq!(
            catalog.get_derived(&transfer.id).await,
            Err(ObjectError::DerivedTransferNotFound)
        );
        assert_eq!(bytes(&catalog, "source").await, b"bytes");
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn committed_callback_deletion_reclaims_source_blob_without_restart() {
        let (catalog, path) = setup().await;
        let original = put(&catalog, "source", b"bytes").await;
        let transfer = ready(&catalog, "source").await;
        catalog
            .publish_derived(&transfer.id, destination("copy"), move |connection, _| {
                Catalog::delete_derived_source_in_transaction(
                    connection,
                    "bucket",
                    "source",
                    original.generation,
                    Default::default(),
                    &BucketAccess::Unchecked,
                )
            })
            .await
            .unwrap();
        assert_eq!(
            catalog
                .get_object("bucket", "source", None, Default::default())
                .await,
            Err(ObjectError::ObjectNotFound)
        );
        assert_eq!(bytes(&catalog, "copy").await, b"bytes");
        let blobs: usize = fs::read_dir(path.join("blobs"))
            .unwrap()
            .map(|shard| fs::read_dir(shard.unwrap().path()).unwrap().count())
            .sum();
        assert_eq!(blobs, 1);
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        assert_eq!(catalog.maintain_derived_transfers().await.unwrap(), None);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn callback_reclamation_failure_preserves_success_and_maintenance_retries() {
        let (catalog, path) = setup().await;
        let original = put(&catalog, "source", b"bytes").await;
        let id = catalog
            .run_object(|connection| {
                Ok(objects::lookup(connection, "bucket", "source")?.unwrap().1)
            })
            .await
            .unwrap();
        let blob = objects::blob_path(&path, &id).unwrap();
        let failed_blob = blob.clone();
        let transfer = ready(&catalog, "source").await;
        catalog
            .publish_derived(&transfer.id, destination("copy"), move |connection, _| {
                Catalog::delete_derived_source_in_transaction(
                    connection,
                    "bucket",
                    "source",
                    original.generation,
                    Default::default(),
                    &BucketAccess::Unchecked,
                )?;
                fs::remove_file(&failed_blob).map_err(unavailable)?;
                fs::create_dir(&failed_blob).map_err(unavailable)?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(bytes(&catalog, "copy").await, b"bytes");
        assert!(catalog.maintain_derived_transfers().await.is_err());
        fs::remove_dir(&blob).unwrap();
        assert_eq!(catalog.maintain_derived_transfers().await.unwrap(), None);
        let tagged = catalog
            .run_object(|connection| {
                connection
                    .query_row("SELECT count(*) FROM derived_deletions", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map_err(unavailable)
            })
            .await
            .unwrap();
        assert_eq!(tagged, 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }
}
