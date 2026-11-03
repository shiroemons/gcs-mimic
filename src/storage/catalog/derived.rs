//! Generation-pinned, resumable byte derivation shared by copy, rewrite and compose.

use super::{
    Catalog, objects,
    resumable::WriterGuard,
    system_time_micros, system_time_millis,
    transfer_io::{self, stage_path},
    unix_micros, unix_millis,
};
use crate::storage::blob_id::BlobId;
use crate::storage::catalog::sql;
use crate::{
    bucket::BucketAccess,
    hashing::{Hasher, Hashes},
    object::{
        ComponentFailure, ComponentFailureKind, ExpectedHashes, ObjectConditions, ObjectError,
        ObjectMetadataUpdate, ObjectRecord, ObjectTarget,
    },
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{fs, path::Path};

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

#[cfg(test)]
use super::fault::{self, Step};

fn random_id() -> Result<BlobId, ObjectError> {
    BlobId::generate().map_err(object_unavailable)
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
    backend: Option<super::BlobBackend>,
    id: BlobId,
    _activity: std::sync::Arc<super::Activity>,
}

impl Drop for DerivedSource {
    fn drop(&mut self) {
        if let Some(backend) = &self.backend
            && let Err(error) = backend.remove_pin(&self.id)
        {
            if backend.is_memory() {
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Warn,
                    "derived_cleanup_deferred",
                    serde_json::json!({"message":format!("deferred RAM source cleanup: {error}")}),
                );
            } else {
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Warn,
                    "derived_cleanup_deferred",
                    serde_json::json!({"message":format!("deferred derived source cleanup: {error}")}),
                );
            }
        }
    }
}

pub(crate) struct DerivedSegment {
    pub source: DerivedSource,
    pub offset: u64,
    pub length: u64,
}

/// Whether a failed source lookup keeps the generic error or names the failing component.
#[derive(Clone, Copy)]
enum SourceFailures {
    Plain,
    NameComponent,
}

/// Turns a missing source or a failed `ifGenerationMatch` into the compose component error.
fn name_component(
    connection: &Connection,
    source: &DerivedSourceSpec,
    error: ObjectError,
) -> ObjectError {
    let kind = match (&error, source.conditions.if_generation_match) {
        (ObjectError::ObjectNotFound, _) => ComponentFailureKind::NotFound,
        (ObjectError::PreconditionFailed, Some(required)) => {
            match objects::lookup(connection, &source.target.bucket, &source.target.name) {
                Ok(Some(live)) => ComponentFailureKind::GenerationMismatch {
                    required,
                    actual: live.record.generation,
                },
                _ => return error,
            }
        }
        _ => return error,
    };
    ObjectError::Component(Box::new(ComponentFailure {
        bucket: source.target.bucket.clone(),
        name: source.target.name.clone(),
        kind,
    }))
}

#[derive(Clone, Debug)]
pub(crate) struct DerivedSourceSpec {
    pub(crate) target: ObjectTarget,
    pub(crate) conditions: ObjectConditions,
    pub(crate) access: BucketAccess,
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
            .filter(|total| *total <= max_bytes && i64::try_from(*total).is_ok())
            .ok_or(ObjectError::TooLarge)
    })
}

