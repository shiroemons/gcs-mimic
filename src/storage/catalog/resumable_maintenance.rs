use super::{
    BlobBackend, Catalog, CatalogError, ResumableError, SessionState, unavailable, unix_millis,
};
use crate::storage::blob_id::BlobId;
use crate::storage::catalog::sql;
use rusqlite::{Connection, params};
#[cfg(test)]
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const PAGE_SIZE: i64 = 16;
const RETRY_DELAY: Duration = Duration::from_secs(1);

fn cleanup_page(
    connection: &Connection,
    now: i64,
    after: &str,
) -> Result<Vec<(String, BlobId, i64)>, CatalogError> {
    connection.prepare_cached("SELECT id, blob_id, deadline FROM resumable_sessions WHERE id > ?1 AND deadline <= ?2 UNION ALL SELECT id, blob_id, deadline FROM resumable_sessions WHERE id > ?1 AND state = 'CANCELLED' AND deadline > ?2 AND EXISTS(SELECT 1 FROM pending_blob_deletions WHERE blob_id = resumable_sessions.blob_id) ORDER BY id LIMIT ?3")?
        .query_map(params![after, now, PAGE_SIZE], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>().map_err(unavailable)
}

fn clean_record(
    connection: &Connection,
    backend: &BlobBackend,
    id: &str,
    blob_id: &BlobId,
    deadline: i64,
    now: i64,
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
    now: i64,
) -> Result<(), CatalogError> {
    let mut after = String::new();
    loop {
        let rows = cleanup_page(connection, now, &after)?;
        if rows.is_empty() {
            return Ok(());
        }
        for (id, blob_id, deadline) in rows {
            clean_record(connection, backend, &id, &blob_id, deadline, now)?;
            after = id;
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
        let id = lease.session.id.clone();
        let now = unix_millis(self.0.clock.now())?;
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
            self.resumable_session(&lease.session.id, &lease.session.bucket)
                .await?;
            return Err(ResumableError::Gone);
        }
        // Remove the cleanup journal only after file removal and directory sync succeed.
        let blob_id = lease.session.blob_id;
        let result = self.clean_session_storage(&lease.guard, blob_id).await;
        if let Err(error) = &result {
            *self
                .0
                .maintenance_error
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = Some(error.to_string());
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "upload_cleanup_deferred",
                serde_json::json!({"message":format!("cancelled upload cleanup failed; maintenance will retry: {error}")}),
            );
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
        *self.0.maintenance_error.lock().map_err(unavailable)? =
            result.as_ref().err().map(ToString::to_string);
        result
    }

    #[cfg(test)]
    pub(crate) fn resumable_maintenance_error(&self) -> Option<String> {
        self.0
            .maintenance_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    async fn maintain_resumable_inner(&self) -> Result<Option<SystemTime>, CatalogError> {
        let now = unix_millis(self.0.clock.now())?;
        let (derived_deadline, mut failure) = match self.maintain_derived_transfers_at(now).await {
            Ok(deadline) => (deadline, None),
            Err(error) => (None, Some(error)),
        };
        let resumable_deadline = self.sweep_resumable_sessions(now).await?;
        if let Some(error) = failure.take() {
            return Err(error);
        }
        let rewrite_deadline = self.maintain_rewrite_sessions(now).await?;
        let multipart_deadline = self.maintain_xml_multipart(now).await?;
        let bucket_deadline = self.maintain_soft_deleted_buckets(now).await?;
        Ok(resumable_deadline
            .into_iter()
            .chain(derived_deadline)
            .chain(rewrite_deadline)
            .chain(multipart_deadline)
            .chain(bucket_deadline)
            .min())
    }

    async fn sweep_resumable_sessions(&self, now: i64) -> Result<Option<SystemTime>, CatalogError> {
        let mut after = String::new();
        let mut failure = None;
        loop {
            let cursor = after.clone();
            let rows = self
                .run(move |connection| cleanup_page(connection, now, &cursor))
                .await?;
            let Some(last) = rows.last() else {
                break;
            };
            after = last.0.clone();
            for (id, blob_id, deadline) in rows {
                let catalog = self;
                let writer = match catalog.try_resumable_writer(&id) {
                    Ok(writer) => Arc::new(writer),
                    Err(CatalogError::Unavailable(cause))
                        if cause.failure()
                            == Some(crate::storage::failure::BackendFailure::SessionBusy) =>
                    {
                        continue;
                    }
                    Err(error) => {
                        failure.get_or_insert(error);
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
                    failure.get_or_insert(error);
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        self.run(move |connection| {
            let deadline: Option<i64> = sql::scalar(
                connection,
                "SELECT min(deadline) FROM resumable_sessions WHERE deadline > ?1",
                [now],
            )?;
            deadline
                .map(super::system_time_millis)
                .transpose()
                .map_err(unavailable)
        })
        .await
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
