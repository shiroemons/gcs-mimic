//! The files that retired synchronization rows still name. A write-back that would have replaced
//! a file a user had edited first copied that file to `sync-conflicts/` in the data directory; a
//! write-back that stopped midway may have left the file it displaced, or its own copy, in the
//! private workspace inside the synchronized folder. Once schema 58 drops the rows, only the
//! manifest written here names those files, so it becomes durable before that transaction
//! starts. [`warning`] tells the operator about them when the transaction commits and at every
//! start while a manifest or a copy remains. Nothing here deletes, moves or changes the files,
//! in the data directory or in a user's folder.

mod warning;

pub(super) use warning::Listing;
pub(in crate::storage::catalog) use warning::warn_retired_files;

#[cfg(test)]
use super::super::fault::{self, Step};
use super::super::{CatalogError, objects, sql, unavailable};
use crate::storage::failure::catalog_context;
use rusqlite::{
    Connection, Row,
    types::{FromSql, FromSqlResult, ValueRef},
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
};

/// Where synchronization kept its copies of user edits, relative to the data directory.
const CONFLICTS: &str = "sync-conflicts";
/// The manifest's name in the data directory.
const MANIFEST: &str = "sync-retired.json";
/// The manifest before its rename is `sync-retired.json.<32 lowercase hex digits>.tmp`, a new
/// name for each attempt. An attempt that stopped before the rename leaves its staged file, and a
/// later attempt removes it. Windows may delay that removal while an antivirus scanner or the
/// indexer holds the file, so a fresh name keeps the delay from failing the next attempt.
const STAGED_PREFIX: &str = "sync-retired.json.";
const STAGED_SUFFIX: &str = ".tmp";
/// The random bytes in a staged name.
const STAGED_ID_BYTES: usize = 16;
/// The private directory synchronization kept inside each synchronized folder.
const WORKSPACE: &str = ".gcs-mimic-sync-work";
/// The marker synchronization kept inside each synchronized folder.
const MARKER: &str = ".gcs-mimic-sync.json";

/// The marker and the workspace synchronization kept in `folder`, by name, among those it can
/// see. The workspace may hold the only copy of a file a write-back displaced, so only the
/// operator decides when they go.
pub(crate) fn left_in_folder(folder: &Path) -> Vec<&'static str> {
    [MARKER, WORKSPACE]
        .into_iter()
        .filter(|name| fs::symlink_metadata(folder.join(name)).is_ok())
        .collect()
}

pub(super) const DESCRIPTION: &str = "gcs-mimic removed directory synchronization at catalog schema 58 and \
listed here the files its records still named. Folder paths are as the gcs-mimic process that \
synchronized them saw them, for example inside its container. folders lists every formerly \
synchronized folder; each may still hold synchronization's marker .gcs-mimic-sync.json and its \
private workspace .gcs-mimic-sync-work/. folder_base64 gives a folder path's bytes when they \
are not UTF-8. conflict_copies are copies of edits made in a synchronized folder, kept in this \
data directory at path; state capturing means the copy may be incomplete. workspace_entries may \
still exist in the private workspace of a formerly synchronized folder, holding the file a \
write-back displaced or its own copy of the object. target is where a file was, relative to its \
folder. gcs-mimic never deletes, moves or changes any of these files: inspect them and remove \
them yourself once they are no longer needed. md5 and crc32c are base64, as in the JSON API's \
md5Hash and crc32c; digests unreadable means synchronization's record of them could not be \
read.";

/// Every formerly synchronized folder.
const FOLDERS: &str = "SELECT location FROM sync_roots ORDER BY location";

// The rows that name files, each as the tables and the condition of a query. [`NAMES_FILES`]
// asks whether any of them exists and the listings select them, so a manifest is written
// exactly when it lists something.

/// A conflict copy not yet discarded (state 2): its file is still under `sync-conflicts/`.
macro_rules! kept_copies {
    () => {
        "FROM sync_conflicts c JOIN sync_roots r ON r.root_id = c.root_id WHERE c.state != 2"
    };
}

/// A write-back not complete (phase 4), except a publish (kind 0) not yet armed (phase 0), whose
/// workspace entry can hold only its private copy of the object. Synchronization's own restart
/// recovery followed the same rule.
macro_rules! open_write_backs {
    () => {
        "FROM sync_writeback_ops o JOIN sync_roots r ON r.root_id = o.root_id
        WHERE o.phase != 4 AND NOT (o.phase = 0 AND o.kind = 0)"
    };
}

