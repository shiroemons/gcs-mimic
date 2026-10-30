//! Generation-pinned, resumable byte derivation shared by copy, rewrite and compose.

use super::{
    Catalog, objects, resumable::WriterGuard, system_time_micros, system_time_millis, unix_micros,
    unix_millis,
};
use crate::{
    bucket::BucketAccess,
    hashing::{Hasher, Hashes},
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate, ObjectRecord},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

const BUFFER_SIZE: usize = 64 * 1024;
const MAX_TRANSFERS_PER_KIND: usize = 128;
const LIFETIME_MILLIS: i64 = 604_800_000;
const EPHEMERAL_LIFETIME_MILLIS: i64 = 900_000;
pub(super) const SCHEMA: &str = "ALTER TABLE objects ADD COLUMN component_count INTEGER CHECK(component_count IS NULL OR component_count > 0);
CREATE TABLE derived_transfers (
    id TEXT PRIMARY KEY CHECK(length(id) = 32),
    total INTEGER NOT NULL CHECK(total >= 0),
    offset INTEGER NOT NULL CHECK(offset >= 0 AND offset <= total),
    md5_state BLOB NOT NULL,
    crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
    ephemeral INTEGER NOT NULL CHECK(ephemeral IN (0, 1)),
    created INTEGER NOT NULL,
    deadline INTEGER NOT NULL CHECK(deadline > created),
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

use crate::storage::failure::object_unavailable;

fn random_id() -> Result<String, ObjectError> {
    super::super::blob_id::BlobId::generate()
        .map(|id| id.to_hex())
        .map_err(object_unavailable)
}

fn stage_path(data_dir: &Path, id: &str) -> Result<PathBuf, ObjectError> {
    super::super::blob_id::BlobId::parse(id)
        .map(|id| id.staging_path(data_dir))
        .ok_or_else(|| object_unavailable("invalid opaque blob ID"))
}

/// Whether a derived transfer can be continued by later requests, or belongs to one request
/// and is reclaimed when that request or the process ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransferKind {
    Continuable,
    Ephemeral,
}

/// A metadata snapshot and independent link to immutable bytes, including after source deletion.
/// Unconsumed handles clean their staging link; `begin_derived` transfers ownership to SQLite.
pub(crate) struct DerivedSource {
    pub object: ObjectRecord,
    path: Option<PathBuf>,
    id: String,
}

impl Drop for DerivedSource {
    fn drop(&mut self) {
        if let Some(path) = &self.path
            && let Err(error) = objects::unlink(path)
        {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "derived_cleanup_deferred",
                serde_json::json!({"message":format!("deferred derived source cleanup: {error}")}),
            );
        }
    }
}

pub(crate) struct DerivedSegment {
    pub source: DerivedSource,
    pub offset: u64,
    pub length: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct DerivedSourceSpec {
    pub bucket: String,
    pub name: String,
    pub generation: Option<i64>,
    pub conditions: ObjectConditions,
    pub access: BucketAccess,
}

impl DerivedSource {
    pub(crate) fn whole(self) -> DerivedSegment {
        let length = self.object.size;
        DerivedSegment {
            source: self,
            offset: 0,
            length,
        }
    }
}

/// The transfer's total length; every range must lie inside its source.
fn derived_total(segments: &[DerivedSegment], max_bytes: u64) -> Result<u64, ObjectError> {
    segments.iter().try_fold(0_u64, |total, segment| {
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
    })
}

fn insert_inputs(
    transaction: &rusqlite::Transaction<'_>,
    id: &str,
    segments: &[DerivedSegment],
) -> Result<(), ObjectError> {
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
            .map_err(object_unavailable)?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DerivedTransfer {
    pub id: String,
    pub total: u64,
    pub offset: u64,
    pub created: std::time::SystemTime,
    pub deadline: std::time::SystemTime,
}

pub(crate) struct DerivedRequestGuard {
    catalog: Catalog,
    id: Option<String>,
}

impl DerivedRequestGuard {
    pub(crate) fn disarm(&mut self) {
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
                                crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "derived_cleanup_deferred", serde_json::json!({"message":format!("deferred abandoned derived cleanup: {error}")}));
                            }
                            return;
                        }
                        Err(ObjectError::DerivedTransferBusy) => notified.await,
                        Err(error) => {
                            crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "derived_cleanup_deferred", serde_json::json!({"message":format!("deferred abandoned derived cleanup: {error}")}));
                            return;
                        }
                    }
                }
            });
        }
    }
}

