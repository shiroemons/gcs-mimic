use super::{Catalog, CatalogError, Incarnation, UnixMillis, Work, unavailable};
use crate::storage::{
    blob_id::BlobId,
    catalog::{
        bucket_incarnation,
        sql::{self, CommitFailure},
    },
};

use crate::{
    bucket::BucketAccess,
    hashing::{Hasher, HasherState, Hashes},
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate},
    storage::failure::{BackendCause, object_unavailable},
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
#[cfg(test)]
use std::path::PathBuf;
use std::{borrow::Cow, fmt, time::SystemTime};

mod admission;
#[cfg(test)]
pub(super) mod golden_tests;
mod options;
pub(super) mod writers;
use admission::admit_session;
pub(super) use admission::open;
pub(super) use writers::WriterGuard;
use writers::{KeyOrigin, WriterKey};

pub(super) const SCHEMA: &str = "CREATE TABLE bucket_incarnations (
    bucket TEXT PRIMARY KEY COLLATE BINARY,
    incarnation TEXT NOT NULL UNIQUE CHECK(length(incarnation) = 32)
) WITHOUT ROWID;
CREATE TABLE resumable_sessions (
    id TEXT PRIMARY KEY CHECK(length(id) = 64),
    blob_id TEXT NOT NULL UNIQUE CHECK(length(blob_id) = 32),
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND 1024),
    incarnation TEXT NOT NULL CHECK(length(incarnation) = 32),
    options TEXT NOT NULL CHECK(length(CAST(options AS BLOB)) <= 65536),
    total INTEGER CHECK(total >= 0),
    offset INTEGER NOT NULL CHECK(offset >= 0 AND (total IS NULL OR offset <= total)),
    created INTEGER NOT NULL,
    deadline INTEGER NOT NULL CHECK(deadline = created + 604800000),
    state TEXT NOT NULL CHECK(state IN ('ACTIVE', 'COMPLETE', 'CANCELLED')),
    completion TEXT CHECK(length(CAST(completion AS BLOB)) <= 65536),
    CHECK((state = 'COMPLETE' AND completion IS NOT NULL AND total = offset) OR (state != 'COMPLETE' AND completion IS NULL))
) WITHOUT ROWID;
CREATE INDEX resumable_sessions_deadline ON resumable_sessions(deadline, id);
PRAGMA user_version = 7;";

/// Schema version 56: the running digest of the bytes a session has persisted, saved with each
/// checkpoint so that a status reply can report it and a restart resumes it without rereading
/// the staged bytes. Sessions an earlier release staged keep NULL and report no digest.
pub(super) const RUNNING_HASH_SCHEMA_VERSION: i64 = 56;
pub(super) const RUNNING_HASH_SCHEMA: &str = "ALTER TABLE resumable_sessions ADD COLUMN running_hash BLOB CHECK(running_hash IS NULL OR length(running_hash) = 101);
PRAGMA user_version = 56;";
const _: () = assert!(crate::hashing::HASHER_STATE_BYTES == 101);

/// The columns of schema version 7, which [`validate`] reads before a catalog is upgraded.
macro_rules! session_columns_v7 {
    () => { "id, blob_id, bucket, name, incarnation, options, total, offset, created, deadline, state, completion" };
}

macro_rules! session_columns {
    () => {
        concat!(session_columns_v7!(), ", running_hash")
    };
}

/// One session by ID, with the columns [`read_session`] reads, for sibling modules that cannot
/// name `session_columns!`.
pub(super) const SELECT_SESSION: &str = concat!(
    "SELECT ",
    session_columns!(),
    " FROM resumable_sessions WHERE id = ?1"
);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionState {
    Active,
    Complete(StoredCompletion),
    Cancelled,
    Aborted,
    InvalidContentRange,
    Overrun(Overrun),
}

impl SessionState {
    pub(crate) fn is_complete(&self) -> bool {
        matches!(self, Self::Complete(_))
    }
}

/// A completed upload's resource in the format its adapter saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum StoredCompletion {
    Json(Map<String, Value>),
    Proto(Vec<u8>),
    /// Earlier readers accepted any syntactically valid JSON here.
    Unrecognized(Value),
    /// The schema's CHECK forbids a NULL payload on complete rows. Keep the original
    /// missing-resource response if a damaged catalog violates that constraint.
    Missing,
}

impl From<Value> for StoredCompletion {
    fn from(value: Value) -> Self {
        match value {
            Value::Object(map) => Self::Json(map),
            value @ Value::Array(_) => Vec::<u8>::deserialize(&value)
                .map_or_else(|_| Self::Unrecognized(value), Self::Proto),
            value @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)) => {
                Self::Unrecognized(value)
            }
        }
    }
}

impl StoredCompletion {
    pub(crate) fn into_value(self) -> Option<Value> {
        match self {
            Self::Json(map) => Some(Value::Object(map)),
            Self::Proto(bytes) => Some(Value::Array(bytes.into_iter().map(Value::from).collect())),
            Self::Unrecognized(value) => Some(value),
            Self::Missing => None,
        }
    }
}

