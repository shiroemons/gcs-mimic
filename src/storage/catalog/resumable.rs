use super::{Catalog, CatalogError, Incarnation, UnixMillis, Work, unavailable};
use crate::storage::{
    blob_id::BlobId,
    catalog::{bucket_incarnation, sql},
};

use crate::{
    bucket::BucketAccess,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate},
    storage::failure::BackendCause,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Value, json};
#[cfg(test)]
use std::path::PathBuf;
use std::{
    borrow::Cow,
    fmt,
    time::{Duration, SystemTime},
};

mod admission;
#[cfg(test)]
pub(super) mod golden_tests;
mod options;
pub(super) mod writers;
use admission::admit_session;
pub(super) use admission::open;
pub(super) use writers::WriterGuard;
use writers::{KeyOrigin, WriterKey};

pub(crate) const SESSION_LIFETIME: Duration = Duration::from_secs(604800);
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

macro_rules! session_columns {
    () => { "id, blob_id, bucket, name, incarnation, options, total, offset, created, deadline, state, completion" };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionState {
    Active,
    Complete,
    Cancelled,
    Aborted,
    InvalidContentRange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResumableOptions {
    pub content_type: String,
    pub metadata: ObjectMetadataUpdate,
    pub expected: ExpectedHashes,
    pub conditions: ObjectConditions,
    pub access: BucketAccess,
    pub origin: Option<String>,
    pub projection_full: bool,
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
            projection_full: false,
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
    pub completion: Option<Value>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResumableError {
    NotFound,
    Gone,
    Cancelled,
    InvalidContentRange,
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
            Self::Invalid(message) => f.write_str(message),
            Self::Object(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for ResumableError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Object(error) => Some(error),
            _ => None,
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
    #[cfg(test)]
    pub(crate) fn staging_path(&self) -> PathBuf {
        self.session
            .blob_id
            .staging_path(&self.guard.catalog.0.data_dir)
    }
}

fn create_session_bytes(catalog: &Catalog, blob_id: &BlobId) -> Result<(), CatalogError> {
    catalog
        .0
        .backend
        .create_resumable_draft(blob_id)
        .map_err(unavailable)
}

fn remove_session_stage(catalog: &Catalog, blob_id: &BlobId) -> Result<(), CatalogError> {
    catalog
        .0
        .backend
        .remove_new_draft(blob_id)
        .map_err(unavailable)
}

enum CreateFailure {
    Rejected(ResumableError),
    Ambiguous(ResumableError),
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
) -> Result<ResumableSession, CreateFailure> {
    let mut ambiguous = false;
    let result = (|| -> Result<_, ResumableError> {
        let transaction = sql::immediate(connection).map_err(unavailable)?;
        let (_, delete_permission) = super::objects::authorized_bucket(
            &transaction,
            &request.bucket,
            &request.options.access,
        )?;
        let previous = super::objects::lookup(&transaction, &request.bucket, &request.name)?;
        let now = catalog.0.clock.now();
        super::objects::check_publication(
            previous.as_ref(),
            ObjectConditions::default(),
            delete_permission,
            now,
        )?;
        let _trimmed = admit_session(&transaction, catalog)?;
        let incarnation = bucket_incarnation(&transaction, &request.bucket)
            .optional()
            .map_err(unavailable)?
            .ok_or(ObjectError::BucketNotFound)?;
        let created = UnixMillis::try_from(now)?;
        let deadline = created
            .checked_add_millis(i64::try_from(SESSION_LIFETIME.as_millis()).map_err(unavailable)?)
            .ok_or_else(|| unavailable("upload deadline overflow"))?;
        let encoded = encode_new_options(&transaction, &request.bucket, &request.options)
            .map_err(unavailable)?;
        let total = request
            .total
            .map(i64::try_from)
            .transpose()
            .map_err(unavailable)?;
        sql::exec(
            &transaction,
            "INSERT INTO resumable_sessions VALUES (?1,?2,?3,?4,?5,?6,?7,0,?8,?9,'ACTIVE',NULL)",
            params![
                request.id,
                request.blob_id,
                request.bucket,
                request.name,
                incarnation,
                encoded,
                total,
                created,
                deadline
            ],
        )
        .map_err(unavailable)?;
        // Once commit starts, only a successful reference probe can authorize removing the draft.
        ambiguous = true;
        if let Err(error) = transaction.commit() {
            if catalog.0.backend.is_memory() {
                let retained: bool = sql::scalar(
                    connection,
                    "SELECT EXISTS(SELECT 1 FROM resumable_sessions WHERE blob_id=?1)",
                    [&request.blob_id],
                )
                .map_err(unavailable)?;
                ambiguous = retained;
            }
            return Err(unavailable(error).into());
        }
        #[cfg(test)]
        super::fault::hit(&catalog.0.data_dir, super::fault::Step::SessionCreated)
            .map_err(unavailable)?;
        sql::row(
            connection,
            concat!(
                "SELECT ",
                session_columns!(),
                " FROM resumable_sessions WHERE id=?1"
            ),
            [&request.id],
            read_session,
        )
        .map_err(|error| unavailable(error).into())
    })();
    result.map_err(|error| {
        if ambiguous {
            CreateFailure::Ambiguous(error)
        } else {
            CreateFailure::Rejected(error)
        }
    })
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
    let mut statement = connection.prepare_cached(concat!(
        "SELECT ",
        session_columns!(),
        " FROM resumable_sessions ORDER BY id"
    ))?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        read_session(row)?;
    }
    Ok(())
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
            let result = result.unwrap_or_else(|error| Err(CreateFailure::Ambiguous(error)));
            catalog.0.maintenance_wake.notify_one();
            match result {
                Ok(session) => Ok(session),
                Err(CreateFailure::Ambiguous(error)) => Err(error),
                Err(CreateFailure::Rejected(error)) => {
                    let files = catalog.clone();
                    let cleanup = crate::task::run_blocking("session-reject-cleanup", move || remove_session_stage(&files, &blob_id)).await;
                    let cleanup = cleanup.map_err(unavailable).and_then(|result| result);
                    if let Err(error) = cleanup {
                        crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "upload_cleanup_deferred", json!({"message":format!("rejected session draft cleanup failed: {error}")}));
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
            .run(move |connection| {
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
        if self.0.clock.now() >= session.deadline {
            return Err(ResumableError::NotFound);
        }
        match session.state {
            SessionState::Cancelled => return Err(ResumableError::Cancelled),
            SessionState::Aborted => return Err(ResumableError::Gone),
            SessionState::InvalidContentRange => return Err(ResumableError::InvalidContentRange),
            SessionState::Active | SessionState::Complete => {}
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
            .wait_for_writer(WriterKey::Resumable(id), KeyOrigin::Client)
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
        self.claim_writer(WriterKey::Resumable(id), KeyOrigin::Client)
            .map_err(CatalogError::from)
    }

    pub(crate) async fn prepare_resumable_status(
        &self,
        lease: &SessionLease,
    ) -> Result<(), ResumableError> {
        if !std::sync::Arc::ptr_eq(&self.0, &lease.guard.catalog.0) {
            return Err(ResumableError::Invalid(
                "Session belongs to another catalog".into(),
            ));
        }
        drop(
            super::transfer_io::Output::open(self, &lease.session.blob_id, lease.session.offset)
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
        self.checkpoint_resumable_options(lease, offset, total, None)
            .await
    }

    pub(super) async fn checkpoint_resumable_options(
        &self,
        lease: &mut SessionLease,
        offset: u64,
        total: Option<u64>,
        options: Option<&ResumableOptions>,
    ) -> Result<(), ResumableError> {
        self.checkpoint_resumable_output(lease, offset, total, options, None)
            .await
            .map(|_| ())
    }

    pub(super) async fn checkpoint_resumable_output(
        &self,
        lease: &mut SessionLease,
        offset: u64,
        total: Option<u64>,
        options: Option<&ResumableOptions>,
        mut output: Option<super::transfer_io::Output>,
    ) -> Result<Option<super::transfer_io::Output>, ResumableError> {
        if !std::sync::Arc::ptr_eq(&self.0, &lease.guard.catalog.0) {
            return Err(ResumableError::Invalid(
                "Session belongs to another catalog".into(),
            ));
        }
        if self.0.clock.now() >= lease.session.deadline {
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
        let now = UnixMillis::try_from(self.0.clock.now())?;
        let encoded = options.map(|o| encode_options(o).to_string());
        if encoded.as_ref().is_some_and(|v| v.len() > 65536) {
            return Err(ResumableError::Invalid(
                "Upload options exceed 64 KiB".into(),
            ));
        }
        let (changed, output) = self.run::<_, CatalogError>(move |connection| {
            let changed = sql::exec(connection, "UPDATE resumable_sessions SET offset = ?1, total = ?2, options = coalesce(?6, options) WHERE id = ?3 AND offset = ?4 AND state = 'ACTIVE' AND deadline > ?5", params![offset, total, id, old, now, encoded])?;
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
        Ok(output)
    }
}

pub(super) fn encode_options(o: &ResumableOptions) -> Value {
    json!(options::StoredOptions::from_options(o, None))
}

/// Encodes the options of a new upload into `bucket`, whose project cannot change while the
/// upload lives: deleting the bucket ends its incarnation and with it the upload.
pub(super) fn encode_new_options(
    connection: &Connection,
    bucket: &str,
    o: &ResumableOptions,
) -> rusqlite::Result<String> {
    let project: Option<String> = match &o.access {
        crate::bucket::BucketAccess::Policy {
            identity: Some(_), ..
        } => sql::optional_scalar(
            connection,
            "SELECT project FROM buckets WHERE name = ?1",
            [bucket],
        )?,
        _ => None,
    };
    Ok(json!(options::StoredOptions::from_options(o, project.as_deref())).to_string())
}

/// Stored resumable session options that no longer decode; the catalog treats them as corrupt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::storage) struct InvalidOptions(BackendCause);

impl InvalidOptions {
    pub(in crate::storage) fn new(reason: impl Into<Cow<'static, str>>) -> Self {
        Self(BackendCause::new(reason))
    }

    /// Options whose embedded value failed to decode; the decoding error stays the source.
    fn from_error(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self(BackendCause::from_error(error))
    }

    pub(in crate::storage) fn with_source(
        reason: &'static str,
        error: impl std::error::Error + Send + Sync + 'static,
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
    let options = serde_json::from_str::<Value>(&row.get::<_, String>(5)?)
        .map_err(|error| sql::conversion_error(5, Type::Text, error))?;
    // Keep terminal reasons in the versioned options document so existing catalogs need no migration.
    let state = match (
        row.get::<_, String>(10)?.as_str(),
        options.get("terminal_error"),
    ) {
        ("ACTIVE", None) => SessionState::Active,
        ("COMPLETE", None) => SessionState::Complete,
        ("CANCELLED", None) => SessionState::Cancelled,
        ("CANCELLED", Some(Value::String(reason))) if reason == "aborted" => SessionState::Aborted,
        ("CANCELLED", Some(Value::String(reason))) if reason == "content-range" => {
            SessionState::InvalidContentRange
        }
        _ => {
            return Err(sql::conversion_error(
                10,
                Type::Text,
                "invalid upload state",
            ));
        }
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
        state,
        completion: completion
            .map(|v| {
                serde_json::from_str(&v)
                    .map_err(|error| sql::conversion_error(11, Type::Text, error))
            })
            .transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejected_creation_releases_the_draft_but_post_commit_failure_retains_it() {
        use super::super::fault::{Fault, Step};
        for memory in [false, true] {
            let dir = crate::test_support::TestDir::new();
            let catalog = Catalog::open_backend(
                dir.0.clone(),
                std::sync::Arc::new(crate::clock::SystemClock),
                super::super::admin::LocalAdmin::Disabled,
                memory.then_some((8192, 1024 * 1024)),
                None,
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
                .backend
                .open_output(
                    &session.blob_id,
                    0,
                    super::super::blob_backend::ReadAccess::WriteOnly,
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
        let encoded = encode_options(&options).to_string();
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
            Some(Some("text/custom".into()))
        );
        assert_eq!(
            options.metadata.metadata,
            Some(Some(std::collections::BTreeMap::from([(
                "key".into(),
                Some("value".into())
            )])))
        );
        assert_eq!(options.conditions.if_generation_match, Some(0));
        assert!(options.projection_full);
    }

    #[test]
    fn nullable_updates_keep_removal_distinct_from_omission() {
        let mut options = ResumableOptions::new(BucketAccess::Permissive);
        options.metadata.content_type = Some(None);
        options.metadata.metadata = Some(None);
        options.metadata.retention = Some(None);
        let encoded = encode_options(&options);
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
