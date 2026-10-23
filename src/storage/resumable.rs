use super::{Catalog, CatalogError, system_time, unavailable, unix_millis};

use crate::{
    auth::{Identity, OBJECTS_CREATE},
    bucket::BucketAccess,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate},
};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde_json::{Value, json};
use std::{
    fmt, fs,
    path::PathBuf,
    time::{Duration, SystemTime},
};

pub const MAX_SESSIONS: usize = 128;
pub const SESSION_LIFETIME: Duration = Duration::from_secs(604800);
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
pub enum SessionState {
    Active,
    Complete,
    Cancelled,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumableOptions {
    pub content_type: String,
    pub metadata: ObjectMetadataUpdate,
    pub expected: ExpectedHashes,
    pub conditions: ObjectConditions,
    pub access: BucketAccess,
    pub origin: Option<String>,
    pub projection_full: bool,
    pub response_fields: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumableSession {
    pub id: String,
    pub blob_id: String,
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
pub enum ResumableError {
    NotFound,
    Gone,
    Invalid(String),
    Object(ObjectError),
}

impl fmt::Display for ResumableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("upload session not found"),
            Self::Gone => f.write_str("upload session is no longer active"),
            Self::Invalid(message) => f.write_str(message),
            Self::Object(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for ResumableError {}
impl From<ObjectError> for ResumableError {
    fn from(error: ObjectError) -> Self {
        Self::Object(error)
    }
}
impl From<CatalogError> for ResumableError {
    fn from(error: CatalogError) -> Self {
        Self::Object(ObjectError::Unavailable(error.to_string()))
    }
}

/// A nonqueued exclusive operation on one session; dropping always releases admission.
pub struct SessionLease {
    pub session: ResumableSession,
    guard: WriterGuard,
}
pub(super) struct WriterGuard {
    pub(super) catalog: Catalog,
    pub(super) id: String,
}
impl Drop for WriterGuard {
    fn drop(&mut self) {
        self.catalog
            .0
            .session_writers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}
impl SessionLease {
    pub fn staging_path(&self) -> PathBuf {
        self.guard
            .catalog
            .0
            .data_dir
            .join("staging")
            .join(&self.session.blob_id)
    }
}

fn random_id<const N: usize>() -> Result<String, CatalogError> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(unavailable)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    transaction
        .execute(
            "INSERT INTO bucket_incarnations SELECT name, lower(hex(randomblob(16))) FROM buckets",
            [],
        )
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    let valid: bool = connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM buckets LEFT JOIN bucket_incarnations ON name = bucket WHERE bucket IS NULL) AND NOT EXISTS(SELECT 1 FROM bucket_incarnations LEFT JOIN buckets ON bucket = name WHERE name IS NULL) AND (SELECT count(*) FROM resumable_sessions) <= 128", [], |r| r.get(0)).map_err(unavailable)?;
    if !valid {
        return Err(unavailable("invalid resumable catalog state"));
    }
    let mut statement = connection
        .prepare("SELECT * FROM resumable_sessions ORDER BY id LIMIT 129")
        .map_err(unavailable)?;
    let mut rows = statement.query([]).map_err(unavailable)?;
    while let Some(row) = rows.next().map_err(unavailable)? {
        read_session(row).map_err(unavailable)?;
    }
    Ok(())
}

impl Catalog {
    pub async fn create_resumable_session(
        &self,
        bucket: &str,
        name: &str,
        options: ResumableOptions,
        total: Option<u64>,
    ) -> Result<ResumableSession, ResumableError> {
        self.check_upload(bucket, name, options.conditions, &options.access)
            .await?;
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let catalog = self.clone();
        let session = self.run(move |connection| Ok((|| -> Result<_,ResumableError> {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
            let (_,allow_overwrite)=super::objects::authorized_bucket(&transaction,&bucket,&options.access)?;
            let previous=super::objects::lookup(&transaction,&bucket,&name)?;
            super::objects::check_publication(previous.as_ref(),options.conditions,allow_overwrite)?;
            let count: i64 = transaction.query_row("SELECT count(*) FROM resumable_sessions", [], |r| r.get(0)).map_err(unavailable)?;
            if count >= MAX_SESSIONS as i64 { return Err(unavailable("retained upload session limit reached").into()); }
            let incarnation: String = transaction.query_row("SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1", [&bucket], |r| r.get(0)).optional().map_err(unavailable)?.ok_or(ObjectError::BucketNotFound)?;
            let id = random_id::<32>()?;
            let blob_id = random_id::<16>()?;
            let now = unix_millis(catalog.0.clock.now())?;
            let deadline = now.checked_add(SESSION_LIFETIME.as_millis() as i64).ok_or_else(|| unavailable("upload deadline overflow"))?;
            let encoded = encode_options(&options).to_string();
            let total = total.map(i64::try_from).transpose().map_err(unavailable)?;
            let path = catalog.0.data_dir.join("staging").join(&blob_id);
            let file = fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(unavailable)?;
            let synced=(|| -> std::io::Result<()> {file.sync_all()?;
            #[cfg(unix)]
            fs::File::open(catalog.0.data_dir.join("staging"))?.sync_all()?;
            Ok(())})();
            if let Err(error)=synced {drop(file);fs::remove_file(&path).map_err(unavailable)?;return Err(unavailable(error).into());}
            let inserted = transaction.execute("INSERT INTO resumable_sessions VALUES (?1,?2,?3,?4,?5,?6,?7,0,?8,?9,'ACTIVE',NULL)", params![id, blob_id, bucket, name, incarnation, encoded, total, now, deadline]);
            if let Err(error) = inserted { drop(file);fs::remove_file(path).map_err(unavailable)?; return Err(unavailable(error).into()); }
            // A failed commit can be ambiguous; retain its file for reference-aware recovery.
            transaction.commit().map_err(unavailable)?;
            connection.query_row("SELECT * FROM resumable_sessions WHERE id = ?1", [&id], read_session).map_err(|e|unavailable(e).into())
        })())).await??;
        self.0.maintenance_wake.notify_one();
        Ok(session)
    }

    pub async fn resumable_session(
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
            .run(move |connection| {
                connection
                    .query_row(
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
        if session.state == SessionState::Cancelled {
            return Err(ResumableError::Gone);
        }
        Ok(session)
    }

    pub async fn lock_resumable_session(
        &self,
        id: &str,
        bucket: &str,
    ) -> Result<SessionLease, ResumableError> {
        let guard = self.try_resumable_writer(id)?;
        let session = self.resumable_session(id, bucket).await?;
        Ok(SessionLease { session, guard })
    }

    pub(super) fn try_resumable_writer(&self, id: &str) -> Result<WriterGuard, CatalogError> {
        let mut writers = self.0.session_writers.lock().map_err(unavailable)?;
        if writers.len() >= MAX_SESSIONS || !writers.insert(id.to_owned()) {
            return Err(unavailable("upload session is busy"));
        }
        Ok(WriterGuard {
            catalog: self.clone(),
            id: id.to_owned(),
        })
    }

    /// Called only with an exclusive lease after file sync; it never transfers body bytes.
    pub async fn checkpoint_resumable_session(
        &self,
        lease: &mut SessionLease,
        offset: u64,
        total: Option<u64>,
    ) -> Result<(), ResumableError> {
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
        let changed = self.run(move |connection| connection.execute("UPDATE resumable_sessions SET offset = ?1, total = ?2 WHERE id = ?3 AND offset = ?4 AND state = 'ACTIVE' AND deadline > ?5", params![i64::try_from(offset).map_err(unavailable)?, total.map(i64::try_from).transpose().map_err(unavailable)?, id, i64::try_from(old).map_err(unavailable)?, now]).map_err(unavailable)).await?;
        if changed != 1 {
            self.resumable_session(&lease.session.id, &lease.session.bucket)
                .await?;
            return Err(ResumableError::Gone);
        }
        lease.session.offset = offset;
        lease.session.total = total;
        Ok(())
    }
}

fn encode_options(o: &ResumableOptions) -> Value {
    let m = &o.metadata;
    let access = match &o.access {
        BucketAccess::Unchecked => Value::Null,
        BucketAccess::Policy {
            identity,
            permission,
            project,
        } => {
            json!({"identity": identity.as_ref().map(Identity::upload_snapshot), "permission": permission, "project": project})
        }
    };
    json!({"version":2,"content_type":o.content_type,"metadata":{"replace":m.replace,"eventBasedHold":m.event_based_hold,"contentType":m.content_type.as_ref().map(|v|json!({"value":v})),"contentEncoding":m.content_encoding.as_ref().map(|v|json!({"value":v})),"contentDisposition":m.content_disposition.as_ref().map(|v|json!({"value":v})),"contentLanguage":m.content_language.as_ref().map(|v|json!({"value":v})),"cacheControl":m.cache_control.as_ref().map(|v|json!({"value":v})),"metadata":m.metadata.as_ref().map(|v|json!({"value":v}))},"md5":o.expected.md5.map(|v| v.to_vec()),"crc32c":o.expected.crc32c,"conditions":[o.conditions.if_generation_match,o.conditions.if_generation_not_match,o.conditions.if_metageneration_match,o.conditions.if_metageneration_not_match],"access":access,"origin":o.origin,"full":o.projection_full,"response_fields":o.response_fields})
}

fn decode_options(v: &Value) -> Result<ResumableOptions, String> {
    let invalid = || "invalid resumable options".to_owned();
    let field = |key: &str| -> Result<Option<Option<String>>, String> {
        if v["version"] == 2 {
            let value = &v["metadata"][key];
            if value.is_null() {
                return Ok(None);
            }
            return Ok(Some(
                serde_json::from_value(value.get("value").ok_or_else(invalid)?.clone())
                    .map_err(|_| invalid())?,
            ));
        }
        match &v["metadata"][key] {
            Value::Null => Ok(None),
            Value::String(s) => Ok(Some(Some(s.clone()))),
            _ => Err(invalid()),
        }
    };
    let conditions = v["conditions"]
        .as_array()
        .filter(|v| v.len() == 4)
        .ok_or_else(invalid)?;
    let mut c = [None; 4];
    for (i, value) in conditions.iter().enumerate() {
        c[i] = if value.is_null() {
            None
        } else {
            Some(value.as_i64().ok_or_else(invalid)?)
        };
    }
    let md5: Option<Vec<u8>> = serde_json::from_value(v["md5"].clone()).map_err(|_| invalid())?;
    let crc32c = serde_json::from_value(v["crc32c"].clone()).map_err(|_| invalid())?;
    let custom = if v["version"] == 2 {
        if v["metadata"]["metadata"].is_null() {
            None
        } else {
            Some(
                serde_json::from_value(
                    v["metadata"]["metadata"]
                        .get("value")
                        .ok_or_else(invalid)?
                        .clone(),
                )
                .map_err(|_| invalid())?,
            )
        }
    } else {
        serde_json::from_value(v["metadata"]["metadata"].clone()).map_err(|_| invalid())?
    };
    let access = if v["access"].is_null() {
        BucketAccess::Unchecked
    } else {
        let a = &v["access"];
        if a["permission"] != OBJECTS_CREATE {
            return Err(invalid());
        }
        BucketAccess::Policy {
            identity: if a["identity"].is_null() {
                None
            } else {
                Some(Identity::from_upload_snapshot(&a["identity"])?)
            },
            permission: OBJECTS_CREATE.into(),
            project: serde_json::from_value(a["project"].clone()).map_err(|_| invalid())?,
        }
    };
    Ok(ResumableOptions {
        content_type: v["content_type"].as_str().ok_or_else(invalid)?.into(),
        metadata: ObjectMetadataUpdate {
            replace: if v["version"] == 2 {
                v["metadata"]["replace"].as_bool().ok_or_else(invalid)?
            } else {
                false
            },
            content_type: field("contentType")?,
            content_encoding: field("contentEncoding")?,
            content_disposition: field("contentDisposition")?,
            content_language: field("contentLanguage")?,
            cache_control: field("cacheControl")?,
            metadata: custom,
            event_based_hold: serde_json::from_value(v["metadata"]["eventBasedHold"].clone())
                .map_err(|_| invalid())?,
        },
        expected: ExpectedHashes {
            md5: md5
                .map(|v| v.try_into().map_err(|_| invalid()))
                .transpose()?,
            crc32c,
        },
        conditions: ObjectConditions {
            if_generation_match: c[0],
            if_generation_not_match: c[1],
            if_metageneration_match: c[2],
            if_metageneration_not_match: c[3],
        },
        access,
        origin: serde_json::from_value(v["origin"].clone()).map_err(|_| invalid())?,
        projection_full: v["full"].as_bool().ok_or_else(invalid)?,
        response_fields: serde_json::from_value(v["response_fields"].clone())
            .map_err(|_| invalid())?,
    })
}

pub(super) fn read_session(row: &Row<'_>) -> rusqlite::Result<ResumableSession> {
    let invalid = |message: String| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, message.into())
    };
    let id: String = row.get(0)?;
    let blob_id: String = row.get(1)?;
    if id.len() != 64
        || blob_id.len() != 32
        || !id
            .bytes()
            .chain(blob_id.bytes())
            .all(|b| b.is_ascii_hexdigit())
    {
        return Err(invalid("invalid upload opaque ID".into()));
    }
    let state = match row.get::<_, String>(10)?.as_str() {
        "ACTIVE" => SessionState::Active,
        "COMPLETE" => SessionState::Complete,
        "CANCELLED" => SessionState::Cancelled,
        _ => return Err(invalid("invalid upload state".into())),
    };
    let completion: Option<String> = row.get(11)?;
    Ok(ResumableSession {
        id,
        blob_id,
        bucket: row.get(2)?,
        name: row.get(3)?,
        incarnation: row.get(4)?,
        options: decode_options(
            &serde_json::from_str::<Value>(&row.get::<_, String>(5)?)
                .map_err(|e| invalid(e.to_string()))?,
        )
        .map_err(invalid)?,
        total: row
            .get::<_, Option<i64>>(6)?
            .map(|n| u64::try_from(n).map_err(|e| invalid(e.to_string())))
            .transpose()?,
        offset: u64::try_from(row.get::<_, i64>(7)?).map_err(|e| invalid(e.to_string()))?,
        created: system_time(row.get(8)?)?,
        deadline: system_time(row.get(9)?)?,
        state,
        completion: completion
            .map(|v| serde_json::from_str(&v).map_err(|e| invalid(e.to_string())))
            .transpose()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
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