/// A chunk that carried more bytes than the session's total left room for. Cloud Storage
/// refused it, failed the session and repeated the same refusal for every later request
/// (recorded 2026-11-15, case obj-resumable-declared-length).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Overrun {
    /// The bytes the chunk's `Content-Range` carries. Cloud Storage counted the bytes it had
    /// read when it noticed, so its count depends on its buffering.
    pub received: u64,
    /// The bytes between the chunk's start and the session's total.
    pub room: u64,
}

impl fmt::Display for Overrun {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { received, room } = self;
        write!(
            f,
            "Invalid request.  There were {received} byte(s) (or more) in the request body.  There should have been {room} byte(s) according to the Content-Range header."
        )
    }
}

crate::choice::string_enum! {
    table
    /// The stored `state` column.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(super) enum StoredState as as_str {
        Active => "ACTIVE",
        Complete => "COMPLETE",
        Cancelled => "CANCELLED",
    };
    sql(parse, "invalid upload state")
}

/// Why the emulator cancelled a session. Kept in the options document's `terminal_error`, so
/// catalogs written before these reasons need no migration: the reasons without data are the
/// strings earlier releases wrote there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum TerminalReason {
    Aborted,
    ContentRange,
    Overrun(Overrun),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResumableOptions {
    pub content_type: String,
    pub metadata: ObjectMetadataUpdate,
    pub expected: ExpectedHashes,
    pub conditions: ObjectConditions,
    pub access: BucketAccess,
    pub origin: Option<String>,
    pub projection: crate::json::Projection,
    pub response_fields: Option<String>,
}

impl ResumableOptions {
    pub(crate) fn new(access: BucketAccess) -> Self {
        Self {
            content_type: String::new(),
            metadata: Default::default(),
            expected: Default::default(),
            conditions: Default::default(),
            access,
            origin: None,
            projection: crate::json::Projection::NoAcl,
            response_fields: None,
        }
    }
}

crate::storage::opaque_id::opaque_id! {
    /// A resumable upload session's capability, carried in its session URI as `upload_id`.
    pub(crate) struct SessionId([u8; 32]);
    invalid = "invalid upload opaque ID";
    column = text;
}

impl SessionId {
    fn generate() -> Result<Self, getrandom::Error> {
        crate::storage::opaque_id::random().map(Self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResumableSession {
    pub id: SessionId,
    pub blob_id: BlobId,
    pub bucket: String,
    pub name: String,
    pub incarnation: Incarnation,
    pub options: ResumableOptions,
    pub total: Option<u64>,
    pub offset: u64,
    pub created: SystemTime,
    pub deadline: SystemTime,
    pub state: SessionState,
    /// The digests of the `offset` persisted bytes, continued by each chunk; `None` for a
    /// session an earlier release staged.
    pub running_hash: Option<HasherState>,
}

impl ResumableSession {
    pub(crate) fn progress(&self) -> Result<Progress, ResumableError> {
        let hashes = self
            .running_hash
            .as_ref()
            .map(HasherState::digest)
            .transpose()
            .map_err(object_unavailable)?;
        Ok(Progress {
            offset: self.offset,
            hashes,
        })
    }
}

/// How much of an open upload a session has persisted, as a status reply reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Progress {
    pub offset: u64,
    /// The digests of those bytes; `None` for a session an earlier release staged.
    pub hashes: Option<Hashes>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResumableError {
    NotFound,
    Gone,
    Cancelled,
    InvalidContentRange,
    Overrun(Overrun),
    Invalid(String),
    Object(ObjectError),
}

impl fmt::Display for ResumableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("upload session not found"),
            Self::Gone => f.write_str("upload session is no longer active"),
            Self::Cancelled => f.write_str("clientClosedRequest"),
            Self::InvalidContentRange => f.write_str("Failed to parse Content-Range header."),
            Self::Overrun(overrun) => overrun.fmt(f),
            Self::Invalid(message) => f.write_str(message),
            Self::Object(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for ResumableError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Object(error) => error.source(),
            Self::NotFound
            | Self::Gone
            | Self::Cancelled
            | Self::InvalidContentRange
            | Self::Overrun(_)
            | Self::Invalid(_) => None,
        }
    }
}
impl From<ObjectError> for ResumableError {
    fn from(error: ObjectError) -> Self {
        Self::Object(error)
    }
}
impl From<CatalogError> for ResumableError {
    fn from(error: CatalogError) -> Self {
        Self::Object(ObjectError::from(error))
    }
}

/// An exclusive operation on one session; dropping always releases its writer.
pub(crate) struct SessionLease {
    pub session: ResumableSession,
    pub(super) guard: std::sync::Arc<WriterGuard>,
}
impl SessionLease {
    /// The session's running digest of its `offset` persisted bytes, to continue over the bytes
    /// that follow them; `None` for a session an earlier release staged, which has none.
    pub(super) fn running_hasher(&self) -> Result<Option<Hasher>, ObjectError> {
        self.session
            .running_hash
            .as_ref()
            .map(|state| Hasher::resume(state, self.session.offset))
            .transpose()
            .map_err(object_unavailable)
    }

