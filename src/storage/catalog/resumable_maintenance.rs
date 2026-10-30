use super::{Catalog, CatalogError, ResumableError, SessionState, unavailable, unix_millis};
use rusqlite::{Connection, TransactionBehavior, params};
use std::{
    fs, io,
    path::Path,
    time::{Duration, SystemTime},
};

const PAGE_SIZE: i64 = 16;
const RETRY_DELAY: Duration = Duration::from_secs(1);

fn cleanup_page(
    connection: &Connection,
    now: i64,
    after: &str,
) -> Result<Vec<(String, String, i64)>, CatalogError> {
    connection.prepare_cached("SELECT id, blob_id, deadline FROM resumable_sessions WHERE id > ?1 AND deadline <= ?2 UNION ALL SELECT id, blob_id, deadline FROM resumable_sessions WHERE id > ?1 AND state = 'CANCELLED' AND deadline > ?2 AND EXISTS(SELECT 1 FROM pending_blob_deletions WHERE blob_id = resumable_sessions.blob_id) ORDER BY id LIMIT ?3").map_err(unavailable)?
        .query_map(params![after, now, PAGE_SIZE], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).map_err(unavailable)?
        .collect::<rusqlite::Result<_>>().map_err(unavailable)
}

fn clean_record(
    connection: &Connection,
    data_dir: &Path,
    id: &str,
    blob_id: &str,
    deadline: i64,
    now: i64,
) -> Result<(), CatalogError> {
    clean_session_files(connection, data_dir, blob_id)?;
    if deadline <= now {
        connection
            // xtask-lint: allow(text::uncached_statement) -- expired sessions are removed only by open-time recovery and the maintenance sweep
            .execute(
                "DELETE FROM resumable_sessions WHERE id = ?1 AND deadline <= ?2",
                params![id, now],
            )
            .map_err(unavailable)?;
    }
    Ok(())
}

pub(super) fn recover_due(
    connection: &Connection,
    data_dir: &Path,
    now: i64,
) -> Result<(), CatalogError> {
    let mut after = String::new();
    loop {
        let rows = cleanup_page(connection, now, &after)?;
        if rows.is_empty() {
            return Ok(());
        }
        for (id, blob_id, deadline) in rows {
            clean_record(connection, data_dir, &id, &blob_id, deadline, now)?;
            after = id;
        }
    }
}

fn unlink(path: &Path) -> Result<(), CatalogError> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        result => result.map_err(unavailable)?,
    }
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && parent.exists()
    {
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(unavailable)?;
    }
    Ok(())
}

pub(super) fn clean_session_files(
    connection: &rusqlite::Connection,
    data_dir: &Path,
    blob_id: &str,
) -> Result<(), CatalogError> {
    let blob = super::super::blob_id::BlobId::parse(blob_id)
        .ok_or_else(|| unavailable("invalid opaque blob ID"))?;
    let live: bool = connection
        .prepare_cached("SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)")
        .and_then(|mut statement| statement.query_row([blob_id], |row| row.get(0)))
        .map_err(unavailable)?;
    unlink(&blob.staging_path(data_dir))?;
    // A completed object outlives its capability and must never be reclaimed with it.
    if !live {
        unlink(&blob.blob_path(data_dir))?;
    }
    connection
        .prepare_cached("DELETE FROM pending_blob_deletions WHERE blob_id = ?1")
        .and_then(|mut statement| statement.execute([blob_id]))
        .map_err(unavailable)?;
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
        let blob_id = lease.session.blob_id.clone();
        let changed = self.run(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
            let changed = transaction.prepare_cached("UPDATE resumable_sessions SET state = 'CANCELLED' WHERE id = ?1 AND state = 'ACTIVE' AND deadline > ?2").and_then(|mut statement| statement.execute(params![id, now])).map_err(unavailable)?;
            if changed == 1 {
                transaction.prepare_cached("INSERT OR IGNORE INTO pending_blob_deletions VALUES (?1)").and_then(|mut statement| statement.execute([blob_id])).map_err(unavailable)?;
            }
            transaction.commit().map_err(unavailable)?;
            Ok(changed)
        }).await?;
        if changed != 1 {
            self.resumable_session(&lease.session.id, &lease.session.bucket)
                .await?;
            return Err(ResumableError::Gone);
        }
        // Remove the cleanup journal only after file removal and directory sync succeed.
        let blob_id = lease.session.blob_id.clone();
        let data_dir = self.0.data_dir.clone();
        let result = self
            .run(move |connection| clean_session_files(connection, &data_dir, &blob_id))
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
                .run(move |connection| {
                    let rows = cleanup_page(connection, now, &after)?;
                    let mut last = None;
                    let mut failure = None;
                    for (id, blob_id, deadline) in rows {
                        last = Some(id.clone());
                        let result = catalog.try_resumable_writer(&id).and_then(|_guard| {
                            clean_record(
                                connection,
                                &catalog.0.data_dir,
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
                let deadline: Option<i64> = connection
                    // xtask-lint: allow(text::uncached_statement) -- the next deadline is read once per maintenance pass
                    .query_row("SELECT min(deadline) FROM resumable_sessions", [], |row| {
                        row.get(0)
                    })
                    .map_err(unavailable)?;
                deadline
                    .map(super::system_time_millis)
                    .transpose()
                    .map_err(unavailable)
            })
            .await?;
        let rewrite_deadline = self.maintain_rewrite_sessions(now).await?;
        Ok(resumable_deadline
            .into_iter()
            .chain(derived_deadline)
            .chain(rewrite_deadline)
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
        tokio::spawn(async move {
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
                let catalog = Catalog(inner);
                let delay = match catalog.run_resumable_maintenance().await {
                    Ok(next) => next.map(|deadline| {
                        deadline
                            .duration_since(catalog.0.clock.now())
                            .unwrap_or_default()
                    }),
                    Err(error) => {
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
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_session_blob_ids_cannot_select_cleanup_or_recovery_paths() {
        let dir = crate::test_support::TestDir::new();
        let data_dir = dir.0.join("data");
        fs::create_dir_all(data_dir.join("staging")).unwrap();
        let protected = dir.0.join("protected");
        fs::write(&protected, b"keep").unwrap();
        let connection = Connection::open_in_memory().unwrap();
        for id in ["", "a", "../../protected", &"A".repeat(32), &"é".repeat(16)] {
            let cleanup = clean_session_files(&connection, &data_dir, id).unwrap_err();
            let recovery =
                super::super::resumable_recovery::recover_prefix(&connection, &data_dir, id, 0)
                    .unwrap_err();
            assert!(cleanup.to_string().contains("invalid opaque blob ID"));
            assert!(recovery.to_string().contains("invalid opaque blob ID"));
            assert_eq!(fs::read(&protected).unwrap(), b"keep");
        }
    }
}