/// Adapters supply inherited or replacement metadata explicitly; bytes and destination defaults
/// are transport independent. Adapters pass the inherited source class for an absent/null body;
/// `None` selects the destination bucket default for an explicit resource without storageClass.
#[derive(Clone, Debug)]
pub(crate) struct DerivedDestination {
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

impl DerivedDestination {
    #[cfg(test)]
    pub(crate) fn new(access: BucketAccess) -> Self {
        Self {
            bucket: String::new(),
            name: String::new(),
            content_type: String::new(),
            metadata: Default::default(),
            storage_class: None,
            expected: Default::default(),
            conditions: Default::default(),
            access,
            component_count: None,
        }
    }
}

struct Input {
    id: String,
    start: u64,
    length: u64,
    size: u64,
}

fn inputs(connection: &Connection, id: &str) -> Result<Vec<Input>, ObjectError> {
    connection.prepare_cached("SELECT blob_id, start, length, size FROM derived_inputs WHERE transfer_id = ?1 ORDER BY position LIMIT 33")
        .map_err(object_unavailable)?.query_map([id], |row| Ok(Input { id: row.get(0)?, start: row.get::<_, i64>(1)? as u64, length: row.get::<_, i64>(2)? as u64, size: row.get::<_, i64>(3)? as u64 }))
        .map_err(object_unavailable)?.collect::<rusqlite::Result<_>>().map_err(object_unavailable)
}

fn transfer(
    connection: &Connection,
    id: &str,
    now: i64,
) -> Result<(DerivedTransfer, Hasher), ObjectError> {
    let (total, offset, state, crc, created, deadline): (i64, i64, Vec<u8>, u32, i64, i64) = connection
        .query_row(
            "SELECT total, offset, md5_state, crc32c, created, deadline FROM derived_transfers WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
        )
        .optional()
        .map_err(object_unavailable)?
        .ok_or(ObjectError::DerivedTransferNotFound)?;
    if now >= deadline {
        return Err(ObjectError::DerivedTransferExpired);
    }
    let hasher = Hasher::from_md5_state(&state, crc, offset as u64).map_err(object_unavailable)?;
    Ok((
        DerivedTransfer {
            id: id.into(),
            total: total as u64,
            offset: offset as u64,
            created: system_time_millis(created).map_err(object_unavailable)?,
            deadline: system_time_millis(deadline).map_err(object_unavailable)?,
        },
        hasher,
    ))
}

impl Catalog {
    /// Recheck grants without changing the adapter's selection-time source snapshot.
    pub(crate) fn check_derived_bucket_access_in_transaction(
        connection: &Connection,
        bucket: &str,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        objects::authorized_bucket(connection, bucket, access).map(|_| ())
    }

    pub(crate) fn guard_derived_request(&self, id: &str) -> DerivedRequestGuard {
        DerivedRequestGuard {
            catalog: self.clone(),
            id: Some(id.into()),
        }
    }
    /// Call only from a publication callback: the caller's transaction owns all checks/mutations.
    pub(crate) fn check_derived_source_in_transaction(
        connection: &Connection,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        objects::selected(
            connection,
            bucket,
            name,
            generation,
            conditions,
            super::objects::RequestKind::Write,
            access,
        )
        .map(|value| value.0)
    }

    /// Hard deletion is intentionally separate from versioning retirement. Any later callback
    /// failure rolls this deletion back together with the destination and deletion journal.
    pub(crate) fn delete_derived_source_in_transaction(
        connection: &Connection,
        bucket: &str,
        name: &str,
        generation: i64,
        conditions: ObjectConditions,
        access: &BucketAccess,
        now: std::time::SystemTime,
    ) -> Result<(), ObjectError> {
        let (object, id) = objects::selected(
            connection,
            bucket,
            name,
            Some(generation),
            conditions,
            super::objects::RequestKind::Write,
            access,
        )?;
        object.check_retention(now)?;
        connection
            .execute("DELETE FROM objects WHERE blob_id = ?1", [&id])
            .map_err(object_unavailable)?;
        connection
            .execute("INSERT INTO pending_blob_deletions VALUES (?1)", [&id])
            .map_err(object_unavailable)?;
        connection.execute("INSERT INTO derived_deletions SELECT ?1, id FROM derived_transfers WHERE publishing = 1", [&id]).map_err(object_unavailable)?;
        super::change_journal::object(
            connection,
            super::change_journal::origin(access),
            super::change_journal::ChangeKind::Delete,
            &object,
        )
        .map_err(object_unavailable)?;
        Ok(())
    }