    /// The digests of the object that the session's persisted bytes followed by `parts` would
    /// make, without writing anything; `None` for a session an earlier release staged, which has
    /// no running digest.
    pub(crate) fn digest_with(
        &self,
        parts: &[bytes::Bytes],
    ) -> Result<Option<Hashes>, ResumableError> {
        let Some(mut hasher) = self.running_hasher()? else {
            return Ok(None);
        };
        for part in parts {
            hasher.update(part);
        }
        Ok(Some(hasher.finish()))
    }

    #[cfg(test)]
    pub(crate) fn staging_path(&self) -> PathBuf {
        self.session
            .blob_id
            .staging_path(&self.guard.catalog.0.storage.data_dir)
    }
}

fn create_session_bytes(catalog: &Catalog, blob_id: &BlobId) -> Result<(), CatalogError> {
    catalog
        .0
        .storage
        .backend
        .create_resumable_draft(blob_id)
        .map_err(unavailable)
}

fn remove_session_stage(catalog: &Catalog, blob_id: &BlobId) -> Result<(), CatalogError> {
    catalog
        .0
        .storage
        .backend
        .remove_new_draft(blob_id)
        .map_err(unavailable)
}

struct CreateSession {
    id: SessionId,
    blob_id: BlobId,
    bucket: String,
    name: String,
    options: ResumableOptions,
    total: Option<u64>,
}

fn insert_session(
    connection: &mut Connection,
    catalog: &Catalog,
    request: &CreateSession,
) -> Result<ResumableSession, CommitFailure<ResumableError>> {
    let transaction = sql::immediate(connection)
        .map_err(|error| CommitFailure::Rejected(unavailable(error).into()))?;
    stage_session(&transaction, catalog, request).map_err(CommitFailure::Rejected)?;
    // Once commit starts, only a successful reference probe can authorize removing the draft.
    if let Err(error) = transaction.commit() {
        return Err(failed_commit(
            connection,
            catalog.0.storage.backend.kind(),
            &request.blob_id,
            error,
        ));
    }
    // After the commit every failure leaves the session in place.
    #[cfg(test)]
    super::fault::hit(
        &catalog.0.storage.data_dir,
        super::fault::Step::SessionCreated,
    )
    .map_err(|error| CommitFailure::Ambiguous(unavailable(error).into()))?;
    sql::row(connection, SELECT_SESSION, [&request.id], read_session)
        .map_err(|error| CommitFailure::Ambiguous(unavailable(error).into()))
}

/// Inserts the session row; nothing is committed yet.
fn stage_session(
    transaction: &sql::WriteTx<'_>,
    catalog: &Catalog,
    request: &CreateSession,
) -> Result<(), ResumableError> {
    let (_, delete_permission) =
        super::objects::authorized_bucket(transaction, &request.bucket, &request.options.access)?;
    // Refuses a class the bucket's location cannot hold before any byte arrives rather than at
    // publication; no recording shows when Cloud Storage refuses it.
    super::objects::BucketPublication::load(transaction, &request.bucket)?
        .check_class(request.options.metadata.storage_class)?;
    let previous = super::objects::lookup(transaction, &request.bucket, &request.name)?;
    let now = catalog.0.policy.clock.now();
    super::objects::check_publication(
        previous.as_ref(),
        ObjectConditions::default(),
        delete_permission,
        now,
    )?;
    let _trimmed = admit_session(transaction, catalog)?;
    let incarnation = bucket_incarnation(transaction, &request.bucket)
        .optional()
        .map_err(unavailable)?
        .ok_or(ObjectError::BucketNotFound)?;
    let created = UnixMillis::try_from(now)?;
    let deadline = created
        .checked_add_millis(super::SESSION_LIFETIME_MILLIS)
        .ok_or_else(|| unavailable("upload deadline overflow"))?;
    let encoded = encode_new_options(transaction, &request.bucket, &request.options)?;
    let total = request
        .total
        .map(i64::try_from)
        .transpose()
        .map_err(unavailable)?;
    sql::exec(
        transaction,
        "INSERT INTO resumable_sessions VALUES (?1,?2,?3,?4,?5,?6,?7,0,?8,?9,'ACTIVE',NULL,?10)",
        params![
            request.id,
            request.blob_id,
            request.bucket,
            request.name,
            incarnation,
            encoded,
            total,
            created,
            deadline,
            Hasher::default().save().to_bytes()
        ],
    )
    .map_err(unavailable)?;
    Ok(())
}

/// A memory catalog can confirm that a failed commit left nothing behind; a disk catalog cannot,
/// and neither can a failed probe.
fn failed_commit(
    connection: &Connection,
    backend: crate::config::Backend,
    blob_id: &BlobId,
    error: rusqlite::Error,
) -> CommitFailure<ResumableError> {
    let error = ResumableError::from(unavailable(error));
    match backend {
        crate::config::Backend::Disk => CommitFailure::Ambiguous(error),
        crate::config::Backend::Memory => match sql::scalar::<bool>(
            connection,
            "SELECT EXISTS(SELECT 1 FROM resumable_sessions WHERE blob_id=?1)",
            [blob_id],
        ) {
            Ok(false) => CommitFailure::Rejected(error),
            Ok(true) => CommitFailure::Ambiguous(error),
            Err(probe) => CommitFailure::Ambiguous(unavailable(probe).into()),
        },
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the schema migration backfills incarnations once"
)]
pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(SCHEMA)?;
    transaction.execute(
        "INSERT INTO bucket_incarnations SELECT name, lower(hex(randomblob(16))) FROM buckets",
        [],
    )?;
    transaction.commit().map_err(unavailable)
}

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    sql::validate(
        connection,
        "SELECT NOT EXISTS(SELECT 1 FROM buckets LEFT JOIN bucket_incarnations ON name = bucket WHERE bucket IS NULL) AND NOT EXISTS(SELECT 1 FROM bucket_incarnations LEFT JOIN buckets ON bucket = name WHERE name IS NULL)",
        "invalid resumable catalog state",
    )?;
    // Row by row: nothing bounds how many sessions a catalog holds, so neither may this check.
    // It runs before an older catalog is upgraded, so it reads the version-7 columns.
    let mut statement = connection.prepare_cached(concat!(
        "SELECT ",
        session_columns_v7!(),
        ", NULL FROM resumable_sessions ORDER BY id"
    ))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        read_session(row)?;
    }
    Ok(())
}

