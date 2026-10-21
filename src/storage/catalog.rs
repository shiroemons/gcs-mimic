use super::lock;
use crate::bucket::{BucketRecord, CatalogError};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params, types::Type};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

const MAX_IN_FLIGHT: usize = 32;
const MAX_LIST_LIMIT: usize = 1001;
const COLUMNS: &str = "name, project, location, location_type, storage_class, labels,
                      metageneration, time_created, updated";

struct Inner {
    resources: Mutex<Option<Resources>>,
    admission: Arc<Semaphore>,
    execution: Arc<Semaphore>,
}

struct Resources {
    // Drop the connection before releasing directory ownership, including on cancelled calls.
    connection: Connection,
    lock: lock::DirectoryLock,
}

/// A single SQLite connection with at most 32 admitted operations and one blocking job.
/// Excess callers receive `Unavailable` immediately; admitted callers await execution.
#[derive(Clone)]
pub struct Catalog(Arc<Inner>);

impl Catalog {
    /// Acquires directory ownership and completes SQLite recovery before returning.
    pub async fn open(data_dir: impl AsRef<Path>) -> Result<Self, CatalogError> {
        let data_dir = data_dir.as_ref().to_owned();
        tokio::task::spawn_blocking(move || {
            let lock = lock::acquire(&data_dir)?;
            let mut connection =
                Connection::open(data_dir.join("catalog.sqlite3")).map_err(unavailable)?;
            connection
                .busy_timeout(Duration::from_secs(5))
                .map_err(unavailable)?;
            connection
                .execute_batch(
                    "PRAGMA cache_size = -2048;
                 PRAGMA mmap_size = 0;
                 PRAGMA temp_store = FILE;",
                )
                .map_err(unavailable)?;
            let version: i64 = connection
                .pragma_query_value(None, "user_version", |r| r.get(0))
                .map_err(unavailable)?;
            if !(0..=1).contains(&version) {
                return Err(CatalogError::Unavailable(format!(
                    "unsupported catalog schema version {version}; supported version is 1"
                )));
            }
            let check: String = connection
                .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
                .map_err(unavailable)?;
            if check != "ok" {
                return Err(CatalogError::Unavailable(format!(
                    "catalog quick_check failed: {check}"
                )));
            }
            if version == 0 {
                let tables: i64 = connection
                    .query_row(
                        "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(unavailable)?;
                if tables != 0 {
                    return Err(CatalogError::Unavailable(
                        "unversioned catalog is not empty".into(),
                    ));
                }
            } else {
                // Reject foreign schemas before any persistent journal-mode change.
                connection
                    .prepare(&format!("SELECT {COLUMNS} FROM buckets LIMIT 0"))
                    .map_err(unavailable)?;
            }
            let mode: String = connection
                .pragma_query_value(None, "journal_mode", |r| r.get(0))
                .map_err(unavailable)?;
            if mode != "wal" {
                let mode: String = connection
                    .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
                    .map_err(unavailable)?;
                if mode != "wal" {
                    return Err(CatalogError::Unavailable(
                        "SQLite WAL mode is required".into(),
                    ));
                }
            }
            connection
                .execute_batch("PRAGMA synchronous = FULL; PRAGMA wal_autocheckpoint = 256;")
                .map_err(unavailable)?;
            if version == 0 {
                let transaction = connection.transaction().map_err(unavailable)?;
                transaction
                    .execute_batch(
                        "CREATE TABLE buckets (
                        name TEXT PRIMARY KEY COLLATE BINARY,
                        project TEXT NOT NULL,
                        location TEXT NOT NULL,
                        location_type TEXT NOT NULL,
                        storage_class TEXT NOT NULL,
                        labels TEXT NOT NULL,
                        metageneration INTEGER NOT NULL,
                        time_created INTEGER NOT NULL,
                        updated INTEGER NOT NULL
                     ) WITHOUT ROWID;
                     CREATE INDEX buckets_project_name ON buckets(project, name);
                     PRAGMA user_version = 1;",
                    )
                    .map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
            }
            // A version marker alone must not make a missing or incompatible schema ready.
            connection
                .prepare(&format!("SELECT {COLUMNS} FROM buckets LIMIT 0"))
                .map_err(unavailable)?;
            Ok(Self(Arc::new(Inner {
                resources: Mutex::new(Some(Resources { connection, lock })),
                admission: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
                execution: Arc::new(Semaphore::new(1)),
            })))
        })
        .await
        .map_err(unavailable)?
    }

