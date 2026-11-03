use super::{
    BlobBackend, Catalog, CatalogError, ResumableError, SessionState, unavailable, unix_millis,
};
use crate::storage::blob_id::BlobId;
use crate::storage::catalog::sql;
use rusqlite::{Connection, params};
#[cfg(test)]
use std::path::Path;
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
        // Resumable session requests wake the maintenance sweep, so this runs per request.
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

pub(super) fn clean_session_storage(
    connection: &rusqlite::Connection,
    backend: &BlobBackend,
    blob_id: &BlobId,
) -> Result<(), CatalogError> {
    let live: bool = super::objects::blob_referenced(connection, blob_id)?;
    backend.remove_draft_durable(blob_id)?;
    // A completed object outlives its capability and must never be reclaimed with it.
    if !live {
        backend.remove_durable(blob_id)?;
    }
    sql::exec(
        connection,
        "DELETE FROM pending_blob_deletions WHERE blob_id = ?1",
        [blob_id],
    )?;
    Ok(())
}

impl Catalog {
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
        let backend = self.0.backend.clone();
        let result = self
            .run(move |connection| clean_session_storage(connection, &backend, &blob_id))
            .await;
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
        let mut after = String::new();
        loop {
            let catalog = self.clone();
            let (last, error) = self
                .run::<_, CatalogError>(move |connection| {
                    let rows = cleanup_page(connection, now, &after)?;
                    let mut last = None;
                    let mut failure = None;
                    for (id, blob_id, deadline) in rows {
                        last = Some(id.clone());
                        let result = catalog.try_resumable_writer(&id).and_then(|_guard| {
                            clean_record(
                                connection,
                                &catalog.0.backend,
                                &id,
                                &blob_id,
                                deadline,
                                now,
                            )
                        });
                        if let Err(error) = result {
                            failure = Some(error);
                        }
                    }
                    Ok((last, failure))
                })
                .await?;
            if error.is_some() {
                failure = error;
            }
            match last {
                Some(id) => after = id,
                None => break,
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        let resumable_deadline = self
            .run(|connection| {
                let deadline: Option<i64> = sql::scalar(
                    connection,
                    "SELECT min(deadline) FROM resumable_sessions",
                    [],
                )?;
                deadline
                    .map(super::system_time_millis)
                    .transpose()
                    .map_err(unavailable)
            })
            .await?;
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
