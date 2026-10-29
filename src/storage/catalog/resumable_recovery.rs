use super::{CatalogError, unavailable};
use rusqlite::Connection;
use std::{fs, io, path::Path};

/// Directory ownership excludes writers; the committed offset is the only acknowledged prefix.
pub(super) fn recover(connection: &Connection, data_dir: &Path) -> Result<(), CatalogError> {
    let mut after = String::new();
    loop {
        let rows = connection.prepare("SELECT id, blob_id, offset FROM resumable_sessions WHERE id > ?1 AND state = 'ACTIVE' ORDER BY id LIMIT 16").map_err(unavailable)?
            .query_map([&after], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))).map_err(unavailable)?
            .collect::<rusqlite::Result<Vec<_>>>().map_err(unavailable)?;
        if rows.is_empty() {
            return Ok(());
        }
        for (id, blob_id, offset) in rows {
            recover_prefix(
                connection,
                data_dir,
                &blob_id,
                u64::try_from(offset).map_err(unavailable)?,
            )?;
            after = id;
        }
    }
}

pub(super) fn recover_prefix(
    connection: &Connection,
    data_dir: &Path,
    blob_id: &str,
    offset: u64,
) -> Result<(), CatalogError> {
    let id = super::super::blob_id::BlobId::parse(blob_id)
        .ok_or_else(|| unavailable("invalid opaque blob ID"))?;
    let stage = id.staging_path(data_dir);
    let blob = id.blob_path(data_dir);
    let live: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)",
            [blob_id],
            |row| row.get(0),
        )
        .map_err(unavailable)?;
    if live {
        return Err(unavailable(format!(
            "active upload blob {blob_id} is already published"
        )));
    }
    let staged = match fs::symlink_metadata(&stage) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(unavailable(error)),
    };
    let metadata = if let Some(metadata) = staged {
        if blob.try_exists().map_err(unavailable)? {
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
        fs::rename(&blob, &stage).map_err(unavailable)?;
        #[cfg(unix)]
        {
            super::objects::sync_dir(blob.parent().expect("blob parent")).map_err(unavailable)?;
            super::objects::sync_dir(&data_dir.join("staging")).map_err(unavailable)?;
        }
        metadata
    };
    validate_prefix(&metadata, blob_id, offset)?;
    if metadata.len() > offset {
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&stage)
            .map_err(unavailable)?;
        file.set_len(offset).map_err(unavailable)?;
        file.sync_all().map_err(unavailable)?;
    }
    Ok(())
}

fn validate_prefix(
    metadata: &fs::Metadata,
    blob_id: &str,
    offset: u64,
) -> Result<(), CatalogError> {
    if !metadata.is_file() || metadata.len() < offset {
        return Err(unavailable(format!(
            "acknowledged upload prefix {blob_id} is truncated or has invalid type"
        )));
    }
    Ok(())
}
