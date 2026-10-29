//! Durable project HMAC key metadata for `projects.hmacKeys`.
//!
//! Keys are project-scoped catalog rows, independent of buckets. Deleted keys stay listed in
//! the `DELETED` state, as the JSON API reports them through `showDeletedKeys`, but their
//! secret is erased in the same transaction so that a later signature verifier cannot accept
//! it. Each key carries its own revision; the etag encodes it, and conditional updates compare
//! it inside the mutating transaction.
use super::{Catalog, CatalogError, system_time_millis, unavailable, unix_millis};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use std::time::SystemTime;

pub(in crate::storage) const SCHEMA: &str = "CREATE TABLE hmac_keys (
    access_id TEXT PRIMARY KEY COLLATE BINARY,
    project TEXT NOT NULL,
    service_account_email TEXT NOT NULL,
    secret TEXT CHECK((state = 'DELETED') = (secret IS NULL)),
    state TEXT NOT NULL CHECK(state IN ('ACTIVE', 'INACTIVE', 'DELETED')),
    time_created INTEGER NOT NULL,
    updated INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0)
) WITHOUT ROWID;
CREATE INDEX hmac_keys_project ON hmac_keys(project, access_id);
CREATE INDEX hmac_keys_service_account ON hmac_keys(project, service_account_email, state);
PRAGMA user_version = 32;";

/// Cloud Storage allows at most this many non-deleted keys per service account. The local
/// catalog counts them per project as well: the emulator does not know which project owns
/// a service account, so a global count would let one project exhaust another's quota.
pub const MAX_KEYS_PER_SERVICE_ACCOUNT: i64 = 10;

/// Service-account access IDs are 61 characters: the `GOOG1E` prefix and 55 characters.
const ACCESS_ID_PREFIX: &str = "GOOG1E";
const ACCESS_ID_RANDOM_CHARS: usize = 55;
/// 30 random bytes encode to the documented 40-character base64 secret without padding.
const SECRET_BYTES: usize = 30;

pub(in crate::storage) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HmacKeyState {
    Active,
    Inactive,
    Deleted,
}

impl HmacKeyState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
            Self::Inactive => "INACTIVE",
            Self::Deleted => "DELETED",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ACTIVE" => Some(Self::Active),
            "INACTIVE" => Some(Self::Inactive),
            "DELETED" => Some(Self::Deleted),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HmacKeyRecord {
    pub access_id: String,
    pub project: String,
    pub service_account_email: String,
    pub state: HmacKeyState,
    pub time_created: SystemTime,
    pub updated: SystemTime,
    pub revision: i64,
}

impl HmacKeyRecord {
    pub fn etag(&self) -> String {
        crate::json::policy_etag(self.revision)
    }
}

/// A newly created key; the secret is returned only from creation.
#[derive(Debug)]
pub struct CreatedHmacKey {
    pub record: HmacKeyRecord,
    pub secret: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HmacKeyListQuery {
    pub project: String,
    pub service_account_email: Option<String>,
    pub show_deleted: bool,
    pub after: Option<String>,
    pub limit: usize,
}

pub struct HmacKeyPage {
    pub items: Vec<HmacKeyRecord>,
    pub has_more: bool,
}

#[derive(Debug)]
pub enum HmacKeyError {
    NotFound,
    PreconditionFailed,
    /// The key is ACTIVE; it must be deactivated before deletion.
    DeleteActive,
    /// A DELETED key cannot be deleted again or change state.
    Deleted,
    LimitReached,
    Catalog(CatalogError),
}

impl From<CatalogError> for HmacKeyError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

const COLUMNS: &str =
    "access_id, project, service_account_email, state, time_created, updated, revision";

fn record(row: &Row<'_>) -> rusqlite::Result<HmacKeyRecord> {
    let state: String = row.get(3)?;
    Ok(HmacKeyRecord {
        access_id: row.get(0)?,
        project: row.get(1)?,
        service_account_email: row.get(2)?,
        state: HmacKeyState::parse(&state).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                "invalid HMAC key state".into(),
            )
        })?,
        time_created: system_time_millis(row.get(4)?)?,
        updated: system_time_millis(row.get(5)?)?,
        revision: row.get(6)?,
    })
}

fn select(
    connection: &Connection,
    project: &str,
    access_id: &str,
) -> Result<Option<HmacKeyRecord>, CatalogError> {
    connection
        .query_row(
            &format!("SELECT {COLUMNS} FROM hmac_keys WHERE access_id = ?1 AND project = ?2"),
            params![access_id, project],
            record,
        )
        .optional()
        .map_err(unavailable)
}

fn random_bytes<const N: usize>() -> Result<[u8; N], CatalogError> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(unavailable)?;
    Ok(bytes)
}