/// Every stored running digest decodes and covers exactly its session's persisted bytes.
pub(super) fn validate_running_hash(connection: &Connection) -> Result<(), CatalogError> {
    let mut statement = connection.prepare_cached(
        "SELECT offset, running_hash FROM resumable_sessions WHERE running_hash IS NOT NULL ORDER BY id",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        running_hash(row, 0, 1)?;
    }
    Ok(())
}

/// The running digest in `hash_column`, which must cover the session offset in
/// `offset_column`.
fn running_hash(
    row: &Row<'_>,
    offset_column: usize,
    hash_column: usize,
) -> rusqlite::Result<Option<HasherState>> {
    use rusqlite::types::Type;
    let offset: u64 = row.get(offset_column)?;
    let Some(bytes) = row.get::<_, Option<Vec<u8>>>(hash_column)? else {
        return Ok(None);
    };
    let state = HasherState::from_bytes(&bytes)
        .map_err(|error| sql::conversion_error(hash_column, Type::Blob, error))?;
    if state.bytes() != offset {
        return Err(sql::conversion_error(
            hash_column,
            Type::Blob,
            "upload running hash does not cover the persisted offset",
        ));
    }
    Ok(Some(state))
}

impl Catalog {
    pub(crate) async fn create_resumable_session(
        &self,
        bucket: &str,
        name: &str,
        options: ResumableOptions,
        total: Option<u64>,
    ) -> Result<ResumableSession, ResumableError> {
        // Cloud Storage opened the session without evaluating the preconditions (recorded 200
        // for `ifGenerationMatch=0` over a live object); they are checked when the upload
        // finalizes, from the copy the session keeps.
        self.check_upload(
            bucket,
            name,
            crate::object::ObjectConditions::default(),
            &options.access,
        )
        .await?;
        let activity = self
            .await_activity(Work::New)
            .await
            .map_err(ObjectError::from)?;
        let request = CreateSession {
            id: SessionId::generate().map_err(unavailable)?,
            blob_id: BlobId::generate().map_err(unavailable)?,
            bucket: bucket.to_owned(),
            name: name.to_owned(),
            options,
            total,
        };
        let catalog = self.clone();
        crate::task::spawn_supervised("resumable-create", async move {
            let _activity = activity;
            let blob_id = request.blob_id;
            let files = catalog.clone();
            crate::task::run_blocking("session-create", move || create_session_bytes(&files, &blob_id)).await.map_err(unavailable)??;
            let metadata = catalog.clone();
            let result = catalog.run(move |connection| Ok::<_, ResumableError>(insert_session(connection, &metadata, &request))).await;
            let result = result.unwrap_or_else(|error| Err(CommitFailure::Ambiguous(error)));
            catalog.0.maintenance.wake.notify_one();
            match result {
                Ok(session) => Ok(session),
                Err(CommitFailure::Ambiguous(error)) => Err(error),
                Err(CommitFailure::Rejected(error)) => {
                    let files = catalog.clone();
                    let cleanup = crate::task::run_blocking("session-reject-cleanup", move || remove_session_stage(&files, &blob_id)).await;
                    let cleanup = cleanup.map_err(unavailable).and_then(|result| result);
                    if let Err(error) = cleanup {
                        crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, crate::diagnostics::Event::UploadCleanupDeferred, json!({"message":format!("rejected session draft cleanup failed: {error}")}));
                    }
                    Err(error)
                }
            }
        }).await.map_err(unavailable)?
    }

    /// The session a client names by the `upload_id` of its session URI. An ID that is not a
    /// session ID cannot name any session.
    pub(crate) async fn resumable_session(
        &self,
        id: &str,
        bucket: &str,
    ) -> Result<ResumableSession, ResumableError> {
        let id = id.parse().map_err(|_| ResumableError::NotFound)?;
        self.session_by_id(id, bucket).await
    }

    async fn session_by_id(
        &self,
        id: SessionId,
        bucket: &str,
    ) -> Result<ResumableSession, ResumableError> {
        let bucket = bucket.to_owned();
        let session = self
            .run_read(move |connection| {
                sql::row(
                    connection,
                    concat!(
                        "SELECT ",
                        session_columns!(),
                        " FROM resumable_sessions WHERE id = ?1 AND bucket = ?2"
                    ),
                    params![id, bucket],
                    read_session,
                )
                .optional()
                .map_err(unavailable)
            })
            .await?
            .ok_or(ResumableError::NotFound)?;
        if self.0.policy.clock.now() >= session.deadline {
            return Err(ResumableError::NotFound);
        }
        match session.state {
            SessionState::Cancelled => return Err(ResumableError::Cancelled),
            SessionState::Aborted => return Err(ResumableError::Gone),
            SessionState::InvalidContentRange => return Err(ResumableError::InvalidContentRange),
            SessionState::Overrun(overrun) => return Err(ResumableError::Overrun(overrun)),
            SessionState::Active | SessionState::Complete(_) => {}
        }
        Ok(session)
    }

    pub(crate) async fn lock_resumable_session(
        &self,
        id: &str,
        bucket: &str,
    ) -> Result<SessionLease, ResumableError> {
        let id: SessionId = id.parse().map_err(|_| ResumableError::NotFound)?;
        let guard = self
            .wait_for_writer(WriterKey::Resumable(id), KeyOrigin::Holder)
            .await
            .map_err(CatalogError::from)?;
        let session = self.session_by_id(id, bucket).await?;
        Ok(SessionLease {
            session,
            guard: std::sync::Arc::new(guard),
        })
    }

    #[cfg(test)]
    pub(super) fn try_resumable_writer(&self, id: SessionId) -> Result<WriterGuard, CatalogError> {
        self.claim_writer(WriterKey::Resumable(id), KeyOrigin::Holder)
            .map_err(CatalogError::from)
    }

    /// A lease from another catalog is a server-side mix-up that no client request can cause, so
    /// it fails as unavailable instead of as an invalid request.
    fn check_lease_owner(&self, lease: &SessionLease) -> Result<(), ResumableError> {
        if std::sync::Arc::ptr_eq(&self.0, &lease.guard.catalog.0) {
            Ok(())
        } else {
            Err(ResumableError::Object(object_unavailable(
                "Session belongs to another catalog",
            )))
        }
    }

    pub(crate) async fn prepare_resumable_status(
        &self,
        lease: &SessionLease,
    ) -> Result<(), ResumableError> {
        self.check_lease_owner(lease)?;
        drop(
            super::transfer_io::Output::open(
                self,
                &lease.session.blob_id,
                lease.session.offset,
                lease.guard.clone(),
            )
            .await?,
        );
        Ok(())
    }

    /// Called only with an exclusive lease after file sync; it never transfers body bytes.
    pub(crate) async fn checkpoint_resumable_session(
        &self,
        lease: &mut SessionLease,
        offset: u64,
        total: Option<u64>,
    ) -> Result<(), ResumableError> {
        self.checkpoint_resumable_output(lease, offset, total, None, None)
            .await
            .map(|_| ())
    }

    /// Persists the session's new offset and total, with what a chunk `appended` and the
    /// options its request settled. The running digest and the offset share one update, so
    /// a restart resumes the digest exactly where the persisted bytes end.
    pub(super) async fn checkpoint_resumable_output(
        &self,
        lease: &mut SessionLease,
        offset: u64,
        total: Option<u64>,
        options: Option<&ResumableOptions>,
        appended: Option<Appended>,
    ) -> Result<Option<super::transfer_io::Output>, ResumableError> {
        self.check_lease_owner(lease)?;
        if self.0.policy.clock.now() >= lease.session.deadline {
            return Err(ResumableError::NotFound);
        }
        if lease.session.state != SessionState::Active {
            return Err(ResumableError::Gone);
        }
        if offset < lease.session.offset
            || total.is_some_and(|n| offset > n)
            || lease.session.total.is_some_and(|n| Some(n) != total)
        {
            return Err(ResumableError::Invalid(
                "inconsistent upload offset or total".into(),
            ));
        }
        let id = lease.session.id;
        let old = lease.session.offset;
        let now = UnixMillis::try_from(self.0.policy.clock.now())?;
        let encoded = options
            .map(encode_options)
            .transpose()?
            .map(|options| options.to_string());
        if encoded
            .as_ref()
            .is_some_and(|v| v.len() > super::MAX_STORED_DOCUMENT_BYTES)
        {
            return Err(ResumableError::Invalid(
                "Upload options exceed 64 KiB".into(),
            ));
        }
        let (mut output, running_hash) = appended.map_or_default(
            |Appended {
                 output,
                 running_hash,
             }| (Some(output), running_hash),
        );
        if running_hash
            .as_ref()
            .is_some_and(|state| state.bytes() != offset)
        {
            return Err(ResumableError::Object(object_unavailable(
                "upload running hash does not cover the checkpoint offset",
            )));
        }
        let stored_hash = running_hash.as_ref().map(HasherState::to_bytes);
        // Bytes the catalog did not digest leave the session without a running digest.
        let (changed, output) = self.run::<_, CatalogError>(move |connection| {
            let changed = sql::exec_on_writer(connection, "UPDATE resumable_sessions SET offset = ?1, total = ?2, options = coalesce(?6, options), running_hash = iif(?1 = ?4, coalesce(?7, running_hash), ?7) WHERE id = ?3 AND offset = ?4 AND state = 'ACTIVE' AND deadline > ?5", params![offset, total, id, old, now, encoded, stored_hash])?;
            if changed == 1 && let Some(output) = &mut output { output.acknowledge(offset); }
            Ok((changed, output))
        }).await?;
        if changed != 1 {
            self.resumable_session(&lease.session.id.to_string(), &lease.session.bucket)
                .await?;
            return Err(ResumableError::Gone);
        }
        lease.session.offset = offset;
        lease.session.total = total;
        if let Some(options) = options {
            lease.session.options = options.clone();
        }
        if running_hash.is_some() || offset != old {
            lease.session.running_hash = running_hash;
        }
        Ok(output)
    }
}

