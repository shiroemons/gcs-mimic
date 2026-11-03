use super::{Catalog, CatalogError, system_time_millis, unavailable, unix_millis};
use crate::storage::{
    blob_id::BlobId,
    catalog::{bucket_incarnation, sql},
};

use crate::{
    bucket::BucketAccess,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate},
    storage::failure::{BackendCause, BackendFailure},
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

#[cfg(test)]
pub(super) mod golden_tests;
mod options;

/// Caps ACTIVE sessions, the only ones that own staged bytes.
pub(crate) const MAX_SESSIONS: usize = 128;
/// Caps COMPLETE and CANCELLED records kept after admission trims them. They exist so status
/// queries and repeated final requests get the original outcome, which clients ask for soon
/// after the transition; holding them against the active cap made every initiation fail for
/// seven days once 128 uploads had finished.
const MAX_TERMINAL_SESSIONS: i64 = 128;
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResumableSession {
    pub id: String,
    pub blob_id: BlobId,
    pub bucket: String,
    pub name: String,
    pub incarnation: String,
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

/// A nonqueued exclusive operation on one session; dropping always releases admission.
pub(crate) struct SessionLease {
    pub session: ResumableSession,
    pub(super) guard: std::sync::Arc<WriterGuard>,
}
pub(super) struct WriterGuard {
    pub(super) catalog: Catalog,
    pub(super) id: String,
    pub(super) _activity: super::Activity,
}
impl Drop for WriterGuard {
    fn drop(&mut self) {
        self.catalog
            .0
            .session_writers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        if self.id.starts_with("derived:") {
            self.catalog.0.derived_wake.notify_waiters();
        }
        if self.id.starts_with("derived:")
            || self.id.starts_with("rewrite:")
            || self.id.starts_with("multipart:")
        {
            self.catalog.0.maintenance_wake.notify_one();
        }
    }
}
impl SessionLease {
    #[cfg(test)]
    pub(crate) fn staging_path(&self) -> PathBuf {
        self.session
            .blob_id
            .staging_path(&self.guard.catalog.0.data_dir)
    }
}

fn random_id<const N: usize>() -> Result<String, CatalogError> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(unavailable)?;
    Ok(crate::hashing::encode_lower(&bytes))
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
        "SELECT NOT EXISTS(SELECT 1 FROM buckets LEFT JOIN bucket_incarnations ON name = bucket WHERE bucket IS NULL) AND NOT EXISTS(SELECT 1 FROM bucket_incarnations LEFT JOIN buckets ON bucket = name WHERE name IS NULL) AND (SELECT count(*) FROM resumable_sessions WHERE state = 'ACTIVE') <= 128 AND (SELECT count(*) FROM resumable_sessions) <= 256",
        "invalid resumable catalog state",
    )?;
    // Admission keeps at most MAX_SESSIONS active plus MAX_TERMINAL_SESSIONS terminal rows.
    let mut statement =
        connection.prepare_cached("SELECT * FROM resumable_sessions ORDER BY id LIMIT 257")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        read_session(row)?;
    }
    Ok(())
}