/// A recovery workspace not yet released (phase 2), with the write-back it recovers while that
/// row remains.
macro_rules! held_workspaces {
    () => {
        "FROM sync_recovery_workspaces w JOIN sync_roots r ON r.root_id = w.root_id
        LEFT JOIN sync_writeback_ops o ON o.root_id = w.root_id AND o.id = w.id
        WHERE w.phase != 2"
    };
}

/// Whether any retired row names a file.
const NAMES_FILES: &str = concat!(
    "SELECT EXISTS(SELECT 1 ",
    kept_copies!(),
    ") OR EXISTS(SELECT 1 ",
    open_write_backs!(),
    ") OR EXISTS(SELECT 1 ",
    held_workspaces!(),
    ")"
);

/// The conflict copies, in the order synchronization captured them. Each was taken from the
/// path synchronization gave its object in the folder, `<bucket>/<name>`.
const CONFLICT_COPIES: &str = concat!(
    "SELECT r.location, c.id, c.bucket || '/' || c.name, c.bucket, c.incarnation, c.name,
    c.generation, c.size, c.retained, c.state ",
    kept_copies!(),
    " ORDER BY c.ordinal"
);

/// The workspace entries a write-back or a recovery may have left. A write-back that a held
/// recovery workspace recovers is one entry.
const WORKSPACE_ENTRIES: &str = concat!(
    "SELECT r.location, o.id, o.path, o.bucket, o.incarnation, o.name ",
    open_write_backs!(),
    " UNION SELECT r.location, w.id, o.path, o.bucket, o.incarnation, o.name ",
    held_workspaces!(),
    " ORDER BY 1, 2"
);

/// Whether the manifest has anything to list.
pub(super) fn named(connection: &Connection) -> Result<bool, CatalogError> {
    Ok(sql::scalar(connection, NAMES_FILES, [])?)
}

/// Writes the manifest durably into `data_dir` and says where and what it lists: a staged file
/// written, flushed and synced, then renamed to the first manifest name that is free, then the
/// directory synced. The rows it reads are the ones a later transaction drops, so writing it
/// again from the same rows gives the same bytes, and a manifest an earlier attempt published
/// with them is kept instead.
pub(super) fn write_manifest(
    connection: &Connection,
    data_dir: &Path,
) -> Result<Listing, CatalogError> {
    let mut manifest = Manifest::create(data_dir)?;
    manifest.put(b"{\n  \"description\": ")?;
    manifest.put(&serde_json::to_vec(DESCRIPTION).map_err(unavailable)?)?;
    manifest.list(connection, "folders", FOLDERS, |row| {
        row.get::<_, Folder>(0)
    })?;
    let conflict_copies = manifest.list(
        connection,
        "conflict_copies",
        CONFLICT_COPIES,
        conflict_copy,
    )?;
    let mut workspace_entries = Vec::new();
    manifest.list(connection, "workspace_entries", WORKSPACE_ENTRIES, |row| {
        let entry = workspace_entry(row)?;
        warning::FolderEntries::count(&mut workspace_entries, &entry.folder);
        Ok(entry)
    })?;
    manifest.put(b"\n}\n")?;
    Ok(Listing {
        manifest: manifest.publish()?,
        conflicts: data_dir.join(CONFLICTS),
        conflict_copies,
        workspace_entries,
    })
}

/// The error of a manifest step, naming the step and the file it worked on.
fn failed<'a>(step: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> CatalogError + 'a {
    move |error| {
        catalog_context(
            format_args!(
                "retired synchronization manifest: could not {step} {}",
                path.display()
            ),
            error,
        )
    }
}

/// Whether `name` is a name an attempt gives its staged manifest.
fn is_staged(name: &str) -> bool {
    name.strip_prefix(STAGED_PREFIX)
        .and_then(|rest| rest.strip_suffix(STAGED_SUFFIX))
        .is_some_and(|id| crate::hex::is_lower(id, STAGED_ID_BYTES * 2))
}