/// The bytes a chunk appended to its session, already durable when they are checkpointed.
pub(super) struct Appended {
    pub(super) output: super::transfer_io::Output,
    /// The session's running digest continued over the appended bytes; `None` while the
    /// session has none.
    pub(super) running_hash: Option<HasherState>,
}

pub(super) fn encode_options(o: &ResumableOptions) -> Result<Value, CatalogError> {
    serde_json::to_value(options::StoredOptions::from_options(o, None)?).map_err(unavailable)
}

/// Encodes the options of a new upload into `bucket`, whose project cannot change while the
/// upload lives: deleting the bucket ends its incarnation and with it the upload.
pub(super) fn encode_new_options(
    connection: &Connection,
    bucket: &str,
    o: &ResumableOptions,
) -> Result<String, CatalogError> {
    let project: Option<String> = match &o.access {
        crate::bucket::BucketAccess::Policy {
            identity: Some(_), ..
        } => sql::optional_scalar(
            connection,
            "SELECT project FROM buckets WHERE name = ?1",
            [bucket],
        )?,
        crate::bucket::BucketAccess::Permissive
        | crate::bucket::BucketAccess::Admin(_)
        | crate::bucket::BucketAccess::Policy { identity: None, .. } => None,
    };
    serde_json::to_string(&options::StoredOptions::from_options(
        o,
        project.as_deref(),
    )?)
    .map_err(unavailable)
}

