use super::{Catalog, CatalogError, ResumableError, SessionState, unavailable, unix_millis};
use rusqlite::params;
use std::{fs, io, path::Path};

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
    let live: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)",
            [blob_id],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    unlink(&data_dir.join("staging").join(blob_id))?;
    // A completed object outlives its capability and must never be reclaimed with it.
    if !live {
        unlink(&data_dir.join("blobs").join(&blob_id[..2]).join(blob_id))?;
    }
    Ok(())
}

impl Catalog {
    pub async fn cancel_resumable_session(
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
        let changed = self.run(move |connection| {
            connection.execute("UPDATE resumable_sessions SET state = 'CANCELLED' WHERE id = ?1 AND state = 'ACTIVE' AND deadline > ?2", params![id, now]).map_err(unavailable)
        }).await?;
        if changed != 1 {
            return Err(ResumableError::NotFound);
        }
        // The terminal row is the durable cleanup journal if unlink or directory sync fails.
        let blob_id = lease.session.blob_id.clone();
        let data_dir = self.0.data_dir.clone();
        self.run(move |connection| clean_session_files(connection, &data_dir, &blob_id))
            .await?;
        Ok(())
    }
}