    /// Source NotMatch conditions are mutation conditions (412), not conditional GETs (304).
    pub(crate) async fn resolve_derived_source(
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
    pub(crate) async fn resolve_derived_sources(
        &self,
        sources: Vec<DerivedSourceSpec>,
    ) -> Result<Vec<DerivedSource>, ObjectError> {
        self.resolve_derived_sources_keyed(sources, None).await
    }

    /// Every source must be readable with `key` (compose requires one shared key).
    pub(crate) async fn resolve_derived_sources_keyed(
        &self,
        sources: Vec<DerivedSourceSpec>,
        key: Option<crate::csek::CustomerKey>,
    ) -> Result<Vec<DerivedSource>, ObjectError> {
        if sources.is_empty() || sources.len() > 32 {
            return Err(ObjectError::InvalidDerivedArgument(
                "derived request requires 1 to 32 sources".into(),
            ));
        }
        let data_dir = self.0.data_dir.clone();
        self.run_object(move |connection| {
            let transaction = connection.transaction().map_err(object_unavailable)?;
            sources
                .into_iter()
                .map(|source| {
                    let (object, original) = objects::selected(
                        &transaction,
                        &source.bucket,
                        &source.name,
                        source.generation,
                        source.conditions,
                        super::objects::RequestKind::Write,
                        &source.access,
                    )?;
                    super::customer_keys::check(&object, key)?;
                    let id = random_id()?;
                    let path = stage_path(&data_dir, &id)?;
                    fs::hard_link(objects::blob_path(&data_dir, &original)?, &path)
                        .map_err(object_unavailable)?;
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
    #[cfg(test)]
    pub(crate) async fn begin_derived(
        &self,
        segments: Vec<DerivedSegment>,
        max_bytes: u64,
    ) -> Result<DerivedTransfer, ObjectError> {
        self.begin_derived_with_kind(segments, max_bytes, TransferKind::Continuable)
            .await
    }

    /// Single-call requests have no continuation owner after a process crash.
    pub(crate) async fn begin_derived_ephemeral(
        &self,
        segments: Vec<DerivedSegment>,
        max_bytes: u64,
    ) -> Result<DerivedTransfer, ObjectError> {
        self.begin_derived_with_kind(segments, max_bytes, TransferKind::Ephemeral)
            .await
    }

    async fn begin_derived_with_kind(
        &self,
        segments: Vec<DerivedSegment>,
        max_bytes: u64,
        kind: TransferKind,
    ) -> Result<DerivedTransfer, ObjectError> {
        self.begin_derived_with_binding(segments, max_bytes, kind, |_, _| Ok(()))
            .await
    }

    /// Request/session bindings commit with transfer ownership, preventing untracked tokens.
    pub(crate) async fn begin_derived_with_binding(
        &self,
        mut segments: Vec<DerivedSegment>,
        max_bytes: u64,
        kind: TransferKind,
        bind: impl FnOnce(&Connection, &DerivedTransfer) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<DerivedTransfer, ObjectError> {
        let ephemeral = kind == TransferKind::Ephemeral;
        if segments.is_empty() || segments.len() > 32 {
            return Err(ObjectError::InvalidDerivedArgument(
                "derived transfer requires 1 to 32 sources".into(),
            ));
        }
        let total = derived_total(&segments, max_bytes)?;
        let permit = self.admit_object_upload()?;
        let data_dir = self.0.data_dir.clone();
        let created = unix_millis(self.0.clock.now()).map_err(object_unavailable)?;
        let deadline = created
            .checked_add(if ephemeral {
                EPHEMERAL_LIFETIME_MILLIS
            } else {
                LIFETIME_MILLIS
            })
            .ok_or_else(|| object_unavailable("derived deadline overflow"))?;
        let id = random_id()?;
        let writer = self.derived_guard(&id)?;
        let mut owner = ephemeral.then(|| self.guard_derived_request(&id));
        let result = self
            .run_object(move |connection| {
                let (_permit, _writer) = (permit, writer);
                let count: i64 = connection
                    .query_row(
                        "SELECT count(*) FROM derived_transfers WHERE ephemeral = ?1",
                        [ephemeral],
                        |row| row.get(0),
                    )
                    .map_err(object_unavailable)?;
                if count >= MAX_TRANSFERS_PER_KIND as i64 {
                    return Err(object_unavailable("derived transfer limit reached"));
                }
                let path = stage_path(&data_dir, &id)?;
                fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&path)
                    .and_then(|file| file.sync_all())
                    .map_err(object_unavailable)?;
                let mut preserve = false;
                let result = (|| {
                    #[cfg(unix)]
                    objects::sync_dir(&data_dir.join("staging")).map_err(object_unavailable)?;
                    let transaction = connection
                        .transaction_with_behavior(TransactionBehavior::Immediate)
                        .map_err(object_unavailable)?;
                    transaction
                    .execute(
                        "INSERT INTO derived_transfers VALUES (?1, ?2, 0, ?3, 0, ?4, ?5, ?6, 0)",
                        params![
                            id,
                            total as i64,
                            Hasher::default().md5_state().0.as_slice(),
                            ephemeral, created, deadline
                        ],
                    )
                    .map_err(object_unavailable)?;
                    insert_inputs(&transaction, &id, &segments)?;
                    let transfer = DerivedTransfer {
                        id: id.clone(),
                        total,
                        offset: 0,
                        created: system_time_millis(created).map_err(object_unavailable)?,
                        deadline: system_time_millis(deadline).map_err(object_unavailable)?,
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
                            .map_err(object_unavailable)?;
                        if !committed {
                            preserve = false;
                            return Err(object_unavailable(error));
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
                        objects::unlink(&path).map_err(object_unavailable)?;
                        Err(error)
                    }
                }
            })
            .await;
        self.0.maintenance_wake.notify_one();
        if result.is_ok()
            && let Some(owner) = &mut owner
        {
            owner.disarm();
        }
        result
    }

    fn derived_guard(&self, id: &str) -> Result<WriterGuard, ObjectError> {
        let id = format!("derived:{id}");
        let mut writers = self.0.session_writers.lock().map_err(object_unavailable)?;
        if !writers.insert(id.clone()) {
            return Err(ObjectError::DerivedTransferBusy);
        }
        Ok(WriterGuard {
            catalog: self.clone(),
            id,
        })
    }

    pub(crate) async fn get_derived(&self, id: &str) -> Result<DerivedTransfer, ObjectError> {
        let id = id.to_owned();
        let now = unix_millis(self.0.clock.now()).map_err(object_unavailable)?;
        self.run_object(move |connection| transfer(connection, &id, now).map(|value| value.0))
            .await
    }

    /// Every durable offset includes the matching incremental hash state. File fsync precedes
    /// SQLite commit; an interrupted tail is discarded before retry, never counted twice.
    pub(crate) async fn advance_derived(
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
            let now = unix_millis(catalog.0.clock.now()).map_err(object_unavailable)?;
            let (mut progress, mut hasher, sources) = catalog.run_upload(move |connection| {
                let (progress, hasher) = transfer(connection, &query, now)?;
                restore_stage(&data_dir, &query)?;
                Ok((progress, hasher, inputs(connection, &query)?))
            }).await?;
            let path = stage_path(&catalog.0.data_dir, &id)?;
            let mut output = tokio::fs::OpenOptions::new().write(true).open(path).await.map_err(object_unavailable)?;
            output.set_len(progress.offset).await.map_err(object_unavailable)?;
            output.seek(io::SeekFrom::Start(progress.offset)).await.map_err(object_unavailable)?;
            let target = progress.offset.saturating_add(budget).min(progress.total);
            let mut skip = progress.offset;
            let mut buffer = [0_u8; BUFFER_SIZE];
            for source in sources {
                if skip >= source.length { skip -= source.length; continue; }
                if progress.offset == target { break; }
                let mut input = tokio::fs::File::open(stage_path(&catalog.0.data_dir, &source.id)?).await.map_err(object_unavailable)?;
                input.seek(io::SeekFrom::Start(source.start + skip)).await.map_err(object_unavailable)?;
                let mut remaining = (source.length - skip).min(target - progress.offset);
                skip = 0;
                while remaining > 0 {
                    if catalog.0.admission.is_closed() { return Err(object_unavailable("catalog is closing")); }
                    let length = remaining.min(BUFFER_SIZE as u64) as usize;
                    input.read_exact(&mut buffer[..length]).await.map_err(object_unavailable)?;
                    output.write_all(&buffer[..length]).await.map_err(object_unavailable)?;
                    hasher.update(&buffer[..length]);
                    progress.offset += length as u64;
                    remaining -= length as u64;
                }
            }
            // sync_all drops the error of tokio's background tail write; flush reports it.
            output.flush().await.map_err(object_unavailable)?;
            output.sync_all().await.map_err(object_unavailable)?;
            catalog.run_upload(move |connection| {
                let (md5_state, crc) = hasher.md5_state();
                connection.execute("UPDATE derived_transfers SET offset = ?2, md5_state = ?3, crc32c = ?4 WHERE id = ?1", params![id, progress.offset as i64, md5_state.as_slice(), crc]).map_err(object_unavailable)?;
                Ok(progress)
            }).await
        }).await.map_err(object_unavailable)?
    }

    /// The callback joins protocol-specific completion state to the same destination commit.
    #[cfg(test)]
    pub(crate) async fn publish_derived(
        &self,
        id: &str,
        destination: DerivedDestination,
        complete: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<ObjectRecord, ObjectError> {
        self.publish_derived_with_validation(id, destination, |_, _| Ok(()), complete)
            .await
    }

    /// Validation precedes retirement so same-name sources remain visible in unversioned buckets.
    pub(crate) async fn publish_derived_with_validation(
        &self,
        id: &str,
        destination: DerivedDestination,
        validate: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        complete: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<ObjectRecord, ObjectError> {
        self.publish_derived_prepared(
            id,
            destination,
            move |connection, record| validate(connection, record),
            complete,
            None,
        )
        .await
    }

    /// Adapter-derived fields must use the destination defaults selected by this transaction.
    pub(crate) async fn publish_derived_prepared(
        &self,
        id: &str,
        destination: DerivedDestination,
        prepare: impl FnOnce(&Connection, &mut ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        complete: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        admission_deadline: Option<tokio::time::Instant>,
    ) -> Result<ObjectRecord, ObjectError> {
        self.publish_derived_replayed(id, destination, prepare, complete, admission_deadline, None)
            .await
    }

    pub(crate) async fn publish_derived_replayed(
        &self,
        id: &str,
        destination: DerivedDestination,
        prepare: impl FnOnce(&Connection, &mut ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        complete: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        admission_deadline: Option<tokio::time::Instant>,
        replay: Option<super::upload_replay::CopyReplay>,
    ) -> Result<ObjectRecord, ObjectError> {
        let permit = self.admit_object_upload()?;
        let guard = self.derived_guard(id)?;
        let execution = self.0.execution.clone().acquire_owned();
        let execution = match admission_deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, execution)
                .await
                .map_err(|_| {
                    object_unavailable("derived publication catalog admission timed out")
                })?,
            None => execution.await,
        }
        .map_err(object_unavailable)?;
        let catalog = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let (_permit, _guard) = (permit, guard);
            let query = id.clone();
            let data_dir = catalog.0.data_dir.clone();
            let clock = catalog.0.clock.clone();
            let result = catalog.run_upload_admitted(execution, move |connection| {
                let (progress, hasher) = transfer(connection, &query, unix_millis(clock.now()).map_err(object_unavailable)?)?;
                let sources = inputs(connection, &query)?;
                if progress.offset != progress.total { return Err(ObjectError::DerivedTransferIncomplete); }
                let Hashes { md5, crc32c } = hasher.finish();
                restore_stage(&data_dir, &query)?;
                let stage = stage_path(&data_dir, &query)?;
                objects::publish_blob(&stage, &objects::blob_path(&data_dir, &query)?).map_err(object_unavailable)?;
                let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(object_unavailable)?;
                let (default_class, allow_overwrite) = objects::authorized_bucket(&transaction, &destination.bucket, &destination.access)?;
                let micros = unix_micros(clock.now()).map_err(object_unavailable)?;
                let cached = replay.as_ref().map(|replay| replay.cached(&transaction, micros)).transpose()?.flatten();
                let record = if let Some(record) = cached {
                    transaction.execute("INSERT INTO pending_blob_deletions(blob_id) VALUES (?1)", [&query]).map_err(object_unavailable)?;
                    record
                } else {
                if replay.is_some() { super::upload_replay::capacity(&transaction, micros)?; }
                // Autoclass ignores rewrite/copy/compose class requests and stores STANDARD.
                let storage_class = super::bucket_autoclass::object_storage_class(&transaction, &destination.bucket, destination.storage_class.clone(), default_class).map_err(object_unavailable)?;
                let (versioning, default_hold) = Catalog::bucket_settings_in_transaction(&transaction, &destination.bucket).map_err(object_unavailable)?;
                let previous = objects::lookup(&transaction, &destination.bucket, &destination.name)?;
                objects::check_publication(previous.as_ref(), destination.conditions, allow_overwrite, clock.now())?;
                if destination.expected.md5.is_some_and(|hash| hash != md5) || destination.expected.crc32c.is_some_and(|hash| hash != crc32c) { return Err(ObjectError::ChecksumMismatch); }
                let now = clock.now();
                now.duration_since(std::time::UNIX_EPOCH).map_err(object_unavailable)?;
                let time = system_time_micros(micros).map_err(object_unavailable)?;
                let (record, _retired, ()) = objects::publish_in_transaction(&transaction, micros, objects::GenerationAssignment::Automatic, previous.as_ref(), versioning, &query, super::change_journal::origin(&destination.access), |generation| {
                crate::bucket::validate_storage_class(&storage_class).map_err(|error| ObjectError::UnsupportedMetadata(error.to_string()))?;
                let mut record = ObjectRecord {
                    bucket: destination.bucket, name: destination.name, generation, metageneration: 1,
                    content_type: destination.content_type, size: progress.total, md5, crc32c, storage_class,
                    component_count: destination.component_count,
                    content_encoding: None, content_disposition: None, content_language: None,
                    cache_control: None, metadata: Default::default(), time_created: time, updated: time,
                    time_deleted: None, soft_delete_time: None, hard_delete_time: None, event_based_hold: default_hold,
                    temporary_hold: false, custom_time: None, retention_expiration_time: None, retention: None, customer_key_sha256: None,
                };
                destination.metadata.apply(&mut record)?;
                transaction.execute("UPDATE derived_transfers SET publishing = 1 WHERE id = ?1", [&query]).map_err(object_unavailable)?;
                prepare(&transaction, &mut record)?;
                Ok((record, ()))
                })?;
                complete(&transaction, &record)?;
                if let Some(replay) = &replay { replay.store(&transaction, micros, &record)?; }
                record
                };
                transaction.execute("DELETE FROM derived_inputs WHERE transfer_id = ?1", [&query]).map_err(object_unavailable)?;
                transaction.execute("DELETE FROM derived_transfers WHERE id = ?1", [&query]).map_err(object_unavailable)?;
                transaction.commit().map_err(object_unavailable)?;
                for source in sources {
                    if let Err(error) = objects::unlink(&stage_path(&data_dir, &source.id)?) {
                        crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "derived_cleanup_deferred", serde_json::json!({"message":format!("deferred derived source cleanup: {error}")}));
                    }
                }
                Ok(record)
            }).await;
            if result.is_ok() {
                catalog.reclaim_after_commit().await;
            }
            catalog.0.maintenance_wake.notify_one();
            // Check SQL references even after an ambiguous commit error before removing bytes.
            if matches!(&result, Err(error) if !error.is_backend_failure() && !matches!(error, ObjectError::DerivedTransferBusy | ObjectError::DerivedTransferIncomplete | ObjectError::DerivedTransferNotFound))
                && let Err(error) = catalog.cleanup_derived(&id).await {
                crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "derived_cleanup_deferred", serde_json::json!({"message":format!("deferred derived cleanup: {error}")}));
            }
            result
        }).await.map_err(object_unavailable)?
    }

    async fn cleanup_derived(&self, id: &str) -> Result<(), ObjectError> {
        let id = id.to_owned();
        let data_dir = self.0.data_dir.clone();
        self.run_upload(move |connection| cleanup_record(connection, &data_dir, &id))
            .await
    }

    pub(crate) async fn cancel_derived(&self, id: &str) -> Result<(), ObjectError> {
        let _guard = self.derived_guard(id)?;
        self.cleanup_derived(id).await
    }

    /// The existing maintenance scheduler includes these deadlines and sleeps until work is due.
    #[cfg(test)]
    pub(crate) async fn maintain_derived_transfers(
        &self,
    ) -> Result<Option<std::time::SystemTime>, crate::bucket::CatalogError> {
        let now = unix_millis(self.0.clock.now())?;
        self.maintain_derived_transfers_at(now).await
    }

    pub(super) async fn maintain_derived_transfers_at(
        &self,
        now: i64,
    ) -> Result<Option<std::time::SystemTime>, crate::bucket::CatalogError> {
        let mut failure = self.reclaim_pending().await.err();
        let due = self.run_object(move |connection| {
            connection.prepare_cached("SELECT id FROM derived_transfers WHERE deadline <= ?1 ORDER BY id LIMIT 128").map_err(object_unavailable)?
                .query_map([now], |row| row.get::<_, String>(0)).map_err(object_unavailable)?.collect::<rusqlite::Result<Vec<_>>>().map_err(object_unavailable)
        }).await.map_err(super::unavailable)?;
        for id in due {
            let _guard = match self.derived_guard(&id) {
                Ok(guard) => guard,
                Err(ObjectError::DerivedTransferBusy) => continue,
                Err(error) => {
                    failure = Some(error);
                    continue;
                }
            };
            if let Err(error) = self.cleanup_derived(&id).await
                && error != ObjectError::DerivedTransferNotFound
            {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            return Err(super::unavailable(error));
        }
        self.run(move |connection| {
            let deadline: Option<i64> = connection
                .query_row(
                    "SELECT min(deadline) FROM derived_transfers WHERE deadline > ?1",
                    [now],
                    |row| row.get(0),
                )
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
                .map(system_time_millis)
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
    let ids = connection.prepare_cached("SELECT blob_id FROM derived_deletions WHERE (?1 IS NULL OR transfer_id = ?1) ORDER BY blob_id LIMIT 128").map_err(object_unavailable)?
        .query_map([transfer_id], |row| row.get::<_, String>(0)).map_err(object_unavailable)?.collect::<rusqlite::Result<Vec<_>>>().map_err(object_unavailable)?;
    let mut failure = None;
    for id in ids {
        match objects::reclaim(connection, data_dir, &id) {
            Ok(()) => {
                connection
                    .execute("DELETE FROM derived_deletions WHERE blob_id = ?1", [id])
                    .map_err(object_unavailable)?;
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
        .map_err(object_unavailable)?;
    if !exists {
        return Err(ObjectError::DerivedTransferNotFound);
    }
    for source in inputs(connection, id)? {
        objects::unlink(&stage_path(data_dir, &source.id)?).map_err(object_unavailable)?;
    }
    objects::unlink(&stage_path(data_dir, id)?).map_err(object_unavailable)?;
    let referenced: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)",
            [id],
            |row| row.get(0),
        )
        .map_err(object_unavailable)?;
    if !referenced {
        objects::reclaim(connection, data_dir, id)?;
    }
    discard_record(connection, id)
}

fn discard_record(connection: &Connection, id: &str) -> Result<(), ObjectError> {
    let transaction = connection
        .unchecked_transaction()
        .map_err(object_unavailable)?;
    transaction
        .execute("DELETE FROM derived_inputs WHERE transfer_id = ?1", [id])
        .map_err(object_unavailable)?;
    transaction
        .execute("DELETE FROM derived_transfers WHERE id = ?1", [id])
        .map_err(object_unavailable)?;
    transaction.commit().map_err(object_unavailable)
}

fn restore_stage(data_dir: &Path, id: &str) -> Result<PathBuf, ObjectError> {
    let stage = stage_path(data_dir, id)?;
    if !stage.try_exists().map_err(object_unavailable)? {
        fs::rename(objects::blob_path(data_dir, id)?, &stage).map_err(object_unavailable)?;
    }
    Ok(stage)
}

pub(super) fn recover(
    connection: &Connection,
    data_dir: &Path,
    now: i64,
) -> Result<(), ObjectError> {
    reclaim_derived_deletions(connection, data_dir, None)?;
    let over_limit: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM derived_transfers GROUP BY ephemeral HAVING count(*) > ?1)",
        [MAX_TRANSFERS_PER_KIND as i64],
        |row| row.get(0),
    ).map_err(object_unavailable)?;
    if over_limit {
        return Err(object_unavailable("too many derived transfers"));
    }
    let ids = connection
        .prepare_cached("SELECT id FROM derived_transfers ORDER BY id LIMIT ?1")
        .map_err(object_unavailable)?
        .query_map([(2 * MAX_TRANSFERS_PER_KIND + 1) as i64], |row| {
            row.get::<_, String>(0)
        })
        .map_err(object_unavailable)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(object_unavailable)?;
    if ids.len() > 2 * MAX_TRANSFERS_PER_KIND {
        return Err(object_unavailable("too many derived transfers"));
    }
    for id in ids {
        let (ephemeral, deadline): (bool, i64) = connection
            .query_row(
                "SELECT ephemeral, deadline FROM derived_transfers WHERE id = ?1",
                [&id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(object_unavailable)?;
        if ephemeral || deadline <= now {
            discard_record(connection, &id)?;
            continue;
        }
        let validity = (|| {
            let (progress, saved_hash) = transfer(connection, &id, now)?;
            let sources = inputs(connection, &id)?;
            if sources.is_empty()
                || sources.len() > 32
                || sources.iter().map(|source| source.length).sum::<u64>() != progress.total
            {
                return Err(object_unavailable("invalid derived sources"));
            }
            let stage = restore_stage(data_dir, &id)?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .read(true)
                .open(stage)
                .map_err(object_unavailable)?;
            if output.metadata().map_err(object_unavailable)?.len() < progress.offset {
                return Err(object_unavailable(
                    "derived prefix is shorter than its checkpoint",
                ));
            }
            output
                .set_len(progress.offset)
                .map_err(object_unavailable)?;
            output.sync_all().map_err(object_unavailable)?;
            // A same-length corrupted prefix must not inherit the acknowledged hash state.
            let mut hasher = Hasher::default();
            let mut remaining = progress.offset;
            let mut buffer = [0; BUFFER_SIZE];
            while remaining > 0 {
                let length = remaining.min(BUFFER_SIZE as u64) as usize;
                std::io::Read::read_exact(&mut output, &mut buffer[..length])
                    .map_err(object_unavailable)?;
                hasher.update(&buffer[..length]);
                remaining -= length as u64;
            }
            if hasher.finish() != saved_hash.finish() {
                return Err(object_unavailable(
                    "derived prefix checksum differs from checkpoint",
                ));
            }
            for source in sources {
                let metadata = fs::symlink_metadata(stage_path(data_dir, &source.id)?)
                    .map_err(object_unavailable)?;
                if !metadata.is_file() || metadata.len() != source.size {
                    return Err(object_unavailable("invalid pinned derived source"));
                }
            }
            Ok::<_, ObjectError>(())
        })();
        if let Err(error) = validity {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "derived_transfer_discarded",
                serde_json::json!({"message":format!("discarding invalid derived transfer {id}: {error}")}),
            );
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
    use md5::{Digest, Md5, digest::common::hazmat::SerializableState};
    use std::io::Write;
    use std::time::UNIX_EPOCH;
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
                autoclass: None,
                labels: Default::default(),
                versioning_enabled: false,
                default_event_based_hold: false,
                public_access_prevention_enforced: false,
                uniform_bucket_level_access_locked_time: None,
                lifecycle: Vec::new(),
                retention_policy: None,
                soft_delete_policy: None,
                object_retention_enabled: false,
                cors: Vec::new(),
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
                &BucketAccess::Permissive,
            )
            .await
            .unwrap()
    }

    fn destination(name: &str) -> DerivedDestination {
        DerivedDestination {
            bucket: "bucket".into(),
            name: name.into(),
            content_type: "text/plain".into(),
            ..DerivedDestination::new(BucketAccess::Permissive)
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
        let target = put(&catalog, "target", b"original").await;
        let connection = Connection::open(path.join("catalog.sqlite3")).unwrap();
        let last_generation: i64 = connection
            .query_row("SELECT last_generation FROM object_generation", [], |row| {
                row.get(0)
            })
            .unwrap();
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
                    &BucketAccess::Permissive,
                    std::time::SystemTime::now(),
                )?;
                Err(ObjectError::PreconditionFailed)
            })
            .await;
        assert_eq!(result, Err(ObjectError::PreconditionFailed));
        assert_eq!(bytes(&catalog, "target").await, b"original");
        assert_eq!(bytes(&catalog, "source").await, b"source");
        let (last_after_failure, pending): (i64, i64) = connection
            .query_row(
                "SELECT last_generation, (SELECT count(*) FROM pending_blob_deletions) FROM object_generation",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((last_after_failure, pending), (last_generation, 0));
        let current = catalog
            .get_object("bucket", "target", None, Default::default())
            .await
            .unwrap();
        assert_eq!(current, target);
        drop(connection);
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
                    &BucketAccess::Permissive,
                    std::time::SystemTime::now(),
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
                        &BucketAccess::Permissive
                    )
                    .await,
                Err(ObjectError::PreconditionFailed)
            ));
        }
        let denied = BucketAccess::Policy {
            identity: None,
            permission: crate::auth::Permission::ObjectsGet,
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
                        &BucketAccess::Permissive,
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
                        &BucketAccess::Permissive,
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
    async fn transfer_kinds_have_independent_bounded_quotas_across_restart() {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"bytes").await;
        let mut persistent = Vec::new();
        for kind in [TransferKind::Ephemeral, TransferKind::Continuable] {
            for _ in 0..MAX_TRANSFERS_PER_KIND {
                let transfer = catalog
                    .begin_derived_with_kind(
                        vec![source(&catalog, "source").await.whole()],
                        5,
                        kind,
                    )
                    .await
                    .unwrap();
                if kind == TransferKind::Continuable {
                    persistent.push(transfer);
                }
            }
            assert_eq!(
                catalog
                    .begin_derived_with_kind(
                        vec![source(&catalog, "source").await.whole()],
                        5,
                        kind,
                    )
                    .await,
                Err(object_unavailable("derived transfer limit reached"))
            );
        }
        assert_eq!(
            fs::read_dir(path.join("staging")).unwrap().count(),
            4 * MAX_TRANSFERS_PER_KIND
        );
        catalog.close().await.unwrap();
        drop(catalog);
        let catalog = Catalog::open(&path).await.unwrap();
        for transfer in persistent {
            assert_eq!(catalog.get_derived(&transfer.id).await.unwrap(), transfer);
        }
        assert_eq!(
            fs::read_dir(path.join("staging")).unwrap().count(),
            2 * MAX_TRANSFERS_PER_KIND
        );
        assert_eq!(
            catalog
                .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
                .await,
            Err(object_unavailable("derived transfer limit reached"))
        );
        let ephemeral = catalog
            .begin_derived_ephemeral(vec![source(&catalog, "source").await.whole()], 5)
            .await
            .unwrap();
        catalog.cancel_derived(&ephemeral.id).await.unwrap();
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
            access: BucketAccess::Permissive,
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
                .publish_derived(&transfer.id, destination("copy"), |_, _| Err(
                    object_unavailable("retryable callback failure")
                ))
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
                TransferKind::Continuable,
                |connection, transfer| {
                    assert_eq!(inputs(connection, &transfer.id)?.len(), 1);
                    connection
                        .execute("UPDATE startup_seed SET complete = 1", [])
                        .map_err(object_unavailable)?;
                    Err(ObjectError::PreconditionFailed)
                },
            )
            .await;
        assert_eq!(result, Err(ObjectError::PreconditionFailed));
        assert!(
            !catalog
                .admin(super::super::admin::Origin::Seed)
                .seed_complete()
                .await
                .unwrap()
        );
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        let transfer = catalog
            .begin_derived_with_binding(
                vec![source(&catalog, "source").await.whole()],
                5,
                TransferKind::Continuable,
                |connection, transfer| {
                    assert_eq!(inputs(connection, &transfer.id)?.len(), 1);
                    connection
                        .execute("UPDATE startup_seed SET complete = 1", [])
                        .map_err(object_unavailable)?;
                    Ok(())
                },
            )
            .await
            .unwrap();
        assert!(
            catalog
                .admin(super::super::admin::Origin::Seed)
                .seed_complete()
                .await
                .unwrap()
        );
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
        let session = catalog
            .create_resumable_session(
                "bucket",
                "upload",
                crate::storage::ResumableOptions::new(BucketAccess::Permissive),
                None,
            )
            .await
            .unwrap();
        let writer = catalog.derived_guard(&transfer.id).unwrap();
        clock.0.store(604800, Ordering::SeqCst);
        assert_eq!(
            catalog.get_derived(&transfer.id).await,
            Err(ObjectError::DerivedTransferExpired)
        );
        assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
        assert!(matches!(
            catalog.resumable_session(&session.id, "bucket").await,
            Err(crate::storage::ResumableError::NotFound)
        ));
        drop(writer);
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
                    &BucketAccess::Permissive,
                    std::time::SystemTime::now(),
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
                    &BucketAccess::Permissive,
                    std::time::SystemTime::now(),
                )?;
                fs::remove_file(&failed_blob).map_err(object_unavailable)?;
                fs::create_dir(&failed_blob).map_err(object_unavailable)?;
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
                    .map_err(object_unavailable)
            })
            .await
            .unwrap();
        assert_eq!(tagged, 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn dropping_begin_after_binding_starts_reclaims_the_ephemeral_transfer() {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"bytes").await;
        let source = source(&catalog, "source").await;
        let worker = catalog.clone();
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let request = tokio::spawn(async move {
            worker
                .begin_derived_with_binding(
                    vec![source.whole()],
                    5,
                    TransferKind::Ephemeral,
                    move |_, transfer| {
                        assert_eq!(
                            transfer
                                .deadline
                                .duration_since(transfer.created)
                                .unwrap()
                                .as_millis(),
                            EPHEMERAL_LIFETIME_MILLIS as u128
                        );
                        entered.send(transfer.id.clone()).unwrap();
                        blocked.recv().unwrap();
                        Ok(())
                    },
                )
                .await
        });
        let id = started.await.unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if catalog.get_derived(&id).await == Err(ObjectError::DerivedTransferNotFound) {
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

    #[test]
    fn foreign_key_bindings_cascade_on_cancel_expiry_and_quarantine() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("PRAGMA foreign_keys = ON; CREATE TABLE objects(blob_id TEXT); CREATE TABLE pending_blob_deletions(blob_id TEXT PRIMARY KEY);").unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection.execute_batch("CREATE TABLE bindings(transfer_id TEXT PRIMARY KEY REFERENCES derived_transfers(id) ON DELETE CASCADE, snapshot TEXT NOT NULL);").unwrap();
        for mode in ["cancel", "expiry", "quarantine"] {
            let id = random_id().unwrap();
            connection
                .execute(
                    "INSERT INTO derived_transfers VALUES (?1,0,0,?2,0,0,0,1,0)",
                    params![id, Md5::new().serialize().as_slice()],
                )
                .unwrap();
            connection
                .execute(
                    "INSERT INTO bindings VALUES (?1, 'request snapshot')",
                    [&id],
                )
                .unwrap();
            match mode {
                "cancel" => cleanup_record(&connection, &std::env::temp_dir(), &id).unwrap(),
                "expiry" => recover(&connection, &std::env::temp_dir(), 1).unwrap(),
                "quarantine" => recover(&connection, &std::env::temp_dir(), 0).unwrap(),
                _ => unreachable!(),
            }
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM bindings", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0
            );
        }
    }

    #[tokio::test]
    async fn migrated_version_nine_catalog_has_no_foreign_key_violations_or_changed_object() {
        let (catalog, path) = setup().await;
        let mut original = put(&catalog, "source", b"bytes").await;
        original.time_created =
            system_time_millis(unix_millis(original.time_created).unwrap()).unwrap();
        original.updated = original.time_created;
        catalog.close().await.unwrap();
        drop(catalog);
        let connection = Connection::open(path.join("catalog.sqlite3")).unwrap();
        super::super::test_support::downgrade_timestamp_precision(&connection);
        connection
            .execute_batch("DROP TABLE rewrite_sessions;")
            .unwrap();
        assert_eq!(connection.query_row("SELECT count(*) FROM sqlite_schema AS schema, pragma_foreign_key_list(schema.name) WHERE schema.type = 'table'", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
        connection.execute_batch("ALTER TABLE buckets DROP COLUMN public_access_prevention_enforced; ALTER TABLE buckets DROP COLUMN cors; DROP TABLE derived_deletions; DROP TABLE derived_inputs; DROP TABLE derived_transfers; ALTER TABLE objects DROP COLUMN component_count; PRAGMA user_version = 9;").unwrap();
        assert!(
            !connection
                .prepare("PRAGMA foreign_key_check")
                .unwrap()
                .exists([])
                .unwrap()
        );
        drop(connection);
        let catalog = Catalog::open(&path).await.unwrap();
        assert_eq!(
            catalog
                .get_object("bucket", "source", None, Default::default())
                .await
                .unwrap(),
            original
        );
        catalog
            .run_object(|connection| {
                assert_eq!(
                    connection
                        .pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
                        .unwrap(),
                    1
                );
                assert!(
                    !connection
                        .prepare("PRAGMA foreign_key_check")
                        .unwrap()
                        .exists([])
                        .unwrap()
                );
                Ok(())
            })
            .await
            .unwrap();
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }
}
