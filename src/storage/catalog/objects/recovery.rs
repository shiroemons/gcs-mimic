use super::*;
use crate::storage::catalog::UnixMillis;

fn os_metadata(entry: &fs::DirEntry) -> bool {
    let name = entry.file_name();
    let name = name.to_string_lossy();
    name == ".DS_Store" || name.starts_with("._")
}

pub(in crate::storage::catalog) fn recover(
    connection: &Connection,
    data_dir: &Path,
    now: UnixMillis,
) -> Result<(), CatalogError> {
    // The phases run in this order: staging and derived state first, then the deletion
    // journal, so the reference and orphan scans only see committed, durable state.
    let recover = || -> Result<(), ObjectError> {
        fs::create_dir_all(data_dir.join("staging"))?;
        fs::create_dir_all(data_dir.join("blobs"))?;
        #[cfg(unix)]
        {
            sync_dir(data_dir)?;
            // A pre-crash shard creator may have stopped before making its parent durable.
            sync_dir(&data_dir.join("blobs"))?;
        }
        super::super::resumable_recovery::recover(connection, data_dir)?;
        super::super::derived::recover(connection, data_dir, now)?;
        remove_unretained_staging(connection, data_dir)?;
        drain_deletion_journal(connection, data_dir)?;
        verify_referenced_blobs(connection, data_dir)?;
        remove_orphan_blobs(connection, data_dir)
    };
    recover().map_err(|error| match error {
        ObjectError::Unavailable(reason) => CatalogError::Unavailable(reason),
        error => unavailable(error),
    })
}

/// Removes staging entries no active upload or derived transfer still owns.
fn remove_unretained_staging(connection: &Connection, data_dir: &Path) -> Result<(), ObjectError> {
    for entry in fs::read_dir(data_dir.join("staging"))? {
        let entry = entry?;
        if os_metadata(&entry) {
            continue;
        }
        let retained: bool = sql::scalar(
            connection,
            "SELECT EXISTS(SELECT 1 FROM resumable_sessions WHERE blob_id = ?1 AND state = 'ACTIVE') OR EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1) OR EXISTS(SELECT 1 FROM derived_inputs WHERE blob_id = ?1) OR EXISTS(SELECT 1 FROM xml_multipart_parts WHERE blob_id = ?1)",
            [entry.file_name().to_string_lossy().as_ref()],
        )?;
        if retained {
            continue;
        }
        #[cfg(test)]
        fault::hit(data_dir, Step::RecoveryStagingUnlink).map_err(|error| {
            crate::storage::failure::object_context(
                format_args!("staging entry {}", entry.path().display()),
                error,
            )
        })?;
        unlink(&entry.path()).map_err(|error| {
            crate::storage::failure::object_context(
                format_args!("staging entry {}", entry.path().display()),
                error,
            )
        })?;
    }
    #[cfg(unix)]
    sync_dir(&data_dir.join("staging"))?;
    Ok(())
}

/// Finishes deletions committed before a crash.
fn drain_deletion_journal(connection: &Connection, data_dir: &Path) -> Result<(), ObjectError> {
    // Keyset pages bound memory independently of the number of objects or pending deletions.
    loop {
        let ids = connection
            .prepare_cached(
                "SELECT blob_id FROM pending_blob_deletions AS pending WHERE NOT EXISTS (SELECT 1 FROM objects WHERE objects.blob_id = pending.blob_id) ORDER BY blob_id LIMIT 128",
            )?
            .query_map([], |r| r.get::<_, BlobId>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if ids.is_empty() {
            break;
        }
        for id in ids {
            reclaim(connection, data_dir, &id).map_err(|error| {
                crate::storage::failure::object_context(
                    format_args!("deletion journal blob {id}"),
                    error,
                )
            })?;
        }
    }
    sql::optional_scalar::<BlobId>(connection, JOURNAL_CONFLICT, [])?
        .map_or(Ok(()), |id| Err(live_journal_error(id)))
}

/// Every catalog row must name a regular blob file of the recorded size.
fn verify_referenced_blobs(connection: &Connection, data_dir: &Path) -> Result<(), ObjectError> {
    let mut after = String::new();
    loop {
        let rows = connection
            .prepare_cached(
                "SELECT blob_id, size FROM objects WHERE blob_id > ?1 ORDER BY blob_id LIMIT 128",
            )?
            .query_map([&after], |r| {
                Ok((r.get::<_, BlobId>(0)?, r.get::<_, u64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.is_empty() {
            return Ok(());
        }
        for (id, size) in rows {
            let path = id.blob_path(data_dir);
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                crate::storage::failure::object_context(format_args!("referenced blob {id}"), error)
            })?;
            if !metadata.is_file() || metadata.len() != size {
                return Err(object_unavailable(format!(
                    "referenced blob {id} has invalid size or type"
                )));
            }
            after = id.to_hex();
        }
    }
}

/// Removes blobs no catalog row references; misplaced entries fail recovery.
fn remove_orphan_blobs(connection: &Connection, data_dir: &Path) -> Result<(), ObjectError> {
    // Directory iterators and indexed single-ID probes also keep orphan scans bounded.
    for shard in fs::read_dir(data_dir.join("blobs"))? {
        let shard = shard?;
        if os_metadata(&shard) {
            continue;
        }
        if !shard.file_type()?.is_dir() {
            return Err(object_unavailable(format!(
                "invalid blob directory entry {}",
                shard.path().display()
            )));
        }
        for blob in fs::read_dir(shard.path())? {
            let blob = blob?;
            if os_metadata(&blob) {
                continue;
            }
            let id = blob.file_name().into_string().map_err(|_| {
                object_unavailable(format!("invalid blob ID {}", blob.path().display()))
            })?;
            let id = id.parse::<BlobId>().map_err(|error| {
                crate::storage::failure::object_context(
                    format_args!("blob entry {}", blob.path().display()),
                    error,
                )
            })?;
            let expected = id.blob_path(data_dir);
            if expected != blob.path() {
                return Err(object_unavailable(format!(
                    "blob in wrong shard {}",
                    blob.path().display()
                )));
            }
            // The same statement as `reclaim`, so this per-blob scan reuses its cache entry.
            let referenced: bool = blob_referenced(connection, &id)?;
            if !referenced {
                unlink(&blob.path()).map_err(|error| {
                    crate::storage::failure::object_context(
                        format_args!("blob entry {}", blob.path().display()),
                        error,
                    )
                })?;
            }
        }
        #[cfg(unix)]
        sync_dir(&shard.path())?;
    }
    Ok(())
}