    pub fn is_healthy(&self) -> bool {
        !self.0.resources.is_poisoned() && !self.0.admission.is_closed()
    }

    /// Stops admission on all handles and waits for blocking work before releasing ownership.
    /// Remaining handles become unavailable; repeated closes succeed without touching a new owner.
    pub async fn close(&self) -> Result<(), CatalogError> {
        self.0.admission.close();
        let execution = self
            .0
            .execution
            .clone()
            .acquire_owned()
            .await
            .map_err(unavailable)?;
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let _execution = execution;
            // Poison must not prevent shutdown from closing the connection and its lock.
            let mut resources = inner
                .resources
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(Resources { connection, lock }) = resources.take() {
                connection
                    .close()
                    .map_err(|(_, error)| unavailable(error))?;
                lock.release().map_err(unavailable)?;
            }
            Ok(())
        })
        .await
        .map_err(unavailable)?
    }

    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, CatalogError> + Send + 'static,
    ) -> Result<T, CatalogError> {
        let admission = self
            .0
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| CatalogError::Unavailable("catalog operation limit reached".into()))?;
        let execution = self
            .0
            .execution
            .clone()
            .acquire_owned()
            .await
            .map_err(unavailable)?;
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let (_admission, _execution) = (admission, execution);
            let result = match inner.resources.lock() {
                Ok(mut resources) => match resources.as_mut() {
                    Some(resources) => operation(&mut resources.connection),
                    None => Err(CatalogError::Unavailable("catalog is closed".into())),
                },
                Err(error) => Err(unavailable(error)),
            };
            // Release ownership before Tokio publishes completion to an immediate reopen.
            drop(inner);
            result
        })
        .await
        .map_err(unavailable)?
    }

    pub async fn insert_bucket(
        &self,
        mut record: BucketRecord,
    ) -> Result<BucketRecord, CatalogError> {
        self.run(move |connection| {
            let created = unix_millis(record.time_created)?;
            let updated = unix_millis(record.updated)?;
            record.time_created = system_time(created).map_err(unavailable)?;
            record.updated = system_time(updated).map_err(unavailable)?;
            let labels = serde_json::to_string(&record.labels).map_err(unavailable)?;
            let changed = connection
                .execute(
                    "INSERT INTO buckets VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(name) DO NOTHING",
                    params![
                        record.name,
                        record.project,
                        record.location,
                        record.location_type,
                        record.storage_class,
                        labels,
                        record.metageneration,
                        created,
                        updated
                    ],
                )
                .map_err(unavailable)?;
            if changed == 0 {
                return Err(CatalogError::AlreadyExists);
            }
            Ok(record)
        })
        .await
    }

    pub async fn get_bucket(&self, name: &str) -> Result<Option<BucketRecord>, CatalogError> {
        let name = name.to_owned();
        self.run(move |connection| {
            connection
                .query_row(
                    &format!("SELECT {COLUMNS} FROM buckets WHERE name = ?1"),
                    [name],
                    read_bucket,
                )
                .optional()
                .map_err(unavailable)
        })
        .await
    }

    /// Returns at most 1001 rows, allowing a protocol page of 1000 plus one lookahead row.
    pub async fn list_buckets(
        &self,
        project: &str,
        prefix: Option<&str>,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<BucketRecord>, CatalogError> {
        if limit > MAX_LIST_LIMIT {
            return Err(CatalogError::Unavailable(format!(
                "list limit exceeds {MAX_LIST_LIMIT}"
            )));
        }
        let project = project.to_owned();
        let prefix = prefix.unwrap_or("").to_owned();
        let start_after = start_after.map(str::to_owned);
        self.run(move |connection| {
            let mut upper = prefix.as_bytes().to_vec();
            let upper = upper
                .iter()
                .rposition(|byte| *byte != u8::MAX)
                .map(|index| {
                    upper[index] += 1;
                    upper.truncate(index + 1);
                    upper
                });
            // Both bounds remain indexable, including when a page starts beyond the prefix.
            let (lower, comparison) = match start_after {
                Some(start) if start >= prefix => (start, ">"),
                _ => (prefix, ">="),
            };
            let upper_clause = if upper.is_some() {
                "AND name < CAST(?4 AS TEXT)"
            } else {
                ""
            };
            let sql = format!(
                "SELECT {COLUMNS} FROM buckets
                WHERE project = ?1 AND name {comparison} ?2 {upper_clause} ORDER BY name LIMIT ?3"
            );
            let mut statement = connection.prepare(&sql).map_err(unavailable)?;
            let mut rows = match upper {
                Some(upper) => statement.query(params![project, lower, limit as i64, upper]),
                None => statement.query(params![project, lower, limit as i64]),
            }
            .map_err(unavailable)?;
            let mut records = Vec::new();
            while let Some(row) = rows.next().map_err(unavailable)? {
                records.push(read_bucket(row).map_err(unavailable)?);
            }
            Ok(records)
        })
        .await
    }

    pub async fn delete_bucket(
        &self,
        name: &str,
        if_metageneration_match: Option<i64>,
        if_metageneration_not_match: Option<i64>,
    ) -> Result<(), CatalogError> {
        let name = name.to_owned();
        self.run(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(unavailable)?;
            let metageneration: i64 = transaction
                .query_row(
                    "SELECT metageneration FROM buckets WHERE name = ?1",
                    [&name],
                    |r| r.get(0),
                )
                .optional()
                .map_err(unavailable)?
                .ok_or(CatalogError::NotFound)?;
            if if_metageneration_match.is_some_and(|value| value != metageneration)
                || if_metageneration_not_match == Some(metageneration)
            {
                return Err(CatalogError::PreconditionFailed);
            }
            // The objects slice must check bucket emptiness inside this same transaction.
            transaction
                .execute("DELETE FROM buckets WHERE name = ?1", [name])
                .map_err(unavailable)?;
            transaction.commit().map_err(unavailable)
        })
        .await
    }
}

