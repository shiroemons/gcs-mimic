use super::{
    After, BlobBackend, Catalog, CatalogError, ResumableError, SessionId, SessionState, UnixMicros,
    UnixMillis,
    resumable::writers::{ClaimError, WriterKey},
    unavailable,
};
use crate::cause::BackendCause;
use crate::clock::{Clock, SystemClock};
use crate::storage::catalog::sql;
use crate::storage::{BlobError, BlobId};
use rusqlite::{Connection, params};
#[cfg(test)]
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

mod probe;
use probe::DueWork;

const PAGE_SIZE: i64 = 16;
const RETRY_DELAY: Duration = Duration::from_secs(1);

/// Each due sweep runs even if an earlier sweep failed.
pub(super) struct SweepOutcome<'a> {
    catalog: &'a Catalog,
    next: Option<SystemTime>,
    failure: Option<CatalogError>,
}

impl<'a> SweepOutcome<'a> {
    pub(super) fn new(catalog: &'a Catalog) -> Self {
        Self {
            catalog,
            next: None,
            failure: None,
        }
    }

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
        if let Some(previous) = &self.failure
            && !self.catalog.shutdown_interrupted(previous)
        {
            if self.catalog.shutdown_interrupted(&error) {
                return;
            }
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                crate::diagnostics::Event::ResumableMaintenanceRetry,
                serde_json::json!({"message": format!("{sweep} maintenance failed: {error}")}),
            );
        } else {
            self.failure = Some(error);
        }
    }

    async fn when_due(
        &mut self,
        sweep: &'static str,
        deadline: Option<SystemTime>,
        now: SystemTime,
        work: impl Future<Output = Result<Option<SystemTime>, CatalogError>>,
    ) -> bool {
        let due = deadline.is_some_and(|time| time <= now);
        if due {
            self.record(sweep, work.await);
        } else {
            self.record(sweep, Ok(deadline));
        }
        due
    }

    pub(super) fn finish(self) -> Result<Option<SystemTime>, CatalogError> {
        self.failure.map_or(Ok(self.next), Err)
    }
}

/// A session whose storage maintenance cleans: expired, or cancelled with its blob journaled.
struct DueSession {
    id: SessionId,
    blob_id: BlobId,
    deadline: UnixMillis,
}

fn cleanup_page(
    connection: &Connection,
    now: UnixMillis,
    after: &After<SessionId>,
) -> Result<Vec<DueSession>, CatalogError> {
    // Start from the deadline index and the journal to bound the work by the due sessions.
    // CROSS JOIN keeps SQLite from scanning sessions in id order.
    connection.prepare_cached("SELECT id, blob_id, deadline FROM resumable_sessions INDEXED BY resumable_sessions_deadline WHERE deadline <= ?2 AND id > ?1 UNION ALL SELECT session.id, session.blob_id, session.deadline FROM pending_blob_deletions AS pending CROSS JOIN resumable_sessions AS session ON session.blob_id = pending.blob_id WHERE session.id > ?1 AND session.state = 'CANCELLED' AND session.deadline > ?2 ORDER BY id LIMIT ?3")?
        .query_map(params![after, now, PAGE_SIZE], |row| Ok(DueSession { id: row.get(0)?, blob_id: row.get(1)?, deadline: row.get(2)? }))?
        .collect::<rusqlite::Result<_>>().map_err(unavailable)
}

/// Signal from the writer even if the request is cancelled or the commit result is ambiguous.
pub(super) fn commit_cancellation(
    transaction: sql::WriteTx<'_>,
    changed: usize,
    wake: &tokio::sync::Notify,
) -> rusqlite::Result<()> {
    let result = transaction.commit();
    if changed != 0 {
        wake.notify_one();
    }
    result
}

fn clean_record(
    connection: &mut Connection,
    backend: &BlobBackend,
    id: SessionId,
    blob_id: &BlobId,
    deadline: UnixMillis,
    now: UnixMillis,
) -> Result<(), CatalogError> {
    clean_session_storage(connection, backend, blob_id)?;
    if deadline <= now {
        sql::exec_on_writer(
            connection,
            "DELETE FROM resumable_sessions WHERE id = ?1 AND deadline <= ?2",
            params![id, now],
        )?;
    }
    Ok(())
}

