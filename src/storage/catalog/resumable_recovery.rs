use super::{CatalogError, unavailable};
use crate::storage::blob_id::BlobId;
use rusqlite::Connection;
use std::{fs, io, path::Path};

/// Directory ownership excludes writers; the committed offset is the only acknowledged prefix.
pub(super) fn recover(connection: &Connection, data_dir: &Path) -> Result<(), CatalogError> {
    let mut after = String::new();
    loop {
        let rows = connection.prepare_cached("SELECT id, blob_id, offset FROM resumable_sessions WHERE id > ?1 AND state = 'ACTIVE' ORDER BY id LIMIT 16")?
            .query_map([&after], |row| Ok((row.get::<_, String>(0)?, row.get::<_, BlobId>(1)?, row.get::<_, u64>(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if rows.is_empty() {
            return Ok(());
        }
        for (id, blob_id, offset) in rows {
            recover_prefix(connection, data_dir, &blob_id, offset)?;
            after = id;
        }
    }
}

pub(super) fn recover_prefix(
    connection: &Connection,
    data_dir: &Path,
    blob_id: &BlobId,
    offset: u64,
) -> Result<(), CatalogError> {
    check_unreferenced(connection, blob_id)?;
    recover_file_prefix(data_dir, blob_id, offset)
}

pub(super) fn check_unreferenced(
    connection: &Connection,
    blob_id: &BlobId,
) -> Result<(), CatalogError> {
    let live: bool = super::objects::blob_referenced(connection, blob_id)?;
    if live {
        return Err(unavailable(format!(
            "active upload blob {blob_id} is already published"
        )));
    }
    Ok(())
}

pub(super) fn recover_file_prefix(
    data_dir: &Path,
    blob_id: &BlobId,
    offset: u64,
) -> Result<(), CatalogError> {
    #[cfg(test)]
    super::fault::record_catalog_thread_io(data_dir);
    let stage = blob_id.staging_path(data_dir);
    let blob = blob_id.blob_path(data_dir);
    let staged = match fs::symlink_metadata(&stage) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(unavailable(error)),
    };
    let metadata = if let Some(metadata) = staged {
        if blob.try_exists()? {
            return Err(unavailable(format!(
                "active upload blob {blob_id} has ambiguous staging and published files"
            )));
        }
        metadata
    } else {
        let metadata = fs::symlink_metadata(&blob).map_err(|error| {
            unavailable(format!(
                "acknowledged upload prefix {blob_id} is missing: {error}"
            ))
        })?;
        validate_prefix(&metadata, blob_id, offset)?;
        // An ACTIVE row resolves a rename-before-commit crash; COMPLETE rows never enter here.
        #[cfg(test)]
        super::fault::hit(data_dir, super::fault::Step::ResumableRecoveryRename)
            .map_err(unavailable)?;
        fs::rename(&blob, &stage)?;
        #[cfg(all(test, unix))]
        super::fault::renamed(&blob, &stage);
        #[cfg(unix)]
        {
            let shard = super::objects::blob_shard(&blob)?;
            #[cfg(test)]
            super::fault::hit(data_dir, super::fault::Step::ResumableRecoveryShardSync)
                .map_err(unavailable)?;
            super::objects::sync_dir(shard)?;
            #[cfg(test)]
            super::fault::hit(data_dir, super::fault::Step::ResumableRecoveryStagingSync)
                .map_err(unavailable)?;
            super::objects::sync_dir(&data_dir.join("staging"))?;
        }
        metadata
    };
    validate_prefix(&metadata, blob_id, offset)?;
    if metadata.len() > offset {
        let file = fs::OpenOptions::new().write(true).open(&stage)?;
        #[cfg(test)]
        super::fault::hit(data_dir, super::fault::Step::ResumableRecoveryTruncate)
            .map_err(unavailable)?;
        file.set_len(offset)?;
        #[cfg(test)]
        super::fault::truncated(data_dir, &file.metadata()?);
        #[cfg(test)]
        super::fault::hit(data_dir, super::fault::Step::ResumableRecoverySync)
            .map_err(unavailable)?;
        file.sync_all()?;
        #[cfg(all(test, unix))]
        super::fault::file_synced(data_dir, &file.metadata()?);
    }
    #[cfg(test)]
    super::fault::hit(data_dir, super::fault::Step::ResumableRecovered).map_err(unavailable)?;
    Ok(())
}

fn validate_prefix(
    metadata: &fs::Metadata,
    blob_id: &BlobId,
    offset: u64,
) -> Result<(), CatalogError> {
    if !metadata.is_file() || metadata.len() < offset {
        return Err(unavailable(format!(
            "acknowledged upload prefix {blob_id} is truncated or has invalid type"
        )));
    }
    Ok(())
}