fn insert_inputs(
    transaction: &rusqlite::Transaction<'_>,
    id: &BlobId,
    segments: &[DerivedSegment],
) -> Result<(), ObjectError> {
    let mut statement =
        transaction.prepare_cached("INSERT INTO derived_inputs VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
    for (position, segment) in segments.iter().enumerate() {
        statement.execute(params![
            id,
            position,
            segment.source.id,
            segment.offset,
            segment.length,
            segment.source.object.size
        ])?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DerivedTransfer {
    pub id: BlobId,
    pub total: u64,
    pub offset: u64,
    pub created: std::time::SystemTime,
    pub deadline: std::time::SystemTime,
}

pub(crate) struct DerivedRequestGuard {
    catalog: Catalog,
    id: Option<BlobId>,
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
            crate::task::spawn_supervised_on(&runtime, "derived-cleanup", async move {
                loop {
                    let notified = catalog.0.derived_wake.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    match catalog.derived_guard(&id) {
                        Ok(_guard) => {
                            if let Err(error) = catalog.cleanup_derived(&id).await
                                && error != ObjectError::DerivedTransferNotFound
                            {
                                crate::diagnostics::emit(
                                    crate::diagnostics::LogLevel::Warn,
                                    "derived_cleanup_deferred",
                                    serde_json::json!({"message":format!("deferred abandoned derived cleanup: {error}")}),
                                );
                            }
                            return;
                        }
                        Err(ObjectError::DerivedTransferBusy) => notified.await,
                        Err(error) => {
                            crate::diagnostics::emit(
                                crate::diagnostics::LogLevel::Warn,
                                "derived_cleanup_deferred",
                                serde_json::json!({"message":format!("deferred abandoned derived cleanup: {error}")}),
                            );
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
    pub storage_class: Option<crate::bucket::StorageClass>,
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
    id: BlobId,
    start: u64,
    length: u64,
    size: u64,
}

fn inputs(connection: &Connection, id: &BlobId) -> Result<Vec<Input>, ObjectError> {
    connection.prepare_cached("SELECT blob_id, start, length, size FROM derived_inputs WHERE transfer_id = ?1 ORDER BY position LIMIT 33")?.query_map([id], |row| Ok(Input { id: row.get(0)?, start: row.get(1)?, length: row.get(2)?, size: row.get(3)? }))?.collect::<rusqlite::Result<_>>().map_err(object_unavailable)
}

fn transfer(
    connection: &Connection,
    id: &BlobId,
    now: i64,
) -> Result<(DerivedTransfer, Hasher), ObjectError> {
    const SELECT: &str = "SELECT total, offset, md5_state, crc32c, created, deadline FROM derived_transfers WHERE id = ?1";
    let (total, offset, state, crc, created, deadline): (u64, u64, Vec<u8>, u32, i64, i64) =
        sql::row(connection, SELECT, [id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .optional()?
        .ok_or(ObjectError::DerivedTransferNotFound)?;
    if now >= deadline {
        return Err(ObjectError::DerivedTransferExpired);
    }
    let hasher = Hasher::from_md5_state(&state, crc, offset).map_err(object_unavailable)?;
    Ok((
        DerivedTransfer {
            id: *id,
            total,
            offset,
            created: system_time_millis(created)?,
            deadline: system_time_millis(deadline)?,
        },
        hasher,
    ))
}

impl Catalog {
    /// Recheck grants without changing the adapter's selection-time source snapshot.
    pub(crate) fn check_derived_source_access_in_transaction(
        connection: &Connection,
        source: &ObjectRecord,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        objects::authorize_generation(
            connection,
            &source.bucket,
            &source.name,
            source.generation,
            access,
        )
    }

    pub(crate) fn guard_derived_request(&self, id: &BlobId) -> DerivedRequestGuard {
        DerivedRequestGuard {
            catalog: self.clone(),
            id: Some(*id),
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
        .map(|stored| stored.record)
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
        let objects::StoredObject {
            record: object,
            blob: id,
        } = objects::selected(
            connection,
            bucket,
            name,
            Some(generation),
            conditions,
            super::objects::RequestKind::Write,
            access,
        )?;
        object.check_retention(now)?;
        super::notification_outbox::enqueue(
            connection,
            crate::notification::Message {
                event: crate::notification::Event::Delete,
                object: &object,
                time: now,
                replacement: None,
            },
        )?;
        sql::exec(connection, "DELETE FROM objects WHERE blob_id = ?1", [&id])?;
        sql::exec(
            connection,
            "INSERT INTO pending_blob_deletions VALUES (?1)",
            [&id],
        )?;
        sql::exec(
            connection,
            "INSERT INTO derived_deletions SELECT ?1, id FROM derived_transfers WHERE publishing = 1",
            [&id],
        )?;
        super::change_journal::object(
            connection,
            super::change_journal::origin(access),
            super::change_journal::ChangeKind::Delete,
            &object,
        )?;
        Ok(())
    }

    /// Source NotMatch conditions are mutation conditions (412), not conditional GETs (304).
    pub(crate) async fn resolve_derived_source(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<DerivedSource, ObjectError> {
        self.resolve_derived_sources(vec![DerivedSourceSpec {
            target,
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
        self.resolve_sources(sources, key, SourceFailures::Plain)
            .await
    }

    /// As [`Self::resolve_derived_sources_keyed`], but a missing component or a failed
    /// generation precondition names the component, as Cloud Storage's compose does.
    pub(crate) async fn resolve_compose_components(
        &self,
        sources: Vec<DerivedSourceSpec>,
        key: Option<crate::csek::CustomerKey>,
    ) -> Result<Vec<DerivedSource>, ObjectError> {
        self.resolve_sources(sources, key, SourceFailures::NameComponent)
            .await
    }

    async fn resolve_sources(
        &self,
        sources: Vec<DerivedSourceSpec>,
        key: Option<crate::csek::CustomerKey>,
        failures: SourceFailures,
    ) -> Result<Vec<DerivedSource>, ObjectError> {
        if sources.is_empty() || sources.len() > 32 {
            return Err(ObjectError::InvalidDerivedArgument(
                "derived request requires 1 to 32 sources".into(),
            ));
        }
        let activity = std::sync::Arc::new(self.activity()?);
        let backend = self.0.backend.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            sources
                .into_iter()
                .map(|source| {
                    let objects::StoredObject {
                        record: object,
                        blob: original,
                    } = objects::selected(
                        &transaction,
                        &source.target.bucket,
                        &source.target.name,
                        source.target.generation,
                        source.conditions,
                        super::objects::RequestKind::Write,
                        &source.access,
                    )
                    .map_err(|error| match failures {
                        SourceFailures::Plain => error,
                        SourceFailures::NameComponent => {
                            name_component(&transaction, &source, error)
                        }
                    })?;
                    super::customer_keys::check(&object, key)?;
                    let id = random_id()?;
                    backend.pin(&original, &id)?;
                    Ok(DerivedSource {
                        object,
                        backend: Some(backend.clone()),
                        id,
                        _activity: activity.clone(),
                    })
                })
                .collect()
        })
        .await
    }

    /// Input handles are consumed after ownership commits. Byte transfer needs no later source
    /// row; protocol adapters may still recheck source metadata, existence and grants.
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
        let backend = self.0.backend.clone();
        let created = unix_millis(self.0.clock.now())?;
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
            .run(move |connection| {
                let (_permit, _writer) = (permit, writer);
                let count: usize = sql::scalar(
                    connection,
                    "SELECT count(*) FROM derived_transfers WHERE ephemeral = ?1",
                    [ephemeral],
                )?;
                if count >= MAX_TRANSFERS_PER_KIND {
                    return Err(object_unavailable("derived transfer limit reached"));
                }
                backend.create_draft(&id)?;
                let mut preserve = false;
                let result = (|| {
                    #[cfg(unix)]
                    backend.sync_staging_dir()?;
                    let transaction = sql::immediate(connection)?;
                    sql::exec(
                        &transaction,
                        "INSERT INTO derived_transfers VALUES (?1, ?2, 0, ?3, 0, ?4, ?5, ?6, 0)",
                        params![
                            id,
                            total,
                            Hasher::default().md5_state().0.as_slice(),
                            ephemeral,
                            created,
                            deadline
                        ],
                    )?;
                    insert_inputs(&transaction, &id, &segments)?;
                    let transfer = DerivedTransfer {
                        id,
                        total,
                        offset: 0,
                        created: system_time_millis(created)?,
                        deadline: system_time_millis(deadline)?,
                    };
                    bind(&transaction, &transfer)?;
                    if let Err(error) = transaction.commit() {
                        // A failed commit can be ambiguous; retain every confirmed reference.
                        preserve = true;
                        let committed: bool = sql::scalar(
                            connection,
                            "SELECT EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1)",
                            [&id],
                        )?;
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
                            segment.source.backend = None;
                        }
                        Ok(transfer)
                    }
                    Err(error) => {
                        if preserve {
                            for segment in &mut segments {
                                segment.source.backend = None;
                            }
                            return Err(error);
                        }
                        backend.remove_draft(&id)?;
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

    fn derived_guard(&self, id: &BlobId) -> Result<WriterGuard, ObjectError> {
        let activity = self.activity()?;
        let id = format!("derived:{id}");
        let mut writers = self.0.session_writers.lock().map_err(object_unavailable)?;
        if !writers.insert(id.clone()) {
            return Err(ObjectError::DerivedTransferBusy);
        }
        Ok(WriterGuard {
            catalog: self.clone(),
            id,
            _activity: activity,
        })
    }

    pub(crate) async fn get_derived(&self, id: &BlobId) -> Result<DerivedTransfer, ObjectError> {
        let id = *id;
        let now = unix_millis(self.0.clock.now())?;
        self.run(move |connection| transfer(connection, &id, now).map(|value| value.0))
            .await
    }

    /// Every durable offset includes the matching incremental hash state. File fsync precedes
    /// SQLite commit; an interrupted tail is discarded before retry, never counted twice.
    pub(crate) async fn advance_derived(
        &self,
        id: &BlobId,
        budget: u64,
    ) -> Result<DerivedTransfer, ObjectError> {
        let permit = self.admit_object_upload()?;
        let guard = self.derived_guard(id)?;
        let catalog = self.clone();
        let id = *id;
        // Detached ownership keeps cancellation from racing directory closure or cleanup.
        crate::task::spawn_supervised("derived-transfer", async move {
            let (_permit, _guard) = (permit, guard);
            let query = id;
            let backend = catalog.0.backend.clone();
            let now = unix_millis(catalog.0.clock.now())?;
            let (mut progress, mut hasher, sources) = catalog.run_upload(move |connection| {
                let (progress, hasher) = transfer(connection, &query, now)?;
                backend.restore_draft(&query)?;
                Ok((progress, hasher, inputs(connection, &query)?))
            }).await?;
            let mut output = transfer_io::Output::open(&catalog, &id, progress.offset).await?;
            let target = progress.offset.saturating_add(budget).min(progress.total);
            let mut skip = progress.offset;
            let mut buffer = [0_u8; BUFFER_SIZE];
            for source in sources {
                if skip >= source.length { skip -= source.length; continue; }
                if progress.offset == target { break; }
                let mut input = transfer_io::InputReader::open(&catalog, &source.id, source.start + skip).await?;
                let mut remaining = (source.length - skip).min(target - progress.offset);
                skip = 0;
                while remaining > 0 {
                    if catalog.0.admission.is_closed() { return Err(object_unavailable("catalog is closing")); }
                    let length = remaining.min(BUFFER_SIZE as u64) as usize;
                    input.read_exact(&mut buffer[..length]).await?;
                    #[cfg(test)]
                    fault::hit(&catalog.0.data_dir, Step::BodyWrite).map_err(object_unavailable)?;
                    output.write(&buffer[..length]).await?;
                    hasher.update(&buffer[..length]);
                    progress.offset += length as u64;
                    remaining -= length as u64;
                    if catalog.0.backend.is_memory() { tokio::task::yield_now().await; }
                }
            }
            output.finish(&catalog).await?;
            catalog.run_upload(move |connection| {
                let (md5_state, crc) = hasher.md5_state();
                sql::exec(connection, "UPDATE derived_transfers SET offset = ?2, md5_state = ?3, crc32c = ?4 WHERE id = ?1", params![id, progress.offset, md5_state.as_slice(), crc])?;
                // The queued SQL job owns the draft until its acknowledged prefix is updated.
                output.acknowledge(progress.offset);
                Ok(progress)
            }).await
        }).await?
    }

    /// The callback joins protocol-specific completion state to the same destination commit.
    #[cfg(test)]
    pub(crate) async fn publish_derived(
        &self,
        id: &BlobId,
        destination: DerivedDestination,
        complete: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<ObjectRecord, ObjectError> {
        self.publish_derived_with_validation(id, destination, |_, _| Ok(()), complete)
            .await
    }

    /// Validation precedes retirement so same-name sources remain visible in unversioned buckets.
    pub(crate) async fn publish_derived_with_validation(
        &self,
        id: &BlobId,
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
        id: &BlobId,
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
        id: &BlobId,
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
        let id = *id;
        crate::task::spawn_supervised("derived-publication", async move {
            let (_permit, _guard) = (permit, guard);
            let query = id;
            #[cfg(test)]
            let data_dir = catalog.0.data_dir.clone();
            let backend = catalog.0.backend.clone();
            let clock = catalog.0.clock.clone();
            let principals = catalog.0.principals.get().cloned();
            let result = catalog.run_upload_admitted(execution, move |connection| {
                let (progress, hasher) = transfer(connection, &query, unix_millis(clock.now())?)?;
                let sources = inputs(connection, &query)?;
                if progress.offset != progress.total { return Err(ObjectError::DerivedTransferIncomplete); }
                let Hashes { md5, crc32c } = hasher.finish();
                if !backend.is_memory() {
                backend.restore_draft(&query)?;
                #[cfg(test)]
                fault::hit(&data_dir, Step::BeforePublish).map_err(object_unavailable)?;
                }
                backend.publish_draft(&query)?;
                if !backend.is_memory() {
                #[cfg(test)]
                fault::hit(&data_dir, Step::AfterPublish).map_err(object_unavailable)?;
                }
                let transaction = sql::immediate(connection)?;
                let (default_class, delete_permission) = objects::authorized_bucket(&transaction, &destination.bucket, &destination.access)?;
                let micros = unix_micros(clock.now())?;
                let cached = replay.as_ref().map(|replay| replay.cached(&transaction, micros)).transpose()?.flatten();
                let record = if let Some(record) = cached {
                    sql::exec(&transaction, "INSERT INTO pending_blob_deletions(blob_id) VALUES (?1)", [&query])?;
                    record
                } else {
                if replay.is_some() { super::upload_replay::capacity(&transaction, micros)?; }
                // Autoclass ignores rewrite/copy/compose class requests and stores STANDARD.
                let storage_class = super::bucket_autoclass::object_storage_class(&transaction, &destination.bucket, destination.storage_class, default_class)?;
                let (versioning, default_hold) = Catalog::bucket_settings_in_transaction(&transaction, &destination.bucket)?;
                let previous = objects::lookup(&transaction, &destination.bucket, &destination.name)?;
                objects::check_publication(previous.as_ref(), destination.conditions, delete_permission, clock.now())?;
                if destination.expected.md5.is_some_and(|hash| hash != md5) || destination.expected.crc32c.is_some_and(|hash| hash != crc32c) { return Err(ObjectError::ChecksumMismatch); }
                let now = clock.now();
                now.duration_since(std::time::UNIX_EPOCH).map_err(object_unavailable)?;
                let time = system_time_micros(micros)?;
                let version = objects::NewVersion { blob_id: query, assignment: objects::GenerationAssignment::Automatic, micros, origin: super::change_journal::origin(&destination.access), notification_mode: crate::notification::NotificationMode::Emit, owner_profile: principals.as_ref(), acl: super::acl::PublicationAcl::InheritDefault };
                let (record, _retired, ()) = objects::publish_in_transaction(&transaction, previous.as_ref(), versioning, version, |generation| {
                let defaults = objects::PublicationDefaults {
                    owner: Some(super::object_owner::for_publication(&transaction, &destination.bucket, &destination.access, principals.as_ref())?),
                    storage_class, time, event_based_hold: default_hold,
                };
                let target = ObjectTarget::new(destination.bucket, destination.name, generation);
                let mut record = defaults.record(target, destination.content_type, progress.total, Hashes { md5, crc32c });
                record.component_count = destination.component_count;
                destination.metadata.apply(&mut record)?;
                sql::exec(&transaction, "UPDATE derived_transfers SET publishing = 1 WHERE id = ?1", [&query])?;
                prepare(&transaction, &mut record)?;
                Ok((record, ()))
                })?;
                complete(&transaction, &record)?;
                if let Some(replay) = &replay { replay.store(&transaction, micros, &record)?; }
                record
                };
                sql::exec(&transaction, "DELETE FROM derived_inputs WHERE transfer_id = ?1", [&query])?;
                sql::exec(&transaction, "DELETE FROM derived_transfers WHERE id = ?1", [&query])?;
                let committed = objects::commit_publication(transaction);
                if backend.is_memory() {
                    // An ambiguous SQL commit must not discard a published blob or leak its pins.
                    let retained: bool = sql::scalar(connection, "SELECT EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1)", [&query])?;
                    if !retained {
                        for source in &sources { backend.remove_pin(&source.id)?; }
                        backend.finish_draft(&query)?;
                    }
                }
                committed?;
                if !backend.is_memory() { for source in sources {
                    if let Err(error) = backend.remove_pin(&source.id) {
                        crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "derived_cleanup_deferred", serde_json::json!({"message":format!("deferred derived source cleanup: {error}")}));
                    }
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
        }).await?
    }

    async fn cleanup_derived(&self, id: &BlobId) -> Result<(), ObjectError> {
        let id = *id;
        let backend = self.0.backend.clone();
        self.run_upload(move |connection| cleanup_record_storage(connection, &backend, &id))
            .await
    }

    pub(crate) async fn cancel_derived(&self, id: &BlobId) -> Result<(), ObjectError> {
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
        let due = self.run(move |connection| {
            connection.prepare_cached("SELECT id FROM derived_transfers WHERE deadline <= ?1 ORDER BY id LIMIT 128")?
                .query_map([now], |row| row.get::<_, BlobId>(0))?.collect::<rusqlite::Result<Vec<_>>>().map_err(object_unavailable)
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
            // Derived transfers wake the maintenance sweep, so these run per request.
            let deadline: Option<i64> = sql::scalar(
                connection,
                "SELECT min(deadline) FROM derived_transfers WHERE deadline > ?1",
                [now],
            )?;
            let pending: bool = sql::scalar(
                connection,
                "SELECT EXISTS(SELECT 1 FROM derived_deletions)",
                [],
            )?;
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

#[expect(
    clippy::disallowed_methods,
    reason = "deferred derived deletions are reclaimed only during open-time recovery"
)]
fn reclaim_derived_deletions(
    connection: &Connection,
    data_dir: &Path,
    transfer_id: Option<&BlobId>,
) -> Result<(), ObjectError> {
    let ids = connection.prepare_cached("SELECT blob_id FROM derived_deletions WHERE (?1 IS NULL OR transfer_id = ?1) ORDER BY blob_id LIMIT 128")?
        .query_map([transfer_id], |row| row.get::<_, BlobId>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut failure = None;
    for id in ids {
        match objects::reclaim(connection, data_dir, &id) {
            Ok(()) => {
                connection.execute("DELETE FROM derived_deletions WHERE blob_id = ?1", [id])?;
            }
            Err(error) => failure = Some(error),
        }
    }
    failure.map_or(Ok(()), Err)
}

#[cfg(test)]
fn cleanup_record(
    connection: &Connection,
    data_dir: &Path,
    id: &BlobId,
) -> Result<(), ObjectError> {
    cleanup_record_storage(
        connection,
        &super::BlobBackend::Disk(data_dir.to_owned()),
        id,
    )
}

fn cleanup_record_storage(
    connection: &Connection,
    backend: &super::BlobBackend,
    id: &BlobId,
) -> Result<(), ObjectError> {
    let exists: bool = sql::scalar(
        connection,
        "SELECT EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1)",
        [id],
    )?;
    if !exists {
        return Err(ObjectError::DerivedTransferNotFound);
    }
    for source in inputs(connection, id)? {
        backend.remove_pin(&source.id)?;
    }
    backend.remove_draft(id)?;
    backend.discard_unreferenced(connection, id)?;
    discard_record(connection, id)
}

fn discard_record(connection: &Connection, id: &BlobId) -> Result<(), ObjectError> {
    let transaction = connection.unchecked_transaction()?;
    sql::exec(
        &transaction,
        "DELETE FROM derived_inputs WHERE transfer_id = ?1",
        [id],
    )?;
    sql::exec(
        &transaction,
        "DELETE FROM derived_transfers WHERE id = ?1",
        [id],
    )?;
    transaction.commit().map_err(object_unavailable)
}

#[expect(
    clippy::disallowed_methods,
    reason = "transfer quotas and each retained transfer are inspected once during open-time recovery"
)]
pub(super) fn recover(
    connection: &Connection,
    data_dir: &Path,
    now: i64,
) -> Result<(), ObjectError> {
    reclaim_derived_deletions(connection, data_dir, None)?;
    let over_limit: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM derived_transfers GROUP BY ephemeral HAVING count(*) > ?1)",
        [MAX_TRANSFERS_PER_KIND],
        |row| row.get(0),
    )?;
    if over_limit {
        return Err(object_unavailable("too many derived transfers"));
    }
    let ids = connection
        .prepare_cached("SELECT id FROM derived_transfers ORDER BY id LIMIT ?1")?
        .query_map([2 * MAX_TRANSFERS_PER_KIND + 1], |row| {
            row.get::<_, BlobId>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if ids.len() > 2 * MAX_TRANSFERS_PER_KIND {
        return Err(object_unavailable("too many derived transfers"));
    }
    for id in ids {
        let (ephemeral, deadline): (bool, i64) = connection.query_row(
            "SELECT ephemeral, deadline FROM derived_transfers WHERE id = ?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
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
            super::blob_backend::restore_disk_draft(data_dir, &id)?;
            let stage = stage_path(data_dir, &id);
            let mut output = fs::OpenOptions::new().write(true).read(true).open(stage)?;
            if output.metadata()?.len() < progress.offset {
                return Err(object_unavailable(
                    "derived prefix is shorter than its checkpoint",
                ));
            }
            output.set_len(progress.offset)?;
            #[cfg(test)]
            fault::truncated(data_dir, &output.metadata()?);
            output.sync_all()?;
            // A same-length corrupted prefix must not inherit the acknowledged hash state.
            let mut hasher = Hasher::default();
            let mut remaining = progress.offset;
            let mut buffer = [0; BUFFER_SIZE];
            while remaining > 0 {
                let length = remaining.min(BUFFER_SIZE as u64) as usize;
                std::io::Read::read_exact(&mut output, &mut buffer[..length])?;
                hasher.update(&buffer[..length]);
                remaining -= length as u64;
            }
            if hasher.finish() != saved_hash.finish() {
                return Err(object_unavailable(
                    "derived prefix checksum differs from checkpoint",
                ));
            }
            for source in sources {
                let metadata = fs::symlink_metadata(stage_path(data_dir, &source.id))?;
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
    use crate::storage::test_fixtures::{
        PERMISSIVE, body, bucket, live, permissive_upload, read_all, version,
    };
    use bytes::Bytes;
    use md5::{Digest, Md5, digest::common::hazmat::SerializableState};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::UNIX_EPOCH;

    #[tokio::test]
    async fn md5_availability_survives_restart_without_component_count() {
        let (catalog, path) = setup().await;
        let original = put(&catalog, "source", b"same bytes").await;
        let transfer = ready(&catalog, "source").await;
        let record = catalog
            .publish_derived_prepared(
                &transfer.id,
                destination("multipart"),
                |_, record| {
                    record.md5_availability = crate::object::Md5Availability::Unavailable;
                    Ok(())
                },
                |_, _| Ok(()),
                None,
            )
            .await
            .unwrap();
        assert!(!record.has_md5());
        assert_eq!(record.component_count, None);
        assert_eq!(record.md5, original.md5);
        assert_eq!(record.crc32c, original.crc32c);
        catalog.close().await.unwrap();
        drop(catalog);
        let catalog = Catalog::open(&path).await.unwrap();
        let reopened = catalog
            .get_object(live("bucket", "multipart"), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        assert_eq!(reopened, record);
        let selected = source(&catalog, "multipart").await;
        let (activity, _updates) = tokio::sync::watch::channel(Default::default());
        let copy = crate::server::copy::execute(
            &catalog,
            crate::server::copy::CopyJob {
                source: selected,
                source_access: BucketAccess::Permissive,
                destination: destination("copy"),
                source_location: None,
                replay: None,
            },
            crate::server::copy::CopyBudget {
                config: &crate::config::Config::default(),
                deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(10),
                activity: &activity,
            },
            None,
        )
        .await
        .unwrap();
        assert!(copy.has_md5());
        assert_eq!(copy.md5, original.md5);
        assert_eq!(copy.crc32c, original.crc32c);
        let selected = source(&catalog, "multipart").await;
        let start = super::super::rewrite::RewriteStart {
            binding: super::super::rewrite::RewriteBinding {
                source_bucket: "bucket".into(),
                source_object: "multipart".into(),
                source_generation: record.generation,
                destination_bucket: "bucket".into(),
                destination_object: "rewrite".into(),
                parameters: Default::default(),
                metadata: None,
            },
            destination: DerivedDestination {
                expected: ExpectedHashes {
                    md5: Some(original.md5),
                    crc32c: Some(original.crc32c),
                },
                ..destination("rewrite")
            },
            source_conditions: Default::default(),
            source_metageneration: record.metageneration,
            source_access: BucketAccess::Permissive,
            source_key_sha256: None,
            budget: 1_048_576,
        };
        let (token, mut owner) = catalog
            .begin_rewrite_session(vec![selected.whole()], u64::MAX, start)
            .await
            .unwrap();
        owner.disarm();
        let result = catalog
            .continue_rewrite(
                &token,
                BucketAccess::Permissive,
                BucketAccess::Permissive,
                |record| Ok(serde_json::json!({"generation":record.generation})),
            )
            .await
            .unwrap();
        assert!(result.resource.is_some());
        let rewritten = catalog
            .get_object(live("bucket", "rewrite"), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        assert!(rewritten.has_md5());
        assert_eq!(rewritten.md5, original.md5);
        assert_eq!(rewritten.crc32c, original.crc32c);
        assert_eq!(
            catalog
                .get_object(live("bucket", "multipart"), Default::default(), PERMISSIVE)
                .await
                .unwrap(),
            record
        );
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    async fn setup() -> (Catalog, PathBuf) {
        let path = std::env::temp_dir().join(format!("gcs-mimic-derived-{}", random_id().unwrap()));
        let catalog = Catalog::open(&path).await.unwrap();
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        (catalog, path)
    }

    async fn put(catalog: &Catalog, name: &str, bytes: &'static [u8]) -> ObjectRecord {
        let body = body(vec![Bytes::from_static(bytes)]);
        catalog
            .put_object(permissive_upload("bucket", name, 1_000_000), body)
            .await
            .unwrap()
    }

    async fn source(catalog: &Catalog, name: &str) -> DerivedSource {
        catalog
            .resolve_derived_source(live("bucket", name), Default::default(), PERMISSIVE)
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
            .open_object(live("bucket", name), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        read_all(&mut reader).await
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
                version("bucket", "first", first.generation),
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        put(&catalog, "second", b"changed").await;
        let output = stage_path(&path, &transfer.id);
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
            &stage_path(&path, &transfer.id),
            &transfer.id.blob_path(&path),
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
                    PERMISSIVE,
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
            .get_object(live("bucket", "target"), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        assert_eq!(current, target);
        drop(connection);
        catalog
            .update_object_metadata(
                live("bucket", "source"),
                Default::default(),
                ObjectMetadataUpdate {
                    event_based_hold: Some(true),
                    ..Default::default()
                },
                |_| Ok(()),
                PERMISSIVE,
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
                    PERMISSIVE,
                    std::time::SystemTime::now(),
                )
            })
            .await;
        assert!(matches!(result, Err(ObjectError::ActiveHold(_))));
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
                    .resolve_derived_source(live("bucket", "source"), conditions, PERMISSIVE)
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
                .resolve_derived_source(live("bucket", "source"), Default::default(), &denied)
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
                        PERMISSIVE,
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
                        PERMISSIVE,
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
                .get_object(live("bucket", "same"), Default::default(), PERMISSIVE)
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
            target: version("bucket", "source", original.generation),
            conditions: Default::default(),
            access: BucketAccess::Permissive,
        };
        let mut missing = spec.clone();
        missing.target.name = "missing".into();
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
        let snapshot_id = source.id;
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
                    connection.execute("UPDATE startup_seed SET complete = 1", [])?;
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
                    connection.execute("UPDATE startup_seed SET complete = 1", [])?;
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
        use crate::clock::ManualClock;
        use std::sync::Arc;
        let (catalog, path) = setup().await;
        catalog.close().await.unwrap();
        drop(catalog);
        let clock = Arc::new(ManualClock::new(UNIX_EPOCH));
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
        clock.set(UNIX_EPOCH + std::time::Duration::from_secs(604800));
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
        let snapshot = source.id.staging_path(&path);
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
        let snapshot = source.id.staging_path(&path);
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
        fs::write(stage_path(&path, &transfer.id), b"bad").unwrap();
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
                    PERMISSIVE,
                    std::time::SystemTime::now(),
                )
            })
            .await
            .unwrap();
        assert_eq!(
            catalog
                .get_object(live("bucket", "source"), Default::default(), PERMISSIVE)
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
            .run(|connection| {
                Ok::<_, ObjectError>(
                    objects::lookup(connection, "bucket", "source")?
                        .unwrap()
                        .blob,
                )
            })
            .await
            .unwrap();
        let blob = id.blob_path(&path);
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
                    PERMISSIVE,
                    std::time::SystemTime::now(),
                )?;
                fs::remove_file(&failed_blob)?;
                fs::create_dir(&failed_blob)?;
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(bytes(&catalog, "copy").await, b"bytes");
        assert!(catalog.maintain_derived_transfers().await.is_err());
        fs::remove_dir(&blob).unwrap();
        assert_eq!(catalog.maintain_derived_transfers().await.unwrap(), None);
        let tagged = catalog
            .run(|connection| {
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
                        entered.send(transfer.id).unwrap();
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
        original.owner = None;
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
                .get_object(live("bucket", "source"), Default::default(), PERMISSIVE)
                .await
                .unwrap(),
            original
        );
        catalog
            .run(|connection| {
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
                Ok::<_, ObjectError>(())
            })
            .await
            .unwrap();
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }
}