pub(super) fn recover_due(
    connection: &mut Connection,
    backend: &BlobBackend,
    now: UnixMillis,
) -> Result<(), CatalogError> {
    let mut after = After::Start;
    loop {
        let rows = cleanup_page(connection, now, &after)?;
        if rows.is_empty() {
            return Ok(());
        }
        for due in rows {
            clean_record(connection, backend, due.id, &due.blob_id, due.deadline, now)?;
            after = After::Key(due.id);
        }
    }
}

#[cfg(test)]
pub(super) fn clean_session_files(
    connection: &mut Connection,
    data_dir: &Path,
    blob_id: &BlobId,
) -> Result<(), CatalogError> {
    clean_session_storage(connection, &BlobBackend::Disk(data_dir.to_owned()), blob_id)
}

fn clean_session_storage(
    connection: &mut Connection,
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

fn remove_session_files(backend: &BlobBackend, cleanup: SessionCleanup) -> Result<(), BlobError> {
    backend.remove_draft_durable(&cleanup.blob_id)?;
    if cleanup.remove_blob {
        backend.remove_durable(&cleanup.blob_id)?;
    }
    Ok(())
}

fn forget_session_cleanup(
    connection: &mut Connection,
    cleanup: SessionCleanup,
) -> Result<(), CatalogError> {
    // A deletion after the plan owns its own journal row; a live plan must not forget it.
    if cleanup.remove_blob {
        sql::exec_on_writer(
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
        #[cfg(test)]
        self.count_cleanup_task();
        crate::task::spawn_supervised("session-cleanup", async move {
            let (_writer, _permit) = (writer, permit);
            let cleanup = catalog
                .run_upload(move |connection| {
                    plan_session_cleanup(connection, &blob_id)
                        .map_err(crate::storage::failure::object_unavailable)
                })
                .await
                .map_err(unavailable)?;
            let backend = catalog.0.storage.backend.clone();
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
        let now = UnixMillis::try_from(self.0.policy.clock.now())?;
        let blob_id = lease.session.blob_id;
        let wake = self.0.maintenance.wake.clone();
        let changed = self.run::<_, CatalogError>(move |connection| {
            let transaction = sql::immediate(connection)?;
            let changed = sql::exec(&transaction, "UPDATE resumable_sessions SET state = 'CANCELLED' WHERE id = ?1 AND state = 'ACTIVE' AND deadline > ?2", params![id, now])?;
            if changed == 1 {
                super::objects::journal_deletion(&transaction, &blob_id)?;
            }
            commit_cancellation(transaction, changed, &wake)?;
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
                crate::diagnostics::Event::UploadCleanupDeferred,
                serde_json::json!({"message":format!("cancelled upload cleanup failed; maintenance will retry: {error}")}),
            );
            self.record_maintenance_error(Some(BackendCause::from_error(error)));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn count_cleanup_task(&self) {
        self.0
            .admin
            .cleanup_tasks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Runs bounded cleanup against the injected clock; failed records remain the retry journal.
    #[cfg(test)]
    pub(crate) async fn maintain_resumable_sessions(
        &self,
    ) -> Result<Option<SystemTime>, CatalogError> {
        let result = self.run_resumable_maintenance().await;
        if result.is_err() {
            // A manually triggered pass can discover work while the scheduler is asleep.
            self.0.maintenance.wake.notify_one();
        }
        result
    }

    async fn run_resumable_maintenance(&self) -> Result<Option<SystemTime>, CatalogError> {
        let result = self.maintain_resumable_inner().await;
        if result
            .as_ref()
            .is_err_and(|error| self.shutdown_interrupted(error))
        {
            return result;
        }
        self.record_maintenance_error(
            result
                .as_ref()
                .err()
                .map(|error| BackendCause::new(error.to_string())),
        );
        result
    }

    pub(super) fn record_maintenance_error(&self, cause: Option<BackendCause>) {
        *crate::sync::lock(&self.0.maintenance.error) = cause;
    }

    #[cfg(test)]
    pub(crate) fn resumable_maintenance_error(&self) -> Option<String> {
        crate::sync::lock(&self.0.maintenance.error)
            .as_ref()
            .map(|cause| cause.message().to_owned())
    }

    async fn maintain_resumable_inner(&self) -> Result<Option<SystemTime>, CatalogError> {
        let reading = self.0.policy.clock.now();
        // Session deadlines are stored in milliseconds, hard delete times in microseconds. Each
        // sweep gets its own unit from one reading: truncating to milliseconds would leave a row
        // due within the current millisecond unpurged and spin the scheduler until the next one.
        let now = UnixMillis::try_from(reading)?;
        let micros = UnixMicros::try_from(reading)?;
        let mut sweep = SweepOutcome::new(self);
        // If the probe cannot inspect one table, still let the other sweeps repair their work.
        let (mut due, probe_error) = match self.maintenance_probe(reading).await {
            Ok(due) => (due, None),
            Err(error) => (DueWork::all(reading), Some(error)),
        };
        let mut may_have_journaled = sweep
            .when_due(
                "derived transfer",
                due.derived,
                reading,
                self.maintain_derived_transfers_at(now),
            )
            .await;
        may_have_journaled |= sweep
            .when_due(
                "resumable session",
                due.resumable,
                reading,
                self.sweep_resumable_sessions(now),
            )
            .await;
        may_have_journaled |= sweep
            .when_due(
                "rewrite session",
                due.rewrite,
                reading,
                self.maintain_rewrite_sessions(now),
            )
            .await;

        // Earlier sweeps can mark lifecycle dirty. Its gate and quarantine belong to the writer.
        let lifecycle = self.0.maintenance.lifecycle.scheduled_at(reading);
        if lifecycle.is_some_and(|time| time <= reading) {
            use super::lifecycle::{LifecycleOutcome, Pass};
            let result = self.run_lifecycle(Pass::Scheduled).await;
            let refresh_deadlines = match &result {
                Ok(LifecycleOutcome::Executed(counts)) => {
                    may_have_journaled |= counts.journaled().any();
                    counts.aborted != 0 || counts.deleted != 0
                }
                Ok(LifecycleOutcome::Disabled) => false,
                Err(_) => {
                    may_have_journaled = true; // An earlier page may have committed.
                    true
                }
            };
            sweep.record("lifecycle", result.map(|outcome| outcome.next()));
            if refresh_deadlines {
                // Lifecycle can create a multipart journal or a new soft-delete deadline.
                match self.maintenance_probe(reading).await {
                    Ok(updated) => due = updated,
                    Err(error) => {
                        sweep.fail("lifecycle", error);
                        due = DueWork::all(reading);
                    }
                }
            }
        } else {
            sweep.record("lifecycle", Ok(lifecycle));
        }
        sweep
            .when_due(
                "XML multipart",
                due.multipart,
                reading,
                self.maintain_xml_multipart(now),
            )
            .await;
        if sweep
            .when_due(
                "soft-deleted bucket",
                due.buckets,
                reading,
                self.maintain_soft_deleted_buckets(micros),
            )
            .await
        {
            may_have_journaled = true;
            // Removing a retained bucket may also have removed the earliest object deadline.
            if due.objects.is_some() {
                match self.run_read(probe::object_expiry).await {
                    Ok(next) => due.objects = next,
                    Err(error) => sweep.fail("soft-deleted object", error),
                }
            }
        }
        may_have_journaled |= sweep
            .when_due(
                "soft-deleted object",
                due.objects,
                reading,
                self.maintain_soft_deleted_objects(micros),
            )
            .await;
        // A committed earlier sweep can create or empty the journal after the initial snapshot.
        let journal = if may_have_journaled {
            self.run_read(probe::journal_due).await
        } else {
            Ok(due.journal)
        };
        match journal {
            Ok(true) => sweep.record("deletion journal", self.maintain_deletion_journal().await),
            Ok(false) => {}
            Err(error) => sweep.fail("deletion journal", error),
        }
        match sweep.finish() {
            Err(error) if self.shutdown_interrupted(&error) => Err(probe_error.unwrap_or(error)),
            Err(error) => Err(error),
            Ok(next) => probe_error.map_or(Ok(next), Err),
        }
    }

    async fn sweep_resumable_sessions(
        &self,
        now: UnixMillis,
    ) -> Result<Option<SystemTime>, CatalogError> {
        let mut after = After::Start;
        let mut sweep = SweepOutcome::new(self);
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
            after = After::Key(last.id);
            for DueSession {
                id,
                blob_id,
                deadline,
            } in rows
            {
                // Wait for exclusive catalog work, but defer a session held by a request.
                let writer = match self.claim_sweep_writer(WriterKey::Resumable(id)).await {
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
                            sql::exec_on_writer(
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
            .maintenance
            .started
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let weak = std::sync::Arc::downgrade(&self.0);
        let wake = self.0.maintenance.wake.clone();
        // A panic must not end expiry for the rest of the process: restart the loop and report
        // the gap through readiness. The supervisor holds only a weak reference, like the loop.
        crate::task::spawn_restarting(
            "resumable-maintenance",
            self.0.maintenance.health.clone(),
            move || {
                let inner = weak.upgrade()?;
                (!inner.is_closing()).then(|| maintenance_loop(weak.clone(), wake.clone()))
            },
        );
    }

    /// False while a background loop is down: restarting after a panic until a pass succeeds,
    /// or failing every pass.
    pub(crate) fn background_tasks_running(&self) -> bool {
        !self.0.maintenance.health.is_down()
            && crate::sync::lock(&self.0.shutdown.background_health)
                .iter()
                .all(|health| !health.is_down())
    }

    /// Reports `health` through readiness for as long as the catalog lives, for a background
    /// worker the catalog does not start at open, such as the notification publisher.
    pub(crate) fn watch_background_task(&self, health: crate::task::TaskHealth) {
        crate::sync::lock(&self.0.shutdown.background_health).push(health);
    }

    /// Makes the next `count` maintenance passes panic before doing any work.
    #[cfg(test)]
    pub(crate) fn inject_maintenance_panics(&self, count: usize) {
        self.0
            .maintenance
            .panics
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
        if inner.is_closing() {
            return;
        }
        #[cfg(test)]
        if inner
            .maintenance
            .panics
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
        let clock = catalog.0.policy.clock.clone();
        let shutdown = catalog.0.shutdown.token.clone();
        let sleep = match catalog.run_resumable_maintenance().await {
            Ok(next) => {
                catalog.0.maintenance.health.pass_succeeded();
                next.map(|deadline| clock.sleep_until(deadline))
            }
            Err(error) if catalog.shutdown_interrupted(&error) => return,
            Err(error) => {
                // Expiry that fails on every pass leaves sessions and staged bytes behind, so
                // persistent failure turns readiness off until a pass succeeds again.
                catalog.0.maintenance.health.pass_failed();
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Warn,
                    crate::diagnostics::Event::ResumableMaintenanceRetry,
                    serde_json::json!({"message":format!("resumable maintenance failed; retrying: {error}")}),
                );
                // Operational retries continue even while the emulated clock is stopped.
                Some(SystemClock.sleep(RETRY_DELAY))
            }
        };
        drop(catalog);
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = notified => {}
            () = async {
                match sleep {
                    Some(sleep) => sleep.await,
                    None => std::future::pending().await,
                }
            } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    async fn a_pass_interrupted_at_reclamation_leaves_its_journal_for_recovery() {
        const CHILD: &str = "GCSM_TEST_JOURNAL_MAINTENANCE_CLOSE";
        if std::env::var_os(CHILD).is_none() {
            let name = std::thread::current().name().unwrap().to_owned();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--nocapture"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            assert!(
                output.stderr.is_empty(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let id = BlobId::generate().unwrap();
        let database = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
        database
            .execute("INSERT INTO pending_blob_deletions VALUES (?1)", [id])
            .unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let gate = catalog.wait_exclusive(deadline).await.unwrap();
        let scheduler = maintenance_loop(
            Arc::downgrade(&catalog.0),
            catalog.0.maintenance.wake.clone(),
        );
        tokio::pin!(scheduler);
        tokio::select! {
            biased;
            () = &mut scheduler => panic!("the probe must wait for the writer"),
            () = std::future::ready(()) => {}
        }
        catalog.0.shutdown.token.cancel();
        catalog.0.shutdown.tasks.drain().await;
        drop(gate);
        // Another publication may also ask for best-effort cleanup after the last owner leaves.
        catalog
            .reclaim_if(super::super::objects::Journaled::SOME)
            .await;
        tokio::time::timeout_at(deadline, scheduler).await.unwrap();
        assert!(catalog.resumable_maintenance_error().is_none());
        let journal = || {
            sql::scalar::<usize>(&database, "SELECT count(*) FROM pending_blob_deletions", [])
                .unwrap()
        };
        assert_eq!(journal(), 1);
        catalog.close().await.unwrap();
        let reopened = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(journal(), 0);
        reopened.close().await.unwrap();
    }

    #[tokio::test]
    async fn a_cancelled_catalog_never_starts_another_maintenance_pass() {
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog.inject_maintenance_panics(1);
        catalog.0.shutdown.token.cancel();
        assert!(!catalog.0.admission.slots.is_closed());
        let scheduler = maintenance_loop(
            Arc::downgrade(&catalog.0),
            catalog.0.maintenance.wake.clone(),
        );
        tokio::time::timeout(Duration::from_secs(5), scheduler)
            .await
            .unwrap();
        assert_eq!(
            catalog
                .0
                .maintenance
                .panics
                .load(std::sync::atomic::Ordering::Acquire),
            1
        );
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn expired_resumable_runs_only_session_cleanup_and_leaves_future_work_asleep() {
        use crate::{
            bucket::BucketAccess,
            clock::ManualClock,
            storage::{ResumableOptions, test_fixtures::bucket},
            test_support::TestDir,
        };
        use std::sync::atomic::Ordering;
        let data = TestDir::new();
        let clock = Arc::new(ManualClock::new(SystemTime::UNIX_EPOCH));
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
                "first",
                ResumableOptions::new(BucketAccess::Permissive),
                None,
            )
            .await
            .unwrap();
        clock.advance(Duration::from_secs(1));
        let later = catalog
            .create_resumable_session(
                "bucket",
                "later",
                ResumableOptions::new(BucketAccess::Permissive),
                None,
            )
            .await
            .unwrap();
        catalog.close().await.unwrap();
        let catalog = Catalog::open_with_clock(&data.0, clock.clone())
            .await
            .unwrap();
        let (w, r) = catalog.job_counts();
        let tasks = catalog.0.admin.cleanup_tasks.load(Ordering::Relaxed);
        clock.set(session.deadline);
        assert_eq!(
            catalog.maintain_resumable_sessions().await.unwrap(),
            Some(later.deadline)
        );
        assert_eq!(catalog.job_counts(), (w + 6, r + 2));
        assert_eq!(
            catalog.0.admin.cleanup_tasks.load(Ordering::Relaxed),
            tasks + 1
        );
        assert!(!session.blob_id.staging_path(&data.0).exists());
        assert!(later.blob_id.staging_path(&data.0).exists());
        catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn sweep_outcome_keeps_the_earliest_deadline_and_first_failure() {
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let mut sweep = SweepOutcome::new(&catalog);
        let early = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        sweep.record("later", Ok(Some(early + Duration::from_secs(10))));
        sweep.record("early", Ok(Some(early)));
        assert_eq!(sweep.finish().unwrap(), Some(early));
        let mut sweep = SweepOutcome::new(&catalog);
        sweep.fail(
            "first",
            unavailable(std::io::Error::from(std::io::ErrorKind::StorageFull)),
        );
        sweep.fail("later", unavailable("later failure"));
        assert!(
            matches!(sweep.finish().unwrap_err(), CatalogError::Io(cause) if cause.kind() == std::io::ErrorKind::StorageFull)
        );
        catalog.0.shutdown.token.cancel();
        let closed = || unavailable(crate::cause::BackendFailure::CatalogClosed);
        let mut sweep = SweepOutcome::new(&catalog);
        sweep.fail("interrupted", closed());
        sweep.fail(
            "disk",
            unavailable(std::io::Error::from(std::io::ErrorKind::StorageFull)),
        );
        sweep.fail("interrupted", closed());
        assert!(
            matches!(sweep.finish(), Err(CatalogError::Io(cause)) if cause.kind() == std::io::ErrorKind::StorageFull)
        );
        catalog.close().await.unwrap();
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
        let mut connection = Connection::open_in_memory().unwrap();
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
        forget_session_cleanup(&mut connection, live_plan).unwrap();
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
        forget_session_cleanup(&mut connection, absent_plan).unwrap();
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
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE objects(blob_id TEXT); CREATE TABLE pending_blob_deletions(blob_id TEXT);").unwrap();
        let id: BlobId = "0".repeat(32).parse().unwrap();
        let stage = id.staging_path(&data_dir);
        fs::write(&stage, b"old").unwrap();
        clean_session_files(&mut connection, &data_dir, &id).unwrap();
        assert!(!stage.exists());
        assert_eq!(fs::read(&protected).unwrap(), b"keep");
        fs::write(&stage, b"prefix-tail").unwrap();
        super::super::resumable_recovery::recover_prefix(&connection, &data_dir, &id, 6).unwrap();
        assert_eq!(fs::read(&stage).unwrap(), b"prefix");
        assert_eq!(fs::read(&protected).unwrap(), b"keep");
    }
}
