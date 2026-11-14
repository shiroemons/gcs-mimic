use super::{
    BlobBackend, Catalog, CatalogError, ResumableError, SessionId, SessionState, UnixMicros,
    UnixMillis,
    resumable::writers::{ClaimError, KeyOrigin, WriterKey},
    unavailable,
};
use crate::storage::blob_id::BlobId;
use crate::storage::catalog::sql;
use crate::storage::failure::BackendCause;
use rusqlite::{Connection, params};
#[cfg(test)]
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const PAGE_SIZE: i64 = 16;
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Every sweep runs; the first failure fails the pass and later failures remain visible.
#[derive(Default)]
pub(super) struct SweepOutcome {
    next: Option<SystemTime>,
    failure: Option<CatalogError>,
}

impl SweepOutcome {
    pub(super) fn record(
        &mut self,
        sweep: &'static str,
        result: Result<Option<SystemTime>, CatalogError>,
    ) {
        match result {
            Ok(deadline) => self.next = self.next.into_iter().chain(deadline).min(),
            Err(error) => self.fail(sweep, error),
        }
    }

    pub(super) fn fail(&mut self, sweep: &'static str, error: CatalogError) {
        if self.failure.is_some() {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "resumable_maintenance_retry",
                serde_json::json!({"message": format!("{sweep} maintenance failed: {error}")}),
            );
        } else {
            self.failure = Some(error);
        }
    }

    pub(super) fn finish(self) -> Result<Option<SystemTime>, CatalogError> {
        self.failure.map_or(Ok(self.next), Err)
    }
}

