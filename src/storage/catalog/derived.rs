//! Generation-pinned, resumable byte derivation shared by copy, rewrite and compose.

use super::{
    Catalog, objects,
    resumable::WriterGuard,
    resumable::writers::{ClaimError, KeyOrigin, WriterKey},
    system_time_micros, system_time_millis,
    transfer_io::{self, stage_path},
    unix_micros, unix_millis,
};
use crate::storage::blob_id::BlobId;
use crate::storage::catalog::sql;
use crate::{
    bucket::{BucketAccess, CatalogError},
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

fn deferred_cleanup(error: &dyn std::fmt::Display) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        "derived_cleanup_deferred",
        serde_json::json!({"message":format!("unreferenced derived data is reclaimed later: {error}")}),
    );
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
            deferred_cleanup(&error);
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

enum BeginOutcome {
    Committed(DerivedTransfer),
    Rejected(ObjectError),
    Ambiguous(ObjectError),
}

struct BeginTransfer {
    id: BlobId,
    total: u64,
    ephemeral: bool,
    created: i64,
    deadline: i64,
}

fn begin_transfer(
    connection: &mut Connection,
    draft: &BeginTransfer,
    segments: &[DerivedSegment],
    bind: impl FnOnce(&Connection, &DerivedTransfer) -> Result<(), ObjectError>,
) -> BeginOutcome {
    let mut ambiguous = false;
    let result = (|| {
        let count: usize = sql::scalar(
            connection,
            "SELECT count(*) FROM derived_transfers WHERE ephemeral = ?1",
            [draft.ephemeral],
        )?;
        if count >= MAX_TRANSFERS_PER_KIND {
            return Err(object_unavailable("derived transfer limit reached"));
        }
        let transaction = sql::immediate(connection)?;
        sql::exec(
            &transaction,
            "INSERT INTO derived_transfers VALUES (?1, ?2, 0, ?3, 0, ?4, ?5, ?6, 0)",
            params![
                draft.id,
                draft.total,
                Hasher::default().md5_state().0.as_slice(),
                draft.ephemeral,
                draft.created,
                draft.deadline
            ],
        )?;
        insert_inputs(&transaction, &draft.id, segments)?;
        let transfer = DerivedTransfer {
            id: draft.id,
            total: draft.total,
            offset: 0,
            created: system_time_millis(draft.created)?,
            deadline: system_time_millis(draft.deadline)?,
        };
        bind(&transaction, &transfer)?;
        #[cfg(test)]
        fault::hit(&fault::database_dir(&transaction), Step::DerivedBeginCommit)
            .map_err(object_unavailable)?;
        if let Err(error) = transaction.commit() {
            // Failed probes retain every possibly committed reference for recovery.
            ambiguous = true;
            let committed: bool = sql::scalar(
                connection,
                "SELECT EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1)",
                [&draft.id],
            )?;
            ambiguous = false;
            if !committed {
                return Err(object_unavailable(error));
            }
        }
        Ok(transfer)
    })();
    match result {
        Ok(transfer) => BeginOutcome::Committed(transfer),
        Err(error) if ambiguous => BeginOutcome::Ambiguous(error),
        Err(error) => BeginOutcome::Rejected(error),
    }
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
                                deferred_cleanup(&error);
                            }
                            return;
                        }
                        Err(ObjectError::DerivedTransferBusy) => notified.await,
                        Err(error) => {
                            deferred_cleanup(&error);
                            return;
                        }
                    }
                }
            });
        } else {
            deferred_cleanup(
                &"abandoned derived transfer cleanup needs a runtime; deferred until expiry",
            );
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

struct TransferPublication<P, C> {
    id: BlobId,
    destination: DerivedDestination,
    backend: super::BlobBackend,
    clock: std::sync::Arc<dyn crate::clock::Clock>,
    principals: Option<super::principals::Profile>,
    replay: Option<super::upload_replay::CopyReplay>,
    prepare: P,
    complete: C,
}

struct Published {
    record: ObjectRecord,
    pins: Vec<BlobId>,
}

fn publish_transfer<P, C>(
    connection: &mut Connection,
    request: TransferPublication<P, C>,
) -> Result<Published, ObjectError>
where
    P: FnOnce(&Connection, &mut ObjectRecord) -> Result<(), ObjectError>,
    C: FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError>,
{
    let id = request.id;
    let backend = request.backend.clone();
    let now = request.clock.now();
    let (progress, hasher) = transfer(connection, &id, unix_millis(now)?)?;
    if progress.offset != progress.total {
        return Err(ObjectError::DerivedTransferIncomplete);
    }
    let pins = inputs(connection, &id)?
        .into_iter()
        .map(|source| source.id)
        .collect::<Vec<_>>();
    if backend.is_memory() {
        backend.publish_draft(&id)?;
    }
    let transaction = sql::immediate(connection)?;
    now.duration_since(std::time::UNIX_EPOCH)
        .map_err(object_unavailable)?;
    let micros = unix_micros(now)?;
    objects::authorized_bucket(
        &transaction,
        &request.destination.bucket,
        &request.destination.access,
    )?;
    let cached = request
        .replay
        .as_ref()
        .map(|replay| replay.cached(&transaction, micros))
        .transpose()?
        .flatten();
    let record = if let Some(record) = cached {
        sql::exec(
            &transaction,
            "INSERT INTO pending_blob_deletions(blob_id) VALUES (?1)",
            [id],
        )?;
        record
    } else {
        publish_destination(
            &transaction,
            request,
            &progress,
            hasher.finish(),
            now,
            micros,
        )?
    };
    sql::exec(
        &transaction,
        "DELETE FROM derived_inputs WHERE transfer_id=?1",
        [id],
    )?;
    sql::exec(
        &transaction,
        "DELETE FROM derived_transfers WHERE id=?1",
        [id],
    )?;
    let committed = objects::commit_publication(transaction);
    if backend.is_memory() {
        // A failed commit can still have published: release pins only after checking references.
        let retained: bool = sql::scalar(
            connection,
            "SELECT EXISTS(SELECT 1 FROM derived_transfers WHERE id=?1)",
            [id],
        )?;
        if !retained {
            for pin in &pins {
                if let Err(error) = backend.remove_pin(pin) {
                    deferred_cleanup(&error);
                }
            }
            if let Err(error) = backend.finish_draft(&id) {
                deferred_cleanup(&error);
            }
        }
    }
    committed?;
    Ok(Published {
        record,
        pins: if backend.is_memory() {
            Vec::new()
        } else {
            pins
        },
    })
}

fn publish_destination<P, C>(
    transaction: &rusqlite::Transaction<'_>,
    request: TransferPublication<P, C>,
    progress: &DerivedTransfer,
    hashes: Hashes,
    now: std::time::SystemTime,
    micros: i64,
) -> Result<ObjectRecord, ObjectError>
where
    P: FnOnce(&Connection, &mut ObjectRecord) -> Result<(), ObjectError>,
    C: FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError>,
{
    let TransferPublication {
        id,
        destination,
        principals,
        replay,
        prepare,
        complete,
        ..
    } = request;
    let (_, delete_permission) =
        objects::authorized_bucket(transaction, &destination.bucket, &destination.access)?;
    if replay.is_some() {
        super::upload_replay::capacity(transaction, micros)?;
    }
    let bucket = objects::BucketPublication::load(transaction, &destination.bucket)?;
    let storage_class = bucket.storage_class(destination.storage_class);
    let previous = objects::lookup(transaction, &destination.bucket, &destination.name)?;
    objects::check_publication(
        previous.as_ref(),
        destination.conditions,
        delete_permission,
        now,
    )?;
    if destination
        .expected
        .md5
        .is_some_and(|hash| hash != hashes.md5)
        || destination
            .expected
            .crc32c
            .is_some_and(|hash| hash != hashes.crc32c)
    {
        return Err(ObjectError::ChecksumMismatch);
    }
    let time = system_time_micros(micros)?;
    let version = objects::NewVersion {
        blob_id: id,
        assignment: objects::GenerationAssignment::Automatic,
        micros,
        origin: super::change_journal::origin(&destination.access),
        notification_mode: crate::notification::NotificationMode::Emit,
        owner_profile: principals.as_ref(),
        acl: super::acl::PublicationAcl::InheritDefault,
    };
    let (record, _retired, ()) = objects::publish_in_transaction(
        transaction,
        previous.as_ref(),
        &bucket,
        version,
        |generation| {
            let defaults = objects::PublicationDefaults {
                owner: Some(super::object_owner::for_publication(
                    transaction,
                    &destination.bucket,
                    &destination.access,
                    principals.as_ref(),
                )?),
                storage_class,
                time,
                event_based_hold: bucket.default_event_hold,
            };
            let mut record = defaults.record(
                ObjectTarget::new(destination.bucket, destination.name, generation),
                destination.content_type,
                progress.total,
                hashes,
            );
            record.component_count = destination.component_count;
            destination.metadata.apply(&mut record)?;
            sql::exec(
                transaction,
                "UPDATE derived_transfers SET publishing=1 WHERE id=?1",
                [id],
            )?;
            prepare(transaction, &mut record)?;
            Ok((record, ()))
        },
    )?;
    complete(transaction, &record)?;
    if let Some(replay) = replay {
        replay.store(transaction, micros, &record)?;
    }
    Ok(record)
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

pub(super) fn transfer(
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
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| object_unavailable("missing resolved source"))
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
                    #[cfg(all(test, unix))]
                    if let super::BlobBackend::Disk(root) = &backend {
                        fault::file_created(&id.staging_path(root));
                    }
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
        let admission = self
            .0
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|error| super::admission_error(&error))?;
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
        let catalog = self.clone();
        let result = crate::task::spawn_supervised("derived-begin", async move {
            let (_permit, _writer) = (permit, writer);
            let files = backend.clone();
            #[cfg(test)]
            let root = catalog.0.data_dir.clone();
            let prepared = crate::task::run_blocking("derived-create", move || {
                files.create_draft(&id)?;
                #[cfg(all(test, unix))]
                if let super::BlobBackend::Disk(root) = &files {
                    fault::file_created(&id.staging_path(root));
                }
                #[cfg(test)]
                fault::hit(&root, Step::DerivedDraftSync).map_err(object_unavailable)?;
                // This sync makes both the draft and the selected input links durable before SQL.
                #[cfg(unix)]
                files.sync_staging_dir()?;
                Ok::<_, ObjectError>(())
            })
            .await
            .map_err(object_unavailable)
            .flatten();
            let (outcome, returned) = match prepared {
                Ok(()) => {
                    catalog
                        .run_admitted(admission, move |connection| {
                            let outcome = begin_transfer(
                                connection,
                                &BeginTransfer {
                                    id,
                                    total,
                                    ephemeral,
                                    created,
                                    deadline,
                                },
                                &segments,
                                bind,
                            );
                            if !matches!(outcome, BeginOutcome::Rejected(_)) {
                                for segment in &mut segments {
                                    segment.source.backend = None;
                                }
                            }
                            Ok::<_, ObjectError>((outcome, segments))
                        })
                        .await?
                }
                Err(error) => (BeginOutcome::Rejected(error), segments),
            };
            let result = match outcome {
                BeginOutcome::Committed(transfer) => Ok(transfer),
                BeginOutcome::Ambiguous(error) => Err(error),
                BeginOutcome::Rejected(error) => {
                    let cleanup = crate::task::run_blocking("derived-begin-cleanup", move || {
                        drop(returned);
                        backend.remove_draft(&id)
                    })
                    .await;
                    match cleanup {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => deferred_cleanup(&error),
                        Err(error) => deferred_cleanup(&error),
                    }
                    Err(error)
                }
            };
            catalog.0.maintenance_wake.notify_one();
            result
        })
        .await?;
        if result.is_ok()
            && let Some(owner) = &mut owner
        {
            owner.disarm();
        }
        result
    }

    fn derived_guard(&self, id: &BlobId) -> Result<WriterGuard, ObjectError> {
        self.claim_writer(WriterKey::Derived(*id), KeyOrigin::Catalog)
            .map_err(|error| match error {
                ClaimError::Busy => ObjectError::DerivedTransferBusy,
                error => CatalogError::from(error).into(),
            })
    }

    #[cfg(test)]
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
                Ok((progress, hasher, inputs(connection, &query)?))
            }).await?;
            crate::task::run_blocking("derived-restore", move || backend.restore_draft(&query)).await??;
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
                    let length = usize::try_from(remaining.min(BUFFER_SIZE as u64)).map_err(object_unavailable)?;
                    let bytes = buffer.get_mut(..length).ok_or_else(|| object_unavailable("invalid derived buffer length"))?;
                    input.read_exact(bytes).await?;
                    #[cfg(test)]
                    fault::hit(&catalog.0.data_dir, Step::BodyWrite).map_err(object_unavailable)?;
                    output.write(bytes).await?;
                    hasher.update(bytes);
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
            let backend = catalog.0.backend.clone();
            let clock = catalog.0.clock.clone();
            let principals = catalog.0.principals.get().cloned();
            let result = async {
            catalog.run_upload_admitted(execution, move |connection| {
                let (progress, hasher) = transfer(connection, &query, unix_millis(clock.now())?)?;
                let sources = inputs(connection, &query)?;
                if progress.offset != progress.total { return Err(ObjectError::DerivedTransferIncomplete); }
                Ok((hasher, sources))
            }).await?;
            if !backend.is_memory() {
                let files = backend.clone();
                crate::task::run_blocking("derived-restore", move || files.restore_draft(&id)).await??;
                catalog.publish_staged(id.staging_path(&catalog.0.data_dir), id.blob_path(&catalog.0.data_dir)).await?;
            }
            let publication = TransferPublication { id, destination, backend, clock: catalog.0.clock.clone(), principals, replay, prepare, complete };
            let published = catalog.run_upload(move |connection| publish_transfer(connection, publication)).await?;
            catalog.release_transfer(DiscardedTransfer { id, pins: published.pins }).await;
            Ok::<_, ObjectError>(published.record)
            }.await;
            if result.is_ok() {
                catalog.reclaim_after_commit().await;
            }
            catalog.0.maintenance_wake.notify_one();
            // Check SQL references even after an ambiguous commit error before removing bytes.
            if matches!(&result, Err(error) if !error.is_backend_failure() && !matches!(error, ObjectError::DerivedTransferBusy | ObjectError::DerivedTransferIncomplete | ObjectError::DerivedTransferNotFound))
                && let Err(error) = catalog.cleanup_derived(&id).await {
                deferred_cleanup(&error);
            }
            result
        }).await?
    }

    async fn cleanup_derived(&self, id: &BlobId) -> Result<(), ObjectError> {
        let id = *id;
        let discarded = self
            .run_upload(move |connection| discard_transfer(connection, &id))
            .await?;
        self.release_transfer(discarded).await;
        self.reclaim_after_commit().await;
        Ok(())
    }

    async fn release_transfer(&self, discarded: DiscardedTransfer) {
        let backend = self.0.backend.clone();
        #[cfg(test)]
        let root = self.0.data_dir.clone();
        match crate::task::run_blocking("derived-release", move || {
            #[cfg(test)]
            fault::hit(&root, Step::DerivedCleanup).map_err(object_unavailable)?;
            for pin in discarded.pins {
                if let Err(error) = backend.remove_pin(&pin) {
                    deferred_cleanup(&error);
                }
            }
            backend.remove_draft(&discarded.id)?;
            #[cfg(test)]
            fault::hit(&root, Step::DerivedCleaned).map_err(object_unavailable)?;
            Ok::<_, ObjectError>(())
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => deferred_cleanup(&error),
            Err(error) => deferred_cleanup(&error),
        }
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
        let mut after = String::new();
        loop {
            let cursor = after.clone();
            let due = self
                .run(move |connection| {
                    connection.prepare_cached("SELECT id FROM derived_transfers WHERE deadline <= ?1 AND id > ?2 ORDER BY id LIMIT 128")?
                        .query_map(params![now, cursor], |row| row.get::<_, BlobId>(0))?
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map_err(object_unavailable)
                })
                .await
                .map_err(super::unavailable)?;
            if due.is_empty() {
                break;
            }
            for id in due {
                after = id.to_hex();
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
fn reclaim_derived_deletions(connection: &Connection, data_dir: &Path) -> Result<(), ObjectError> {
    let ids = connection
        .prepare_cached("SELECT blob_id FROM derived_deletions ORDER BY blob_id LIMIT 128")?
        .query_map([], |row| row.get::<_, BlobId>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
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

#[must_use = "a discarded transfer must release its staged data"]
struct DiscardedTransfer {
    id: BlobId,
    pins: Vec<BlobId>,
}

fn discard_transfer(
    connection: &mut Connection,
    id: &BlobId,
) -> Result<DiscardedTransfer, ObjectError> {
    let transaction = sql::immediate(connection)?;
    let exists: bool = sql::scalar(
        &transaction,
        "SELECT EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1)",
        [id],
    )?;
    if !exists {
        return Err(ObjectError::DerivedTransferNotFound);
    }
    let pins = inputs(&transaction, id)?
        .into_iter()
        .map(|source| source.id)
        .collect();
    if !objects::blob_referenced(&transaction, id)? {
        sql::exec(
            &transaction,
            "INSERT OR IGNORE INTO pending_blob_deletions VALUES (?1)",
            [id],
        )?;
    }
    sql::exec(
        &transaction,
        "DELETE FROM derived_inputs WHERE transfer_id=?1",
        [id],
    )?;
    sql::exec(
        &transaction,
        "DELETE FROM derived_transfers WHERE id=?1",
        [id],
    )?;
    #[cfg(test)]
    fault::hit(
        &fault::database_dir(&transaction),
        Step::DerivedCleanupCommit,
    )
    .map_err(object_unavailable)?;
    transaction.commit()?;
    Ok(DiscardedTransfer { id: *id, pins })
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
    reclaim_derived_deletions(connection, data_dir)?;
    let over_limit: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM derived_transfers GROUP BY ephemeral HAVING count(*) > ?1)",
        [MAX_TRANSFERS_PER_KIND],
        |row| row.get(0),
    )?;
    if over_limit {
        return Err(object_unavailable("too many derived transfers"));
    }
    let ids = connection
        .prepare_cached(
            "SELECT id, ephemeral, deadline FROM derived_transfers ORDER BY id LIMIT ?1",
        )?
        .query_map([2 * MAX_TRANSFERS_PER_KIND + 1], |row| {
            Ok((
                row.get::<_, BlobId>(0)?,
                row.get::<_, bool>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if ids.len() > 2 * MAX_TRANSFERS_PER_KIND {
        return Err(object_unavailable("too many derived transfers"));
    }
    for (id, ephemeral, deadline) in ids {
        if ephemeral || deadline <= now {
            discard_record(connection, &id)?;
            continue;
        }
        let validity = (|| {
            let (progress, saved_hash) = transfer(connection, &id, now)?;
            let sources = inputs(connection, &id)?;
            if sources.is_empty()
                || sources.len() > 32
                || sources
                    .iter()
                    .try_fold(0_u64, |total, source| total.checked_add(source.length))
                    != Some(progress.total)
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
                let length = usize::try_from(remaining.min(BUFFER_SIZE as u64))
                    .map_err(object_unavailable)?;
                let bytes = buffer
                    .get_mut(..length)
                    .ok_or_else(|| object_unavailable("invalid derived buffer length"))?;
                std::io::Read::read_exact(&mut output, bytes)?;
                hasher.update(bytes);
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
mod tests;