/// Checks the active cap and trims the oldest terminal records down to their own cap in the
/// creating transaction, so the catalog never holds more than both caps together. The returned
/// leases keep the trimmed sessions out of reach until the transaction ends.
fn admit_session(
    transaction: &rusqlite::Transaction<'_>,
    catalog: &Catalog,
) -> Result<Vec<WriterGuard>, CatalogError> {
    let (active, terminal): (usize, i64) = sql::row(
        transaction,
        "SELECT coalesce(sum(state = 'ACTIVE'), 0), coalesce(sum(state != 'ACTIVE'), 0) FROM resumable_sessions",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if active >= MAX_SESSIONS {
        return Err(unavailable(BackendFailure::RetainedSessionLimit));
    }
    let mut excess = terminal - MAX_TERMINAL_SESSIONS;
    let mut trimmed = Vec::new();
    if excess <= 0 {
        return Ok(trimmed);
    }
    // A completed session owns no staging bytes and never its published blob, which object
    // reclamation tracks. A cancelled one is reclaimable only after its cleanup journal entry
    // is gone; until then maintenance still needs the row.
    let candidates: Vec<(String, BlobId, String)> = transaction
        .prepare_cached("SELECT id, blob_id, state FROM resumable_sessions WHERE state = 'COMPLETE' OR (state = 'CANCELLED' AND NOT EXISTS(SELECT 1 FROM pending_blob_deletions WHERE blob_id = resumable_sessions.blob_id)) ORDER BY deadline, id LIMIT 256")
        .and_then(|mut statement| {
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect()
        })?;
    for (id, blob_id, state) in candidates {
        if excess <= 0 {
            break;
        }
        // A request holding the session (such as a finalizer re-reading its outcome) keeps it.
        let Ok(guard) = catalog.try_resumable_writer(&id) else {
            continue;
        };
        if state == "COMPLETE" {
            // Completion normally drops the RAM draft; this covers a failed drop.
            catalog.0.backend.finish_draft(&blob_id)?;
        }
        sql::exec(
            transaction,
            "DELETE FROM resumable_sessions WHERE id = ?1",
            [&id],
        )?;
        trimmed.push(guard);
        excess -= 1;
    }
    if excess > 0 {
        return Err(unavailable(BackendFailure::RetainedSessionLimit));
    }
    Ok(trimmed)
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
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let catalog = self.clone();
        let session = self.run_waiting(move |connection| -> Result<_,ResumableError> {
            let transaction = sql::immediate(connection).map_err(unavailable)?;
            let (_,delete_permission)=super::objects::authorized_bucket(&transaction,&bucket,&options.access)?;
            let previous=super::objects::lookup(&transaction,&bucket,&name)?;
            super::objects::check_publication(previous.as_ref(),crate::object::ObjectConditions::default(),delete_permission,catalog.0.clock.now())?;
            let _trimmed = admit_session(&transaction, &catalog)?;
            let incarnation: String = bucket_incarnation(&transaction, &bucket).optional().map_err(unavailable)?.ok_or(ObjectError::BucketNotFound)?;
            let id = random_id::<32>()?;
            let blob = super::super::blob_id::BlobId::generate().map_err(unavailable)?;
            let blob_id = blob;
            let now = unix_millis(catalog.0.clock.now())?;
            let deadline = now.checked_add(i64::try_from(SESSION_LIFETIME.as_millis()).map_err(unavailable)?).ok_or_else(|| unavailable("upload deadline overflow"))?;
            let encoded = encode_options(&options).to_string();
            let total = total.map(i64::try_from).transpose().map_err(unavailable)?;
            create_session_bytes(&catalog, &blob_id)?;
            let inserted = sql::exec(&transaction, "INSERT INTO resumable_sessions VALUES (?1,?2,?3,?4,?5,?6,?7,0,?8,?9,'ACTIVE',NULL)", params![id, blob_id, bucket, name, incarnation, encoded, total, now, deadline]);
            if let Err(error) = inserted { remove_session_stage(&catalog, &blob_id)?; return Err(unavailable(error).into()); }
            // A failed commit can be ambiguous; retain its file for reference-aware recovery.
            if let Err(error) = transaction.commit() {
                if catalog.0.backend.is_memory() {
                    let retained: bool = sql::scalar(connection, "SELECT EXISTS(SELECT 1 FROM resumable_sessions WHERE blob_id = ?1)", [&blob_id]).map_err(unavailable)?;
                    if !retained { remove_session_stage(&catalog, &blob_id)?; }
                }
                return Err(unavailable(error).into());
            }
            #[cfg(test)]
            super::fault::hit(&catalog.0.data_dir, super::fault::Step::SessionCreated).map_err(unavailable)?;
            sql::row(connection, "SELECT * FROM resumable_sessions WHERE id = ?1", [&id], read_session).map_err(|e|unavailable(e).into())
        }).await?;
        self.0.maintenance_wake.notify_one();
        Ok(session)
    }

    pub(crate) async fn resumable_session(
        &self,
        id: &str,
        bucket: &str,
    ) -> Result<ResumableSession, ResumableError> {
        if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ResumableError::NotFound);
        }
        let id = id.to_owned();
        let bucket = bucket.to_owned();
        let session = self
            .run_waiting(move |connection| {
                sql::row(
                    connection,
                    "SELECT * FROM resumable_sessions WHERE id = ?1 AND bucket = ?2",
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
        let guard = self.try_resumable_writer(id)?;
        let session = self.resumable_session(id, bucket).await?;
        Ok(SessionLease {
            session,
            guard: std::sync::Arc::new(guard),
        })
    }

    pub(super) fn try_resumable_writer(&self, id: &str) -> Result<WriterGuard, CatalogError> {
        let activity = self.activity()?;
        let mut writers = self.0.session_writers.lock().map_err(unavailable)?;
        if writers.len() >= MAX_SESSIONS || !writers.insert(id.to_owned()) {
            return Err(unavailable(BackendFailure::SessionBusy));
        }
        Ok(WriterGuard {
            catalog: self.clone(),
            id: id.to_owned(),
            _activity: activity,
        })
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
        let id = lease.session.id.clone();
        let old = lease.session.offset;
        let now = unix_millis(self.0.clock.now())?;
        let encoded = options.map(|o| encode_options(o).to_string());
        if encoded.as_ref().is_some_and(|v| v.len() > 65536) {
            return Err(ResumableError::Invalid(
                "Upload options exceed 64 KiB".into(),
            ));
        }
        let (changed, output) = self.run_waiting::<_, CatalogError>(move |connection| {
            let changed = sql::exec(connection, "UPDATE resumable_sessions SET offset = ?1, total = ?2, options = coalesce(?6, options) WHERE id = ?3 AND offset = ?4 AND state = 'ACTIVE' AND deadline > ?5", params![offset, total, id, old, now, encoded])?;
            if changed == 1 && let Some(output) = &mut output { output.acknowledge(offset); }
            Ok((changed, output))
        }).await?;
        if changed != 1 {
            self.resumable_session(&lease.session.id, &lease.session.bucket)
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
    json!(options::StoredOptions::from_options(o))
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
    let id: String = row.get(0)?;
    let blob_id: BlobId = row.get(1)?;
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(sql::conversion_error(
            0,
            Type::Text,
            "invalid upload opaque ID",
        ));
    }
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
        created: system_time_millis(row.get(8)?)?,
        deadline: system_time_millis(row.get(9)?)?,
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
