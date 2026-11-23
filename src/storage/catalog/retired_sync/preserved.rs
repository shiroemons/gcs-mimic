//! The files that retired synchronization rows still name. A write-back that would have replaced
//! a file a user had edited first copied that file to `sync-conflicts/` in the data directory; a
//! write-back that stopped midway may have left the file it displaced, or its own copy, in the
//! private workspace inside the synchronized folder. Once schema 58 drops the rows, only the
//! manifest written here names those files, so it becomes durable before that transaction
//! starts. Each start warns while `sync-conflicts/` holds entries. Nothing here deletes, moves or
//! changes the files, in the data directory or in a user's folder.

#[cfg(test)]
use super::super::fault::{self, Step};
use super::super::{CatalogError, sql, unavailable};
use rusqlite::{Connection, Row, types::Type};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::Path,
};

/// Where synchronization kept its copies of user edits, relative to the data directory.
const CONFLICTS: &str = "sync-conflicts";
/// The manifest's name in the data directory.
const MANIFEST: &str = "sync-retired.json";
/// The manifest before its rename. An attempt that stopped before the rename leaves it, and the
/// next attempt writes it again from the start.
const STAGED: &str = "sync-retired.json.tmp";
/// The private directory synchronization kept inside each synchronized folder.
const WORKSPACE: &str = ".gcs-mimic-sync-work";

pub(super) const DESCRIPTION: &str = "gcs-mimic removed directory synchronization at catalog schema 58 and \
listed here the files its records still named. conflict_copies are copies of edits made in a \
synchronized folder, kept in this data directory; state capturing means the copy may be \
incomplete. workspace_entries may still exist in the private workspace of a formerly \
synchronized folder, holding the file a write-back displaced or its own copy of the object; \
target is where that file was, relative to root. gcs-mimic never deletes, moves or changes any \
of these files: inspect them and remove them yourself once they are no longer needed. md5 and \
crc32c are base64, as in the JSON API's md5Hash and crc32c.";

/// Whether any retired row names a file: a conflict copy not yet discarded (state 2), or a
/// workspace entry [`WORKSPACE_ENTRIES`] lists.
const NAMES_FILES: &str = "SELECT EXISTS(SELECT 1 FROM sync_conflicts WHERE state != 2)
    OR EXISTS(SELECT 1 FROM sync_writeback_ops WHERE phase != 4 AND NOT (phase = 0 AND kind = 0))
    OR EXISTS(SELECT 1 FROM sync_recovery_workspaces WHERE phase != 2)";

/// The conflict copies not yet discarded, in the order synchronization captured them.
const CONFLICT_COPIES: &str = "SELECT id, bucket, name, generation, size, retained, state
    FROM sync_conflicts WHERE state != 2 ORDER BY ordinal";

/// The workspace entries a write-back may have left, the rule synchronization's own restart
/// recovery followed: any operation not complete (phase 4), except a publish (kind 0) not yet
/// armed (phase 0), whose entry can hold only its private copy of the object; and any recovery
/// workspace not yet released (phase 2).
const WORKSPACE_ENTRIES: &str = "SELECT r.location, o.id, o.bucket, o.name, o.path
    FROM sync_writeback_ops o JOIN sync_roots r ON r.root_id = o.root_id
    WHERE o.phase != 4 AND NOT (o.phase = 0 AND o.kind = 0)
    UNION
    SELECT r.location, w.id, o.bucket, o.name, o.path
    FROM sync_recovery_workspaces w JOIN sync_roots r ON r.root_id = w.root_id
    LEFT JOIN sync_writeback_ops o ON o.root_id = w.root_id AND o.id = w.id
    WHERE w.phase != 2
    ORDER BY 1, 2";

/// Whether the manifest has anything to list.
pub(super) fn named(connection: &Connection) -> Result<bool, CatalogError> {
    Ok(sql::scalar(connection, NAMES_FILES, [])?)
}