fn cleanup_page(
    connection: &Connection,
    now: UnixMillis,
    after: &str,
) -> Result<Vec<(SessionId, BlobId, UnixMillis)>, CatalogError> {
    // Requests wake maintenance, so this runs per request and must not read every session. Both
    // arms start from what is due: the deadline index, and the journal (CROSS JOIN keeps SQLite
    // from scanning sessions in id order instead).
    connection.prepare_cached("SELECT id, blob_id, deadline FROM resumable_sessions INDEXED BY resumable_sessions_deadline WHERE deadline <= ?2 AND id > ?1 UNION ALL SELECT session.id, session.blob_id, session.deadline FROM pending_blob_deletions AS pending CROSS JOIN resumable_sessions AS session ON session.blob_id = pending.blob_id WHERE session.id > ?1 AND session.state = 'CANCELLED' AND session.deadline > ?2 ORDER BY id LIMIT ?3")?
        .query_map(params![after, now, PAGE_SIZE], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>().map_err(unavailable)
}

fn clean_record(
    connection: &Connection,
    backend: &BlobBackend,
    id: SessionId,
    blob_id: &BlobId,
    deadline: UnixMillis,
    now: UnixMillis,
) -> Result<(), CatalogError> {
    clean_session_storage(connection, backend, blob_id)?;
    if deadline <= now {
        sql::exec(
            connection,
            "DELETE FROM resumable_sessions WHERE id = ?1 AND deadline <= ?2",
            params![id, now],
        )?;
    }
    Ok(())
}

pub(super) fn recover_due(
    connection: &Connection,
    backend: &BlobBackend,
    now: UnixMillis,
) -> Result<(), CatalogError> {
    let mut after = String::new();
    loop {
        let rows = cleanup_page(connection, now, &after)?;
        if rows.is_empty() {
            return Ok(());
        }
        for (id, blob_id, deadline) in rows {
            clean_record(connection, backend, id, &blob_id, deadline, now)?;
            after = id.to_hex();
        }
    }
}

#[cfg(test)]
pub(super) fn clean_session_files(
    connection: &rusqlite::Connection,
    data_dir: &Path,
    blob_id: &BlobId,
) -> Result<(), CatalogError> {
    clean_session_storage(connection, &BlobBackend::Disk(data_dir.to_owned()), blob_id)
}

fn clean_session_storage(
    connection: &rusqlite::Connection,
    backend: &BlobBackend,
    blob_id: &BlobId,
) -> Result<(), CatalogError> {
    let cleanup = plan_session_cleanup(connection, blob_id)?;
    remove_session_files(backend, cleanup)?;
    forget_session_cleanup(connection, cleanup)
}

#[derive(Clone, Copy)]
struct SessionCleanup {
    blob_id: BlobId,
    remove_blob: bool,
}

fn plan_session_cleanup(
    connection: &Connection,
    blob_id: &BlobId,
) -> Result<SessionCleanup, CatalogError> {
    Ok(SessionCleanup {
        blob_id: *blob_id,
        remove_blob: !super::objects::blob_referenced(connection, blob_id)?,
    })
}

fn remove_session_files(backend: &BlobBackend, cleanup: SessionCleanup) -> std::io::Result<()> {
    backend.remove_draft_durable(&cleanup.blob_id)?;
    if cleanup.remove_blob {
        backend.remove_durable(&cleanup.blob_id)?;
    }
    Ok(())
}

fn forget_session_cleanup(
    connection: &Connection,
    cleanup: SessionCleanup,
) -> Result<(), CatalogError> {
    // A deletion after the plan owns its own journal row; a live plan must not forget it.
    if cleanup.remove_blob {
        sql::exec(
            connection,
            "DELETE FROM pending_blob_deletions WHERE blob_id = ?1 AND NOT EXISTS (SELECT 1 FROM objects WHERE blob_id = ?1)",
            [cleanup.blob_id],
        )?;
    }
    Ok(())
}

impl Catalog {
    /// The last guard owner releases the writer only after filesystem work and forgetting settle.
    pub(super) async fn clean_session_storage(
        &self,
        writer: &Arc<super::resumable::WriterGuard>,
        blob_id: BlobId,
    ) -> Result<(), CatalogError> {
        let permit = self.admit_object_upload().map_err(unavailable)?;
        let (catalog, writer) = (self.clone(), writer.clone());
        crate::task::spawn_supervised("session-cleanup", async move {
            let (_writer, _permit) = (writer, permit);
            let cleanup = catalog
                .run_upload(move |connection| {
                    plan_session_cleanup(connection, &blob_id)
                        .map_err(crate::storage::failure::object_unavailable)
                })
                .await
                .map_err(unavailable)?;
            let backend = catalog.0.backend.clone();
            crate::task::run_blocking("session-cleanup", move || {
                remove_session_files(&backend, cleanup)
            })
            .await
            .map_err(unavailable)?
            .map_err(unavailable)?;
            catalog
                .run_upload(move |connection| {
                    forget_session_cleanup(connection, cleanup)
                        .map_err(crate::storage::failure::object_unavailable)
                })
                .await
                .map_err(unavailable)
        })
        .await
        .map_err(unavailable)?
    }

    pub(crate) async fn cancel_resumable_session(
        &self,
        id: &str,
        bucket: &str,
    ) -> Result<(), ResumableError> {
        let lease = self.lock_resumable_session(id, bucket).await?;
        if lease.session.state != SessionState::Active {
            return Err(ResumableError::Gone);
        }
        let id = lease.session.id;
        let now = UnixMillis::try_from(self.0.clock.now())?;
        let blob_id = lease.session.blob_id;
        let changed = self.run::<_, CatalogError>(move |connection| {
            let transaction = sql::immediate(connection)?;
            let changed = sql::exec(&transaction, "UPDATE resumable_sessions SET state = 'CANCELLED' WHERE id = ?1 AND state = 'ACTIVE' AND deadline > ?2", params![id, now])?;
            if changed == 1 {
                sql::exec(&transaction, "INSERT OR IGNORE INTO pending_blob_deletions VALUES (?1)", [blob_id])?;
            }
            transaction.commit()?;
            Ok(changed)
        }).await?;
        if changed != 1 {
            self.resumable_session(&lease.session.id.to_string(), &lease.session.bucket)
                .await?;
            return Err(ResumableError::Gone);
        }
        // Remove the cleanup journal only after file removal and directory sync succeed.
        let blob_id = lease.session.blob_id;
        let result = self.clean_session_storage(&lease.guard, blob_id).await;
        if let Err(error) = result {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "upload_cleanup_deferred",
                serde_json::json!({"message":format!("cancelled upload cleanup failed; maintenance will retry: {error}")}),
            );
            self.record_maintenance_error(Some(BackendCause::from_error(error)));
        }
        drop(lease);
        self.0.maintenance_wake.notify_one();
        Ok(())
    }

    /// Runs bounded cleanup against the injected clock; failed records remain the retry journal.
    #[cfg(test)]
    pub(crate) async fn maintain_resumable_sessions(
        &self,
    ) -> Result<Option<SystemTime>, CatalogError> {
        let result = self.run_resumable_maintenance().await;
        self.0.maintenance_wake.notify_one();
        result
    }

    async fn run_resumable_maintenance(&self) -> Result<Option<SystemTime>, CatalogError> {
        let result = self.maintain_resumable_inner().await;
        self.record_maintenance_error(
            result
                .as_ref()
                .err()
                .map(|error| BackendCause::new(error.to_string())),
        );
        result
    }

    pub(super) fn record_maintenance_error(&self, cause: Option<BackendCause>) {
        *self
            .0
            .maintenance_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = cause;
    }

    #[cfg(test)]
    pub(crate) fn resumable_maintenance_error(&self) -> Option<String> {
        self.0
            .maintenance_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .map(|cause| cause.message().to_owned())
    }

    async fn maintain_resumable_inner(&self) -> Result<Option<SystemTime>, CatalogError> {
        let reading = self.0.clock.now();
        // Session deadlines are stored in milliseconds, hard delete times in microseconds. Each
        // sweep gets its own unit from one reading: truncating to milliseconds would leave a row
        // due within the current millisecond unpurged and spin the scheduler until the next one.
        let now = UnixMillis::try_from(reading)?;
        let hard_delete_now = UnixMicros::try_from(reading)?;
        let mut sweep = SweepOutcome::default();
        sweep.record(
            "derived transfer",
            self.maintain_derived_transfers_at(now).await,
        );
        sweep.record(
            "resumable session",
            self.sweep_resumable_sessions(now).await,
        );
        sweep.record("rewrite session", self.maintain_rewrite_sessions(now).await);
        sweep.record(
            "lifecycle",
            self.run_lifecycle(super::lifecycle::Pass::Scheduled)
                .await
                .map(|outcome| outcome.next()),
        );
        sweep.record("XML multipart", self.maintain_xml_multipart(now).await);
        sweep.record(
            "soft-deleted bucket",
            self.maintain_soft_deleted_buckets(hard_delete_now).await,
        );
        sweep.record(
            "soft-deleted object",
            self.maintain_soft_deleted_objects(hard_delete_now).await,
        );
        // Last, so one pass also removes the blobs of every row the sweeps above journaled.
        sweep.record("deletion journal", self.maintain_deletion_journal().await);
        sweep.finish()
    }

    async fn sweep_resumable_sessions(
        &self,
        now: UnixMillis,
    ) -> Result<Option<SystemTime>, CatalogError> {
        let mut after = String::new();
        let mut sweep = SweepOutcome::default();
        loop {
            let cursor = after.clone();
            let rows = match self
                .run(move |connection| cleanup_page(connection, now, &cursor))
                .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    sweep.fail("resumable page", error);
                    break;
                }
            };
            let Some(last) = rows.last() else {
                break;
            };
            after = last.0.to_hex();
            for (id, blob_id, deadline) in rows {
                // A held session is swept once its request releases it, so this never waits.
                let writer = match self.claim_writer(WriterKey::Resumable(id), KeyOrigin::Catalog) {
                    Ok(writer) => Arc::new(writer),
                    Err(ClaimError::Busy) => continue,
                    Err(error @ (ClaimError::Poisoned | ClaimError::Catalog(_))) => {
                        sweep.fail("resumable session", error.into());
                        continue;
                    }
                };
                let result = async {
                    self.clean_session_storage(&writer, blob_id).await?;
                    if deadline <= now {
                        self.run::<_, CatalogError>(move |connection| {
                            sql::exec(
                                connection,
                                "DELETE FROM resumable_sessions WHERE id = ?1 AND deadline <= ?2",
                                params![id, now],
                            )?;
                            Ok(())
                        })
                        .await?;
                    }
                    Ok::<_, CatalogError>(())
                }
                .await;
                if let Err(error) = result {
                    sweep.fail("resumable session", error);
                }
            }
        }
        let deadline = self
            .run(move |connection| {
                let deadline: Option<UnixMillis> = sql::scalar(
                    connection,
                    "SELECT min(deadline) FROM resumable_sessions WHERE deadline > ?1",
                    [now],
                )?;
                deadline
                    .map(UnixMillis::system_time)
                    .transpose()
                    .map_err(unavailable)
            })
            .await;
        sweep.record("resumable deadline", deadline);
        sweep.finish()
    }

    /// Starts one scheduler. Its owner must call `close` before releasing directory ownership.
    pub fn start_resumable_maintenance(&self) {
        if self
            .0
            .maintenance_started
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let weak = std::sync::Arc::downgrade(&self.0);
        let wake = self.0.maintenance_wake.clone();
        // A panic must not end expiry for the rest of the process: restart the loop and report
        // the gap through readiness. The supervisor holds only a weak reference, like the loop.
        crate::task::spawn_restarting(
            "resumable-maintenance",
            self.0.maintenance_health.clone(),
            move || {
                let inner = weak.upgrade()?;
                (!inner.admission.is_closed()).then(|| maintenance_loop(weak.clone(), wake.clone()))
            },
        );
    }

    /// False while a background loop is down: restarting after a panic until a pass succeeds,
    /// failing every pass, or stopped for good.
    pub(crate) fn background_tasks_running(&self) -> bool {
        !self.0.maintenance_health.is_down()
            && self
                .0
                .background_health
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .all(|health| !health.is_down())
    }

    /// Reports `health` through readiness for as long as the catalog lives, for a background
    /// worker the catalog does not start itself, such as the sync mirror.
    pub(crate) fn watch_background_task(&self, health: crate::task::TaskHealth) {
        self.0
            .background_health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(health);
    }

    /// Makes the next `count` maintenance passes panic before doing any work.
    #[cfg(test)]
    pub(crate) fn inject_maintenance_panics(&self, count: usize) {
        self.0
            .maintenance_panics
            .store(count, std::sync::atomic::Ordering::Release);
    }
}