fn unavailable(error: impl std::fmt::Display) -> CatalogError {
    CatalogError::Unavailable(error.to_string())
}

fn unix_millis(time: SystemTime) -> Result<i64, CatalogError> {
    let nanos = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as i128,
        Err(error) => -(error.duration().as_nanos() as i128),
    };
    i64::try_from(nanos.div_euclid(1_000_000)).map_err(unavailable)
}

fn system_time(millis: i64) -> rusqlite::Result<SystemTime> {
    let duration = Duration::from_millis(millis.unsigned_abs());
    let time = if millis < 0 {
        UNIX_EPOCH.checked_sub(duration)
    } else {
        UNIX_EPOCH.checked_add(duration)
    };
    time.ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            Type::Integer,
            "timestamp outside SystemTime range".into(),
        )
    })
}

fn read_bucket(row: &Row<'_>) -> rusqlite::Result<BucketRecord> {
    let labels: String = row.get(5)?;
    Ok(BucketRecord {
        name: row.get(0)?,
        project: row.get(1)?,
        location: row.get(2)?,
        location_type: row.get(3)?,
        storage_class: row.get(4)?,
        labels: serde_json::from_str(&labels)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(5, Type::Text, Box::new(e)))?,
        metageneration: row.get(6)?,
        time_created: system_time(row.get(7)?)?,
        updated: system_time(row.get(8)?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn close_waits_for_cancelled_blocking_work_and_invalidates_clones() {
        use std::{future::Future, task::Poll};

        let dir =
            std::env::temp_dir().join(format!("gcs-mimic-catalog-close-{}", std::process::id()));
        let catalog = Catalog::open(&dir).await.unwrap();
        let worker = catalog.clone();
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, finish) = std::sync::mpsc::channel();
        let task = tokio::spawn(async move {
            worker
                .run(move |_| {
                    started.send(()).unwrap();
                    finish.recv().unwrap();
                    Ok(())
                })
                .await
        });
        running.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        {
            let mut close = std::pin::pin!(catalog.close());
            assert!(
                std::future::poll_fn(|cx| Poll::Ready(close.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            assert!(!catalog.is_healthy());
            assert!(matches!(
                catalog.get_bucket("missing").await,
                Err(CatalogError::Unavailable(_))
            ));
            assert!(Catalog::open(&dir).await.is_err());
            release.send(()).unwrap();
            close.await.unwrap();
        }
        let reopened = Catalog::open(&dir).await.unwrap();
        catalog.close().await.unwrap();
        drop(catalog);
        assert!(Catalog::open(&dir).await.is_err());
        reopened.close().await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn poisoned_catalog_permanently_revokes_readiness() {
        use crate::{clock::SystemClock, server::State};
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir =
            std::env::temp_dir().join(format!("gcs-mimic-catalog-poison-{}", std::process::id()));
        let catalog = Catalog::open(&dir).await.unwrap();
        let state = State {
            ready: AtomicBool::new(true),
            clock: Arc::new(SystemClock),
            catalog: catalog.clone(),
            api_base_url: "http://127.0.0.1:4443".into(),
        };
        assert!(state.is_ready());
        assert!(
            catalog
                .run::<()>(|_| panic!("injected catalog panic"))
                .await
                .is_err()
        );
        assert!(!state.is_ready());
        assert!(!state.ready.load(Ordering::Acquire));
        assert!(matches!(
            catalog.get_bucket("abc").await,
            Err(CatalogError::Unavailable(_))
        ));
        assert!(!state.is_ready());
        catalog.close().await.unwrap();
        Catalog::open(&dir).await.unwrap().close().await.unwrap();
        drop(state);
        drop(catalog);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn timestamps_use_integer_unix_milliseconds() {
        assert_eq!(
            unix_millis(UNIX_EPOCH + Duration::from_micros(1999)).unwrap(),
            1
        );
        assert_eq!(
            unix_millis(UNIX_EPOCH - Duration::from_micros(1)).unwrap(),
            -1
        );
        for millis in [-1001, -1, 0, 1, 1001] {
            assert_eq!(unix_millis(system_time(millis).unwrap()).unwrap(), millis);
        }
    }

    #[tokio::test]
    async fn admission_is_bounded_and_sqlite_settings_are_explicit() {
        let dir =
            std::env::temp_dir().join(format!("gcs-mimic-catalog-settings-{}", std::process::id()));
        let catalog = Catalog::open(&dir).await.unwrap();
        let permits = catalog
            .0
            .admission
            .clone()
            .try_acquire_many_owned(MAX_IN_FLIGHT as u32)
            .unwrap();
        assert!(matches!(
            catalog.get_bucket("bucket").await,
            Err(CatalogError::Unavailable(_))
        ));
        drop(permits);
        catalog
            .run(|connection| {
                for (pragma, expected) in [
                    ("synchronous", 2),
                    ("cache_size", -2048),
                    ("mmap_size", 0),
                    ("busy_timeout", 5000),
                    ("user_version", 1),
                ] {
                    assert_eq!(
                        connection
                            .pragma_query_value(None, pragma, |row| row.get::<_, i64>(0))
                            .map_err(unavailable)?,
                        expected,
                        "{pragma}"
                    );
                }
                assert_eq!(
                    connection
                        .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
                        .map_err(unavailable)?,
                    "wal"
                );
                Ok(())
            })
            .await
            .unwrap();
        drop(catalog);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