/// Removes the staged manifests earlier attempts left in `data_dir`, and nothing else. One that
/// cannot go yet is named in a warning and left for a later attempt, since this attempt stages
/// under a name of its own.
fn remove_stale_staged(data_dir: &Path) {
    let entries = match fs::read_dir(data_dir) {
        Ok(entries) => entries,
        Err(error) => {
            warning::unreadable(data_dir, &error);
            return;
        }
    };
    for entry in entries {
        let name = match entry {
            Ok(entry) => entry.file_name(),
            Err(error) => {
                warning::unreadable(data_dir, &error);
                return;
            }
        };
        if !name.to_str().is_some_and(is_staged) {
            continue;
        }
        let path = data_dir.join(name);
        if let Err(error) = objects::unlink(&path) {
            warning::staged_kept(&path, &error);
        }
    }
}

/// The staged manifest while it is written. Each record goes through one `put`, which the test
/// fault seam can fail; the buffer keeps the writes few.
struct Manifest<'a> {
    data_dir: &'a Path,
    staged: PathBuf,
    out: io::BufWriter<fs::File>,
}

impl<'a> Manifest<'a> {
    /// Creates the staged file under a new name, where nothing else exists, once the staged
    /// files earlier attempts left are gone. A link at an old staged name is removed rather
    /// than followed.
    fn create(data_dir: &'a Path) -> Result<Self, CatalogError> {
        remove_stale_staged(data_dir);
        let id = crate::storage::opaque_id::random::<STAGED_ID_BYTES>().map_err(unavailable)?;
        let staged = data_dir.join(format!(
            "{STAGED_PREFIX}{}{STAGED_SUFFIX}",
            crate::hashing::encode_lower(&id)
        ));
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .map_err(failed("create", &staged))?;
        #[cfg(all(test, unix))]
        fault::file_created(&staged);
        Ok(Self {
            data_dir,
            staged,
            out: io::BufWriter::new(file),
        })
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), CatalogError> {
        #[cfg(test)]
        fault::hit_io(self.data_dir, Step::RetiredManifestWrite)
            .map_err(failed("write", &self.staged))?;
        self.out
            .write_all(bytes)
            .map_err(failed("write", &self.staged))
    }