/// Writes the manifest durably into `data_dir`: a staged file written, flushed and synced, then
/// renamed over any manifest an earlier attempt published, then the directory synced. The rows
/// it reads are the ones a later transaction drops, so writing it again from the same rows
/// gives the same bytes.
pub(super) fn write_manifest(connection: &Connection, data_dir: &Path) -> Result<(), CatalogError> {
    let mut manifest = Manifest::create(data_dir)?;
    manifest.put(b"{\n  \"description\": ")?;
    manifest.put(&serde_json::to_vec(DESCRIPTION).map_err(unavailable)?)?;
    manifest.list(
        connection,
        "conflict_copies",
        CONFLICT_COPIES,
        conflict_copy,
    )?;
    manifest.list(
        connection,
        "workspace_entries",
        WORKSPACE_ENTRIES,
        workspace_entry,
    )?;
    manifest.put(b"\n}\n")?;
    manifest.publish()
}

/// The staged manifest while it is written. Each record goes through one `put`, which the test
/// fault seam can fail; the buffer keeps the writes few.
struct Manifest<'a> {
    data_dir: &'a Path,
    out: io::BufWriter<fs::File>,
}

impl<'a> Manifest<'a> {
    fn create(data_dir: &'a Path) -> io::Result<Self> {
        let path = data_dir.join(STAGED);
        let file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                #[cfg(all(test, unix))]
                fault::file_created(&path);
                file
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let file = fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&path)?;
                #[cfg(test)]
                fault::truncated(data_dir, &file.metadata()?);
                file
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            data_dir,
            out: io::BufWriter::new(file),
        })
    }

    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        fault::hit_io(self.data_dir, Step::RetiredManifestWrite)?;
        self.out.write_all(bytes)
    }

    /// Writes `key` and one compact JSON object per row `query` selects, a line each.
    fn list<T: Serialize>(
        &mut self,
        connection: &Connection,
        key: &str,
        query: &'static str,
        read: fn(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<(), CatalogError> {
        self.put(format!(",\n  \"{key}\": [").as_bytes())?;
        let mut statement = connection.prepare_cached(query)?;
        let mut rows = statement.query([])?;
        let mut written = false;
        while let Some(row) = rows.next()? {
            let separator: &[u8] = if written { b",\n    " } else { b"\n    " };
            let mut line = separator.to_vec();
            serde_json::to_writer(&mut line, &read(row)?).map_err(unavailable)?;
            self.put(&line)?;
            written = true;
        }
        let close: &[u8] = if written { b"\n  ]" } else { b"]" };
        Ok(self.put(close)?)
    }

    fn publish(mut self) -> Result<(), CatalogError> {
        #[cfg(test)]
        fault::hit_io(self.data_dir, Step::RetiredManifestFlush)?;
        self.out.flush()?;
        let file = self
            .out
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?;
        #[cfg(test)]
        fault::hit_io(self.data_dir, Step::RetiredManifestSync)?;
        file.sync_all()?;
        #[cfg(test)]
        fault::file_synced(self.data_dir, &file.metadata()?);
        drop(file);
        let staged = self.data_dir.join(STAGED);
        let manifest = self.data_dir.join(MANIFEST);
        #[cfg(test)]
        fault::hit_io(self.data_dir, Step::RetiredManifestRename)?;
        fs::rename(&staged, &manifest)?;
        // Windows has no directory sync; like the blob store there, it relies on the rename.
        #[cfg(unix)]
        {
            #[cfg(test)]
            fault::renamed(&staged, &manifest);
            #[cfg(test)]
            fault::hit_io(self.data_dir, Step::RetiredManifestDirSync)?;
            super::super::objects::sync_dir(self.data_dir)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum CopyState {
    /// The copy was being written when synchronization last ran, so it may be incomplete.
    Capturing,
    Registered,
}
sql::integer_enum!(CopyState, { Capturing => 0, Registered => 1 });

#[derive(Serialize)]
struct ConflictCopy {
    /// Relative to the data directory, with `/` on every platform.
    path: String,
    bucket: String,
    object: String,
    /// The object's generation the copy was taken against; absent when there was no object.
    #[serde(skip_serializing_if = "Option::is_none")]
    generation: Option<i64>,
    size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    md5: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    crc32c: Option<String>,
    state: CopyState,
}

/// What `sync_conflicts.retained` holds: JSON of the copy's observation once it was written. The
/// manifest reports only the digests; the rest is the copy's file identity.
#[derive(Deserialize)]
struct Retained {
    hashes: Option<RetainedHashes>,
}

#[derive(Deserialize)]
struct RetainedHashes {
    md5: [u8; 16],
    crc32c: u32,
}

fn conflict_copy(row: &Row<'_>) -> rusqlite::Result<ConflictCopy> {
    let id: String = row.get(0)?;
    let hashes = row
        .get::<_, Option<Vec<u8>>>(5)?
        .map(|bytes| serde_json::from_slice::<Retained>(&bytes))
        .transpose()
        .map_err(|error| sql::conversion_error(5, Type::Blob, error))?
        .and_then(|retained| retained.hashes);
    Ok(ConflictCopy {
        path: format!("{CONFLICTS}/{id}"),
        bucket: row.get(1)?,
        object: row.get(2)?,
        generation: row.get(3)?,
        size: row.get(4)?,
        md5: hashes
            .as_ref()
            .map(|hashes| crate::base64::encode(&hashes.md5)),
        crc32c: hashes.map(|hashes| crate::base64::encode(&hashes.crc32c.to_be_bytes())),
        state: row.get(6)?,
    })
}

#[derive(Serialize)]
struct WorkspaceEntry {
    /// The synchronized folder, as synchronization recorded it.
    root: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    bucket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    object: Option<String>,
    /// Where the file was, relative to `root`, with `/` on every platform.
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
}

fn workspace_entry(row: &Row<'_>) -> rusqlite::Result<WorkspaceEntry> {
    // The location holds the folder's path in the platform's encoding; a name that is not
    // UTF-8 keeps a replacement character, which is still enough to find the folder.
    let root = String::from_utf8_lossy(&row.get::<_, Vec<u8>>(0)?).into_owned();
    let id: String = row.get(1)?;
    let path = Path::new(&root)
        .join(WORKSPACE)
        .join(id)
        .display()
        .to_string();
    Ok(WorkspaceEntry {
        root,
        path,
        bucket: row.get(2)?,
        object: row.get(3)?,
        target: row.get(4)?,
    })
}

/// The start-up warning about the copies in `sync-conflicts/`.
#[derive(Serialize)]
struct PreservedCopies {
    message: String,
    directory: String,
    entries: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest: Option<String>,
}

/// What `sync-conflicts/` in `data_dir` holds, or `None` while it is missing or empty.
fn preserved_copies(data_dir: &Path) -> io::Result<Option<PreservedCopies>> {
    let directory = data_dir.join(CONFLICTS);
    let listing = match fs::read_dir(&directory) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut entries: u64 = 0;
    for entry in listing {
        entry?;
        entries += 1;
    }
    if entries == 0 {
        return Ok(None);
    }
    let path = data_dir.join(MANIFEST);
    let manifest = path.try_exists()?.then(|| path.display().to_string());
    let directory = directory.display().to_string();
    let noun = if entries == 1 { "entry" } else { "entries" };
    let listed = manifest
        .as_ref()
        .map_or_else(String::new, |manifest| format!(" {manifest} lists them."));
    let message = format!(
        "{directory} holds {entries} {noun}: copies of edits that the removed directory \
         synchronization preserved. gcs-mimic never deletes them automatically.{listed} Inspect \
         them, then remove the directory yourself once you no longer need them."
    );
    Ok(Some(PreservedCopies {
        message,
        directory,
        entries,
        manifest,
    }))
}

/// Warns while `sync-conflicts/` in `data_dir` holds entries. Every open repeats it, so the
/// copies stay visible until the operator removes them.
pub(in crate::storage::catalog) fn warn_preserved_copies(data_dir: &Path) -> io::Result<()> {
    if let Some(copies) = preserved_copies(data_dir)? {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Warn,
            "sync_conflicts_preserved",
            copies,
        );
    }
    Ok(())
}