/// Stored resumable session options that no longer decode; the catalog treats them as corrupt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::storage) struct InvalidOptions(BackendCause);

impl InvalidOptions {
    pub(in crate::storage) fn new(reason: impl Into<Cow<'static, str>>) -> Self {
        Self(BackendCause::new(reason))
    }

    /// Options whose embedded value failed to decode; the decoding error stays the source.
    fn from_error(error: impl Into<crate::storage::failure::Source>) -> Self {
        Self(BackendCause::from_error(error))
    }

    pub(in crate::storage) fn with_source(
        reason: &'static str,
        error: impl Into<crate::storage::failure::Source>,
    ) -> Self {
        Self(BackendCause::with_source(reason, error))
    }
}

impl fmt::Display for InvalidOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for InvalidOptions {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

pub(super) fn decode_options(v: &Value) -> Result<ResumableOptions, InvalidOptions> {
    options::decode(v)
}

pub(super) fn read_session(row: &Row<'_>) -> rusqlite::Result<ResumableSession> {
    use rusqlite::types::Type;
    let id: SessionId = row.get(0)?;
    let blob_id: BlobId = row.get(1)?;
    let options = row.get::<_, sql::Json<Value>>(5)?.0;
    let stored: StoredState = row.get(10)?;
    let invalid_state = || sql::conversion_error(10, Type::Text, "invalid upload state");
    // Keep terminal reasons in the versioned options document so existing catalogs need no migration.
    let reason = options
        .get("terminal_error")
        .map(TerminalReason::deserialize)
        .transpose()
        .map_err(|_| invalid_state())?;
    let incomplete_state = match (stored, reason) {
        (StoredState::Active, None) => Some(SessionState::Active),
        (StoredState::Complete, None) => None,
        (StoredState::Cancelled, None) => Some(SessionState::Cancelled),
        (StoredState::Cancelled, Some(TerminalReason::Aborted)) => Some(SessionState::Aborted),
        (StoredState::Cancelled, Some(TerminalReason::ContentRange)) => {
            Some(SessionState::InvalidContentRange)
        }
        (StoredState::Cancelled, Some(TerminalReason::Overrun(overrun))) => {
            Some(SessionState::Overrun(overrun))
        }
        (StoredState::Active | StoredState::Complete, Some(_)) => return Err(invalid_state()),
    };
    let completion: Option<String> = row.get(11)?;
    Ok(ResumableSession {
        id,
        blob_id,
        bucket: row.get(2)?,
        name: row.get(3)?,
        incarnation: row.get(4)?,
        options: decode_options(&options)
            .map_err(|error| sql::conversion_error(5, Type::Text, error))?,
        total: row.get(6)?,
        offset: row.get(7)?,
        created: row.get::<_, UnixMillis>(8)?.system_time()?,
        deadline: row.get::<_, UnixMillis>(9)?.system_time()?,
        state: {
            let completion = completion
                .map(|text| {
                    serde_json::from_str::<Value>(&text)
                        .map_err(|error| sql::conversion_error(11, Type::Text, error))
                })
                .transpose()?;
            incomplete_state.unwrap_or_else(|| {
                SessionState::Complete(completion.map_or(StoredCompletion::Missing, Into::into))
            })
        },
        running_hash: running_hash(row, 7, 12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{MapPatch, Patch};

    #[test]
    fn stored_completion_shapes_round_trip_as_the_original_json_value() {
        for (text, expected) in [
            ("{}", StoredCompletion::Json(Map::new())),
            ("[]", StoredCompletion::Proto(vec![])),
            ("[0,255]", StoredCompletion::Proto(vec![0, 255])),
        ] {
            let original: Value = serde_json::from_str(text).unwrap();
            let completion = StoredCompletion::from(original.clone());
            assert_eq!(completion, expected, "{text}");
            assert_eq!(completion.into_value(), Some(original), "{text}");
        }
        for text in [
            "[256]",
            "[-1]",
            "[1.0]",
            "[1,\"a\"]",
            "[-0]",
            "null",
            "true",
            "1",
            "\"text\"",
        ] {
            let original: Value = serde_json::from_str(text).unwrap();
            let completion = StoredCompletion::from(original.clone());
            assert!(
                matches!(completion, StoredCompletion::Unrecognized(_)),
                "{text}"
            );
            assert_eq!(completion.into_value(), Some(original), "{text}");
        }
        assert_eq!(StoredCompletion::Missing.into_value(), None);
    }

    /// Only a memory catalog's probe can show that a failed commit left nothing; a failed probe
    /// reports its own error and keeps the draft.
    #[test]
    fn a_failed_session_commit_is_rejected_only_when_a_memory_probe_finds_nothing() {
        use crate::config::Backend;
        let busy = || {
            rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                None,
            )
        };
        let blob_id: BlobId = "00000000000000000000000000000001".parse().unwrap();
        let sessions = |rows: &str| {
            let connection = Connection::open_in_memory().unwrap();
            connection
                .execute_batch(&format!(
                    "CREATE TABLE resumable_sessions (blob_id TEXT); {rows}"
                ))
                .unwrap();
            connection
        };
        let empty = sessions("");
        let committed =
            sessions("INSERT INTO resumable_sessions VALUES ('00000000000000000000000000000001');");
        let unprobed = Connection::open_in_memory().unwrap();
        let commit_error = busy().to_string();
        for (connection, backend, rejected) in [
            (&empty, Backend::Disk, false),
            (&committed, Backend::Disk, false),
            (&empty, Backend::Memory, true),
            (&committed, Backend::Memory, false),
        ] {
            let (CommitFailure::Rejected(error) | CommitFailure::Ambiguous(error)) =
                failed_commit(connection, backend, &blob_id, busy());
            assert!(error.to_string().ends_with(&commit_error), "{error}");
            assert_eq!(
                matches!(
                    failed_commit(connection, backend, &blob_id, busy()),
                    CommitFailure::Rejected(_)
                ),
                rejected,
                "{backend:?}"
            );
        }
        let CommitFailure::Ambiguous(error) =
            failed_commit(&unprobed, Backend::Memory, &blob_id, busy())
        else {
            panic!("a failed probe keeps the draft");
        };
        assert!(
            error.to_string().contains("no such table"),
            "the probe's own error: {error}"
        );
    }

    #[tokio::test]
    async fn rejected_creation_releases_the_draft_but_post_commit_failure_retains_it() {
        use super::super::fault::{Fault, Step};
        for memory in [false, true] {
            let dir = crate::test_support::TestDir::new();
            let backing = if memory {
                super::super::open::Backing::test_memory(dir.0.clone())
            } else {
                super::super::open::Backing::test_disk(dir.0.clone())
            };
            let catalog = Catalog::open_backend(
                backing,
                std::sync::Arc::new(crate::clock::SystemClock),
                super::super::admin::LocalAdmin::Disabled,
            )
            .await
            .unwrap();
            catalog
                .insert_bucket(crate::storage::test_fixtures::bucket("bucket", "project"))
                .await
                .unwrap();
            catalog.run::<_, CatalogError>(|connection| {
                connection.execute_batch("CREATE TEMP TRIGGER reject_session BEFORE INSERT ON resumable_sessions BEGIN SELECT RAISE(ABORT,'session rejected'); END;")?;
                Ok(())
            }).await.unwrap();
            let options = ResumableOptions::new(BucketAccess::Permissive);
            assert!(
                catalog
                    .create_resumable_session("bucket", "object", options.clone(), None)
                    .await
                    .is_err()
            );
            let rows: i64 = catalog
                .run(|connection| {
                    sql::scalar(connection, "SELECT count(*) FROM resumable_sessions", [])
                        .map_err(unavailable)
                })
                .await
                .unwrap();
            assert_eq!(rows, 0);
            if !memory {
                assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
            }
            catalog
                .run::<_, CatalogError>(|connection| {
                    connection.execute_batch("DROP TRIGGER reject_session")?;
                    Ok(())
                })
                .await
                .unwrap();
            catalog.inject(
                Step::SessionCreated,
                0,
                Fault::Io(std::io::ErrorKind::Other),
            );
            assert!(
                catalog
                    .create_resumable_session("bucket", "object", options, None)
                    .await
                    .is_err()
            );
            let session = catalog
                .run(|connection| {
                    sql::row(
                        connection,
                        concat!("SELECT ", session_columns!(), " FROM resumable_sessions"),
                        [],
                        read_session,
                    )
                    .map_err(unavailable)
                })
                .await
                .unwrap();
            catalog
                .0
                .storage
                .backend
                .open_output(
                    &session.blob_id,
                    0,
                    super::super::blob_backend::ReadAccess::WriteOnly,
                    std::sync::Arc::new(()),
                )
                .await
                .unwrap();
            assert_eq!(
                catalog
                    .resumable_session(&session.id.to_string(), "bucket")
                    .await
                    .unwrap()
                    .state,
                SessionState::Active
            );
            catalog.close().await.unwrap();
        }
    }

    const OPTIONS_JSON: &str = r#"{"version":2,"content_type":"text/plain","metadata":{"replace":false,"eventBasedHold":null,"temporaryHold":null,"customTime":null,"contentType":null,"contentEncoding":null,"contentDisposition":null,"contentLanguage":null,"cacheControl":null,"metadata":null,"retention":null,"customerKeySha256":null},"md5":null,"crc32c":null,"conditions":[null,null,null,null],"access":null,"origin":null,"full":false,"response_fields":null}"#;

    #[test]
    fn persisted_options_keep_their_json_bytes() {
        let mut options = ResumableOptions::new(BucketAccess::Permissive);
        options.content_type = "text/plain".into();
        let encoded = encode_options(&options).unwrap().to_string();
        assert_eq!(encoded, OPTIONS_JSON);
        assert_eq!(
            decode_options(&serde_json::from_str(&encoded).unwrap()).unwrap(),
            options
        );
    }

    #[test]
    fn legacy_options_keep_nullable_metadata_and_conditions() {
        let legacy = json!({"content_type":"text/plain","metadata":{"contentType":"text/custom","metadata":{"key":"value"}},"conditions":[0,null,1,null],"access":null,"full":true});
        let options = decode_options(&legacy).unwrap();
        assert_eq!(
            options.metadata.content_type,
            Patch::Set("text/custom".into())
        );
        assert_eq!(
            options.metadata.metadata,
            MapPatch::Merge(std::collections::BTreeMap::from([(
                "key".into(),
                Some("value".into())
            )]))
        );
        assert_eq!(options.conditions.if_generation_match, Some(0));
        assert_eq!(options.projection, crate::json::Projection::Full);
    }

    #[test]
    fn nullable_updates_keep_removal_distinct_from_omission() {
        let mut options = ResumableOptions::new(BucketAccess::Permissive);
        options.metadata.content_type = Patch::Clear;
        options.metadata.metadata = MapPatch::Clear;
        options.metadata.retention = Patch::Clear;
        let encoded = encode_options(&options).unwrap();
        assert_eq!(
            encoded["metadata"]["contentType"].to_string(),
            r#"{"value":null}"#
        );
        assert_eq!(
            encoded["metadata"]["metadata"].to_string(),
            r#"{"value":null}"#
        );
        assert_eq!(
            encoded["metadata"]["retention"].to_string(),
            r#"{"value":null}"#
        );
        assert_eq!(decode_options(&encoded).unwrap(), options);
        for field in ["contentType", "metadata"] {
            let mut invalid = encoded.clone();
            invalid["metadata"][field] = json!({});
            let error = decode_options(&invalid).unwrap_err();
            assert_eq!(error.to_string(), "invalid resumable options");
            assert!(std::error::Error::source(&error).is_some());
        }
    }

    #[test]
    fn failed_session_migration_rolls_back_tables_indexes_and_version() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE buckets(name TEXT); CREATE TABLE resumable_sessions(sentinel TEXT); PRAGMA user_version = 6;").unwrap();
        assert!(migrate(&mut connection).is_err());
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
                .unwrap(),
            6
        );
        assert_eq!(connection.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('bucket_incarnations','resumable_sessions_deadline')",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    }
}