fn generate_access_id() -> Result<String, CatalogError> {
    // 256 is a multiple of 32, so masking each byte draws base32 characters without bias.
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let bytes = random_bytes::<ACCESS_ID_RANDOM_CHARS>()?;
    let mut access_id = String::with_capacity(ACCESS_ID_PREFIX.len() + ACCESS_ID_RANDOM_CHARS);
    access_id.push_str(ACCESS_ID_PREFIX);
    access_id.extend(
        bytes
            .iter()
            .map(|byte| char::from(ALPHABET[usize::from(byte & 31)])),
    );
    Ok(access_id)
}

fn generate_secret() -> Result<String, CatalogError> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = random_bytes::<SECRET_BYTES>()?;
    let mut secret = String::with_capacity(SECRET_BYTES / 3 * 4);
    for chunk in bytes.as_chunks::<3>().0 {
        let value = u32::from(chunk[0]) << 16 | u32::from(chunk[1]) << 8 | u32::from(chunk[2]);
        for shift in [18, 12, 6, 0] {
            secret.push(char::from(ALPHABET[(value >> shift & 63) as usize]));
        }
    }
    Ok(secret)
}

impl Catalog {
    pub(crate) async fn create_hmac_key(
        &self,
        project: &str,
        service_account_email: &str,
    ) -> Result<CreatedHmacKey, HmacKeyError> {
        let project = project.to_owned();
        let email = service_account_email.to_owned();
        let now = unix_millis(self.0.clock.now())?;
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            let live: i64 = transaction
                .query_row(
                    "SELECT count(*) FROM hmac_keys WHERE project = ?1 AND service_account_email = ?2 AND state != 'DELETED'",
                    [&project, &email],
                    |row| row.get(0),
                )
                .map_err(unavailable)?;
            if live >= MAX_KEYS_PER_SERVICE_ACCOUNT {
                return Ok(Err(HmacKeyError::LimitReached));
            }
            // A collision among 275 random bits is not expected; retrying keeps IDs unique anyway.
            let mut access_id = generate_access_id()?;
            while transaction
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM hmac_keys WHERE access_id = ?1)",
                    [&access_id],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(unavailable)?
            {
                access_id = generate_access_id()?;
            }
            let secret = generate_secret()?;
            transaction
                .execute(
                    "INSERT INTO hmac_keys (access_id, project, service_account_email, secret, state, time_created, updated, revision)
                     VALUES (?1, ?2, ?3, ?4, 'ACTIVE', ?5, ?5, 1)",
                    params![access_id, project, email, secret, now],
                )
                .map_err(unavailable)?;
            let record = select(&transaction, &project, &access_id)?
                .ok_or_else(|| unavailable("inserted HMAC key is missing"))?;
            transaction.commit().map_err(unavailable)?;
            Ok(Ok(CreatedHmacKey { record, secret }))
        })
        .await?
    }

    pub(crate) async fn get_hmac_key(
        &self,
        project: &str,
        access_id: &str,
    ) -> Result<HmacKeyRecord, HmacKeyError> {
        let project = project.to_owned();
        let access_id = access_id.to_owned();
        self.run(move |connection| select(connection, &project, &access_id))
            .await?
            .ok_or(HmacKeyError::NotFound)
    }

    pub(crate) async fn list_hmac_keys(
        &self,
        query: HmacKeyListQuery,
    ) -> Result<HmacKeyPage, HmacKeyError> {
        if !(1..=super::MAX_LIST_LIMIT - 1).contains(&query.limit) {
            return Err(CatalogError::InvalidMetadata("invalid HMAC key page size".into()).into());
        }
        Ok(self
            .run(move |connection| {
                let mut statement = connection
                    .prepare(&format!(
                        "SELECT {COLUMNS} FROM hmac_keys
                         WHERE project = ?1
                           AND (?2 IS NULL OR service_account_email = ?2)
                           AND (?3 OR state != 'DELETED')
                           AND (?4 IS NULL OR access_id > ?4)
                         ORDER BY access_id LIMIT ?5"
                    ))
                    .map_err(unavailable)?;
                let mut items = statement
                    .query_map(
                        params![
                            query.project,
                            query.service_account_email,
                            query.show_deleted,
                            query.after,
                            (query.limit + 1) as i64
                        ],
                        record,
                    )
                    .map_err(unavailable)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(unavailable)?;
                let has_more = items.len() > query.limit;
                items.truncate(query.limit);
                Ok(HmacKeyPage { items, has_more })
            })
            .await?)
    }

    /// Change the state of a non-deleted key to ACTIVE or INACTIVE, optionally conditioned on
    /// the etag the caller last observed.
    pub(crate) async fn update_hmac_key_state(
        &self,
        project: &str,
        access_id: &str,
        state: HmacKeyState,
        etag: Option<String>,
    ) -> Result<HmacKeyRecord, HmacKeyError> {
        if state == HmacKeyState::Deleted {
            return Err(CatalogError::InvalidMetadata(
                "state can be set only to ACTIVE or INACTIVE; delete the key to make it DELETED"
                    .into(),
            )
            .into());
        }
        let project = project.to_owned();
        let access_id = access_id.to_owned();
        let now = unix_millis(self.0.clock.now())?;
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            let Some(current) = select(&transaction, &project, &access_id)? else {
                return Ok(Err(HmacKeyError::NotFound));
            };
            if etag.is_some_and(|etag| etag != current.etag()) {
                return Ok(Err(HmacKeyError::PreconditionFailed));
            }
            if current.state == HmacKeyState::Deleted {
                return Ok(Err(HmacKeyError::Deleted));
            }
            transaction
                .execute(
                    "UPDATE hmac_keys SET state = ?1, updated = max(?2, time_created), revision = revision + 1 WHERE access_id = ?3",
                    params![state.as_str(), now, access_id],
                )
                .map_err(unavailable)?;
            let record = select(&transaction, &project, &access_id)?
                .ok_or_else(|| unavailable("updated HMAC key is missing"))?;
            transaction.commit().map_err(unavailable)?;
            Ok(Ok(record))
        })
        .await?
    }

    /// Mark an INACTIVE key DELETED and erase its secret.
    pub(crate) async fn delete_hmac_key(
        &self,
        project: &str,
        access_id: &str,
    ) -> Result<(), HmacKeyError> {
        let project = project.to_owned();
        let access_id = access_id.to_owned();
        let now = unix_millis(self.0.clock.now())?;
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            let Some(current) = select(&transaction, &project, &access_id)? else {
                return Ok(Err(HmacKeyError::NotFound));
            };
            match current.state {
                HmacKeyState::Active => return Ok(Err(HmacKeyError::DeleteActive)),
                HmacKeyState::Deleted => return Ok(Err(HmacKeyError::Deleted)),
                HmacKeyState::Inactive => {}
            }
            transaction
                .execute(
                    "UPDATE hmac_keys SET state = 'DELETED', secret = NULL, updated = max(?1, time_created), revision = revision + 1 WHERE access_id = ?2",
                    params![now, access_id],
                )
                .map_err(unavailable)?;
            transaction.commit().map_err(unavailable)?;
            Ok(Ok(()))
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    #[tokio::test]
    async fn previous_schema_migrates_and_keys_survive_reopen() {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog.close().await.unwrap();
        let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
        super::super::test_support::downgrade_hmac_key_schema(&connection);
        connection
            .execute_batch("PRAGMA user_version = 31;")
            .unwrap();
        drop(connection);
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let created = catalog
            .create_hmac_key("project", "sa@example.invalid")
            .await
            .unwrap();
        catalog.close().await.unwrap();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(
            catalog
                .get_hmac_key("project", &created.record.access_id)
                .await
                .unwrap(),
            created.record
        );
        catalog.close().await.unwrap();
        let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            35
        );
    }

    #[tokio::test]
    async fn concurrent_creation_never_exceeds_the_per_account_limit() {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let catalog = catalog.clone();
            tasks.spawn(async move {
                catalog
                    .create_hmac_key("project", "sa@example.invalid")
                    .await
            });
        }
        let (mut created, mut limited) = (0, 0);
        while let Some(result) = tasks.join_next().await {
            match result.unwrap() {
                Ok(_) => created += 1,
                Err(HmacKeyError::LimitReached) => limited += 1,
                Err(error) => panic!("{error:?}"),
            }
        }
        assert_eq!((created, limited), (10, 6));
        // Another project's keys for the same account do not consume this project's quota.
        catalog
            .create_hmac_key("other-project", "sa@example.invalid")
            .await
            .unwrap();
        // Concurrent conditional updates with the same etag admit exactly one writer.
        let page = catalog
            .list_hmac_keys(HmacKeyListQuery {
                project: "project".into(),
                service_account_email: None,
                show_deleted: false,
                after: None,
                limit: 1,
            })
            .await
            .unwrap();
        let key = page.items[0].clone();
        assert!(page.has_more);
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..4 {
            let (catalog, key) = (catalog.clone(), key.clone());
            tasks.spawn(async move {
                catalog
                    .update_hmac_key_state(
                        &key.project,
                        &key.access_id,
                        HmacKeyState::Inactive,
                        Some(key.etag()),
                    )
                    .await
            });
        }
        let mut won = 0;
        while let Some(result) = tasks.join_next().await {
            match result.unwrap() {
                Ok(record) => {
                    won += 1;
                    assert_eq!(record.revision, key.revision + 1);
                }
                Err(HmacKeyError::PreconditionFailed) => {}
                Err(error) => panic!("{error:?}"),
            }
        }
        assert_eq!(won, 1);
        catalog.close().await.unwrap();
    }

    #[test]
    fn generated_access_ids_and_secrets_follow_the_documented_shapes() {
        let access_id = generate_access_id().unwrap();
        assert_eq!(access_id.len(), 61);
        assert!(access_id.starts_with("GOOG1E"));
        assert!(
            access_id
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        );
        let secret = generate_secret().unwrap();
        assert_eq!(secret.len(), 40);
        assert!(
            secret
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
        );
        assert_ne!(generate_access_id().unwrap(), access_id);
        assert_ne!(generate_secret().unwrap(), secret);
    }
}