async fn maintenance_loop(
    weak: std::sync::Weak<super::Inner>,
    wake: std::sync::Arc<tokio::sync::Notify>,
) {
    loop {
        let notified = wake.notified();
        tokio::pin!(notified);
        // Register before reading the next deadline so insertions cannot lose a wakeup.
        notified.as_mut().enable();
        let Some(inner) = weak.upgrade() else {
            return;
        };
        if inner.admission.is_closed() {
            return;
        }
        #[cfg(test)]
        if inner
            .maintenance_panics
            .try_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |count| count.checked_sub(1),
            )
            .is_ok()
        {
            panic!("injected resumable maintenance panic");
        }
        let catalog = Catalog(inner);
        let delay = match catalog.run_resumable_maintenance().await {
            Ok(next) => {
                catalog.0.maintenance_health.pass_succeeded();
                next.map(|deadline| {
                    deadline
                        .duration_since(catalog.0.clock.now())
                        .unwrap_or_default()
                })
            }
            Err(error) => {
                // Expiry that fails on every pass leaves sessions and staged bytes behind, so
                // persistent failure turns readiness off until a pass succeeds again.
                catalog.0.maintenance_health.pass_failed();
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Warn,
                    "resumable_maintenance_retry",
                    serde_json::json!({"message":format!("resumable maintenance failed; retrying: {error}")}),
                );
                Some(RETRY_DELAY)
            }
        };
        drop(catalog);
        match delay {
            Some(delay) => {
                tokio::select! { _ = tokio::time::sleep(delay) => {}, _ = &mut notified => {} }
            }
            None => notified.await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn sweep_outcome_keeps_the_earliest_deadline_and_first_failure() {
        let mut sweep = SweepOutcome::default();
        let early = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        sweep.record("later", Ok(Some(early + Duration::from_secs(10))));
        sweep.record("early", Ok(Some(early)));
        assert_eq!(sweep.finish().unwrap(), Some(early));
        let mut sweep = SweepOutcome::default();
        sweep.fail(
            "first",
            unavailable(std::io::Error::from(std::io::ErrorKind::StorageFull)),
        );
        sweep.fail("later", unavailable("later failure"));
        assert!(
            matches!(sweep.finish().unwrap_err(), CatalogError::Io(cause) if cause.kind() == std::io::ErrorKind::StorageFull)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn maintenance_runs_later_sweeps_after_a_session_cleanup_failure() {
        use crate::{
            clock::{Clock, ManualClock},
            storage::test_fixtures::bucket,
            test_support::TestDir,
        };
        let data = TestDir::new();
        let clock = Arc::new(ManualClock::new(SystemTime::now()));
        let catalog = Catalog::open_with_clock(&data.0, clock.clone())
            .await
            .unwrap();
        catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        let session = catalog
            .create_resumable_session(
                "bucket",
                "object",
                super::super::ResumableOptions::new(crate::bucket::BucketAccess::Permissive),
                None,
            )
            .await
            .unwrap();
        let multipart = catalog
            .begin_xml_multipart(
                "bucket",
                "multipart",
                super::super::xml_multipart::Options {
                    content_type: "text/plain".into(),
                    metadata: Default::default(),
                },
                crate::bucket::BucketAccess::Permissive,
            )
            .await
            .unwrap();
        clock.set(session.deadline + Duration::from_secs(1));
        assert!(clock.now() > multipart.created + Duration::from_secs(604800));
        super::super::fault::arm(
            &data.0,
            super::super::fault::Step::SessionCleanupSync,
            0,
            super::super::fault::Fault::Io(std::io::ErrorKind::StorageFull),
        );
        assert!(
            matches!(catalog.maintain_resumable_sessions().await.unwrap_err(), CatalogError::Io(cause) if cause.kind() == std::io::ErrorKind::StorageFull)
        );
        assert!(catalog.resumable_maintenance_error().is_some());
        let connection = Connection::open(data.0.join("catalog.sqlite3")).unwrap();
        assert_eq!(
            sql::scalar::<i64>(
                &connection,
                "SELECT count(*) FROM xml_multipart_uploads",
                []
            )
            .unwrap(),
            0
        );
        assert_eq!(
            sql::scalar::<i64>(&connection, "SELECT count(*) FROM resumable_sessions", []).unwrap(),
            1
        );
        catalog.maintain_resumable_sessions().await.unwrap();
        assert!(catalog.resumable_maintenance_error().is_none());
        assert_eq!(
            sql::scalar::<i64>(&connection, "SELECT count(*) FROM resumable_sessions", []).unwrap(),
            0
        );
        catalog.close().await.unwrap();
    }

    #[test]
    fn session_cleanup_never_forgets_a_later_object_deletion() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE objects(blob_id TEXT); CREATE TABLE pending_blob_deletions(blob_id TEXT PRIMARY KEY);").unwrap();
        let id: BlobId = "00000000000000000000000000000001".parse().unwrap();
        connection
            .execute("INSERT INTO objects VALUES (?1)", [id])
            .unwrap();
        let live_plan = plan_session_cleanup(&connection, &id).unwrap();
        connection.execute("DELETE FROM objects", []).unwrap();
        connection
            .execute("INSERT INTO pending_blob_deletions VALUES (?1)", [id])
            .unwrap();
        forget_session_cleanup(&connection, live_plan).unwrap();
        assert_eq!(
            sql::scalar::<i64>(
                &connection,
                "SELECT count(*) FROM pending_blob_deletions",
                []
            )
            .unwrap(),
            1
        );
        let absent_plan = plan_session_cleanup(&connection, &id).unwrap();
        connection
            .execute("INSERT INTO objects VALUES (?1)", [id])
            .unwrap();
        forget_session_cleanup(&connection, absent_plan).unwrap();
        assert_eq!(
            sql::scalar::<i64>(
                &connection,
                "SELECT count(*) FROM pending_blob_deletions",
                []
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn typed_session_cleanup_and_recovery_preserve_unrelated_files() {
        let dir = crate::test_support::TestDir::new();
        let data_dir = dir.0.join("data");
        fs::create_dir_all(data_dir.join("staging")).unwrap();
        let protected = dir.0.join("protected");
        fs::write(&protected, b"keep").unwrap();
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE objects(blob_id TEXT); CREATE TABLE pending_blob_deletions(blob_id TEXT);").unwrap();
        let id: BlobId = "0".repeat(32).parse().unwrap();
        let stage = id.staging_path(&data_dir);
        fs::write(&stage, b"old").unwrap();
        clean_session_files(&connection, &data_dir, &id).unwrap();
        assert!(!stage.exists());
        assert_eq!(fs::read(&protected).unwrap(), b"keep");
        fs::write(&stage, b"prefix-tail").unwrap();
        super::super::resumable_recovery::recover_prefix(&connection, &data_dir, &id, 6).unwrap();
        assert_eq!(fs::read(&stage).unwrap(), b"prefix");
        assert_eq!(fs::read(&protected).unwrap(), b"keep");
    }
}