    /// Writes `key`, which needs no JSON escape, and one compact JSON object per row `query`
    /// selects, a line each, and returns how many it wrote.
    fn list<T: Serialize>(
        &mut self,
        connection: &Connection,
        key: &'static str,
        query: &'static str,
        mut read: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<u64, CatalogError> {
        self.put(format!(",\n  \"{key}\": [").as_bytes())?;
        let mut statement = connection.prepare_cached(query)?;
        let mut rows = statement.query([])?;
        let mut written = 0;
        while let Some(row) = rows.next()? {
            let separator: &[u8] = if written == 0 { b"\n    " } else { b",\n    " };
            let mut line = separator.to_vec();
            serde_json::to_writer(&mut line, &read(row)?).map_err(unavailable)?;
            self.put(&line)?;
            written += 1;
        }
        let close: &[u8] = if written == 0 { b"]" } else { b"\n  ]" };
        self.put(close)?;
        Ok(written)
    }

    fn publish(self) -> Result<PathBuf, CatalogError> {
        let Self {
            data_dir,
            staged,
            mut out,
        } = self;
        #[cfg(test)]
        fault::hit_io(data_dir, Step::RetiredManifestFlush).map_err(failed("flush", &staged))?;
        out.flush().map_err(failed("flush", &staged))?;
        let file = out
            .into_inner()
            .map_err(|error| failed("flush", &staged)(error.into_error()))?;
        #[cfg(test)]
        fault::hit_io(data_dir, Step::RetiredManifestSync).map_err(failed("sync", &staged))?;
        file.sync_all().map_err(failed("sync", &staged))?;
        let metadata = file.metadata().map_err(failed("sync", &staged))?;
        #[cfg(test)]
        fault::file_synced(data_dir, &metadata);
        drop(file);
        let manifest = place(data_dir, &staged, metadata.len())?;
        // Windows has no directory sync; like the blob store there, it relies on the rename.
        #[cfg(unix)]
        {
            #[cfg(test)]
            fault::hit_io(data_dir, Step::RetiredManifestDirSync)
                .map_err(failed("sync the directory", data_dir))?;
            objects::sync_dir(data_dir).map_err(failed("sync the directory", data_dir))?;
        }
        Ok(manifest)
    }
}

/// The name of the `number`th manifest: `sync-retired.json`, then `sync-retired.1.json` and on.
fn manifest_name(number: u32) -> String {
    if number == 0 {
        MANIFEST.to_owned()
    } else {
        format!("sync-retired.{number}.json")
    }
}

/// How a manifest name can take the staged manifest.
enum Destination {
    Free,
    /// It holds the staged bytes already: an earlier attempt published the manifest from the
    /// same rows and stopped before the rows went.
    Identical,
}

/// How many manifest names [`place`] tries: `sync-retired.json`, then `sync-retired.1.json`
/// through `sync-retired.9999.json`.
const MANIFEST_NAMES: u32 = 10_000;

/// Publishes the `len` staged bytes under the first manifest name in `data_dir` that is free or
/// holds exactly those bytes, and returns that name. A manifest with other content, such as one
/// an upgrade of a restored catalog wrote, is never replaced: the rename refuses a name that
/// something took after it was found free, and the search goes on to the next one.
fn place(data_dir: &Path, staged: &Path, len: u64) -> Result<PathBuf, CatalogError> {
    for number in 0..MANIFEST_NAMES {
        let candidate = data_dir.join(manifest_name(number));
        let Some(found) = compare(staged, len, &candidate)
            .map_err(failed("compare the staged file with", &candidate))?
        else {
            continue;
        };
        match found {
            Destination::Identical => {
                // The staged copy is surplus; one the platform cannot remove yet waits for a
                // later attempt instead of failing this one.
                if let Err(error) = objects::unlink(staged) {
                    warning::staged_kept(staged, &error);
                }
                return Ok(candidate);
            }
            Destination::Free => {
                #[cfg(test)]
                fault::hit_io(data_dir, Step::RetiredManifestRename)
                    .map_err(failed("rename the staged file to", &candidate))?;
                match rename_new(staged, &candidate) {
                    Ok(()) => {
                        #[cfg(all(test, unix))]
                        fault::renamed(staged, &candidate);
                        return Ok(candidate);
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => {
                        return Err(failed("rename the staged file to", &candidate)(error));
                    }
                }
            }
        }
    }
    Err(unavailable(format!(
        "{} and the numbered names after it up to sync-retired.{}.json in {} all hold other \
         manifests; move the ones you have checked aside so the retired synchronization records \
         can be listed",
        MANIFEST,
        MANIFEST_NAMES - 1,
        data_dir.display()
    )))
}

/// Renames `staged` to `manifest` unless something already has that name.
#[cfg(unix)]
fn rename_new(staged: &Path, manifest: &Path) -> io::Result<()> {
    crate::fsutil::rename_noreplace(staged, manifest)
}

/// Renames `staged` to `manifest` unless something already has that name; `MoveFileExW`
/// without `MOVEFILE_REPLACE_EXISTING` refuses an existing destination.
#[cfg(windows)]
fn rename_new(staged: &Path, manifest: &Path) -> io::Result<()> {
    crate::fsutil::move_write_through(staged, manifest)
}

/// Whether `candidate` can take the `len` staged bytes, or `None` while it holds anything else.
fn compare(staged: &Path, len: u64, candidate: &Path) -> io::Result<Option<Destination>> {
    let metadata = match fs::symlink_metadata(candidate) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Some(Destination::Free));
        }
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.len() != len {
        return Ok(None);
    }
    let mut ours = io::BufReader::new(fs::File::open(staged)?);
    let mut theirs = io::BufReader::new(fs::File::open(candidate)?);
    loop {
        let left = ours.fill_buf()?;
        let right = theirs.fill_buf()?;
        if left.is_empty() || right.is_empty() {
            return Ok((left.is_empty() && right.is_empty()).then_some(Destination::Identical));
        }
        let common = left.len().min(right.len());
        if left.iter().zip(right).any(|(left, right)| left != right) {
            return Ok(None);
        }
        ours.consume(common);
        theirs.consume(common);
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

/// A formerly synchronized folder, as the process that synchronized it saw its path.
#[derive(Clone, PartialEq, Eq, Serialize)]
struct Folder {
    folder: String,
    /// The path's bytes, when they are not UTF-8 and `folder` replaces some of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    folder_base64: Option<String>,
}

/// `sync_roots.location`: the folder's path in the bytes of the platform that synchronized it.
impl FromSql for Folder {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let location = value.as_blob()?;
        Ok(match std::str::from_utf8(location) {
            Ok(folder) => Self {
                folder: folder.to_owned(),
                folder_base64: None,
            },
            Err(_) => Self {
                folder: String::from_utf8_lossy(location).into_owned(),
                folder_base64: Some(crate::base64::encode(location)),
            },
        })
    }
}

#[derive(Serialize)]
struct ConflictCopy {
    /// Relative to the data directory, with `/` on every platform.
    path: String,
    #[serde(flatten)]
    folder: Folder,
    /// Where the edited file was, relative to the folder, with `/` on every platform.
    target: String,
    bucket: String,
    bucket_incarnation: String,
    object: String,
    /// The object's generation the copy was taken against; absent when there was no object.
    #[serde(skip_serializing_if = "Option::is_none")]
    generation: Option<i64>,
    size: u64,
    /// Absent while the copy was not yet written in full.
    #[serde(flatten)]
    digests: Option<Digests>,
    state: CopyState,
}

/// What the manifest says about a copy's digests.
#[derive(Serialize)]
#[serde(untagged)]
enum Digests {
    Known {
        md5: String,
        crc32c: String,
    },
    /// `retained` holds something no release wrote. The copy is listed all the same, because
    /// its file matters more than the digests.
    Unreadable {
        digests: Unreadable,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Unreadable {
    Unreadable,
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

impl Digests {
    /// The digests a `retained` value records: none for `NULL` or an observation without them.
    fn from_retained(value: ValueRef<'_>) -> Option<Self> {
        let bytes = match value {
            ValueRef::Null => return None,
            ValueRef::Blob(bytes) | ValueRef::Text(bytes) => bytes,
            ValueRef::Integer(_) | ValueRef::Real(_) => return Some(Self::unreadable()),
        };
        match serde_json::from_slice::<Retained>(bytes) {
            Ok(retained) => retained.hashes.map(|hashes| Self::Known {
                md5: crate::base64::encode(&hashes.md5),
                crc32c: crate::base64::encode(&hashes.crc32c.to_be_bytes()),
            }),
            Err(_) => Some(Self::unreadable()),
        }
    }

    fn unreadable() -> Self {
        Self::Unreadable {
            digests: Unreadable::Unreadable,
        }
    }
}

fn conflict_copy(row: &Row<'_>) -> rusqlite::Result<ConflictCopy> {
    let id: String = row.get(1)?;
    Ok(ConflictCopy {
        path: format!("{CONFLICTS}/{id}"),
        folder: row.get(0)?,
        target: row.get(2)?,
        bucket: row.get(3)?,
        bucket_incarnation: row.get(4)?,
        object: row.get(5)?,
        generation: row.get(6)?,
        size: row.get(7)?,
        digests: Digests::from_retained(row.get_ref(8)?),
        state: row.get(9)?,
    })
}

#[derive(Serialize)]
struct WorkspaceEntry {
    #[serde(flatten)]
    folder: Folder,
    path: String,
    /// Absent for a recovery whose write-back row was gone.
    #[serde(flatten)]
    operation: Option<Operation>,
}

/// The write-back that left a workspace entry.
#[derive(Serialize)]
struct Operation {
    /// Where the file was, relative to the folder, with `/` on every platform.
    target: String,
    bucket: String,
    bucket_incarnation: String,
    object: String,
}

fn workspace_entry(row: &Row<'_>) -> rusqlite::Result<WorkspaceEntry> {
    let folder: Folder = row.get(0)?;
    let id: String = row.get(1)?;
    // A folder path that is not UTF-8 keeps its replacement characters here; `folder_base64`
    // has its bytes.
    let path = Path::new(&folder.folder)
        .join(WORKSPACE)
        .join(id)
        .display()
        .to_string();
    // The write-back's columns are all set or, without its row, all `NULL`.
    let operation = row
        .get::<_, Option<String>>(2)?
        .map(|target| {
            Ok::<_, rusqlite::Error>(Operation {
                target,
                bucket: row.get(3)?,
                bucket_incarnation: row.get(4)?,
                object: row.get(5)?,
            })
        })
        .transpose()?;
    Ok(WorkspaceEntry {
        folder,
        path,
        operation,
    })
}
