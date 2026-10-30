//! Test-only storage fault injection for the durable write paths.
//!
//! Write paths call [`hit`] at their durable step boundaries. A test arms a fault for one
//! catalog's data directory; when that catalog reaches the step, the fault either fails it
//! with a chosen error or snapshots the data directory, so the test can reopen the snapshot
//! as the state a process abort or a power loss at that boundary would leave. Faults are
//! keyed by data directory because tests run in parallel, each on its own catalog. The
//! module and every call site are compiled only under `cfg(test)`, so production builds
//! carry no hook.
//!
//! A process abort keeps everything the kernel has accepted, so a [`Fault::Snapshot`]
//! passes even when a write path forgets an fsync. A [`Fault::PowerLoss`] snapshot keeps
//! only what the seamed sync points made durable, so a missing fsync on those paths fails
//! the crash test that covers it. It is a model, not a filesystem simulator:
//!
//! - File contents: a file is identified by device and inode, so the record follows it
//!   through renames. Files that existed when the fault was armed count as durable at their
//!   length then; [`file_synced`] records a file's current length as durable. The snapshot
//!   truncates every other copy to its durable length, or to empty when none was recorded.
//!   Writes are modeled as appends: a rewrite in place below the durable length survives.
//! - Directory entries: [`dir_created`] and [`renamed`] record entries that stay volatile
//!   until [`dir_synced`] syncs their parent. The snapshot undoes them, newest first: a
//!   volatile rename moves the file back to its old name and a volatile directory vanishes
//!   with its contents. A new file's own entry, an unlink and entries made by paths outside
//!   the seam are not modeled; they survive as they are on disk.
//! - SQLite files keep their on-disk state: SQLite syncs them itself, and its WAL commit
//!   protocol is not what these tests check.
//! - Unix only, because directory fsync is the unix publication protocol.

use crate::storage::failure::{BackendCause, Classified, StorageCause};
#[cfg(unix)]
use std::collections::HashMap;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

/// A durable step boundary on a blob write path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Before one write of body bytes to the staged file.
    BodyWrite,
    /// Before the flush that reports tokio's background tail write.
    BodyFlush,
    /// Before the staged file's `sync_all`.
    BodySync,
    /// After the staged file is durable, before it is renamed into `blobs/`.
    BeforePublish,
    /// After the rename and shard sync, before the catalog transaction.
    AfterPublish,
    /// Before the publication transaction commits.
    BeforeCommit,
    /// After the publication transaction committed.
    AfterCommit,
    /// Before one write of appended bytes to an appendable data file.
    AppendWrite,
    /// Before the appendable data file's `sync_data` on a flush.
    AppendSync,
    /// After the appended bytes are durable, before the transaction that publishes their size.
    AppendCommit,
}

/// What an armed step does when it is reached.
#[derive(Clone, Debug)]
pub(crate) enum Fault {
    /// Fails the step with an I/O error of this kind, as the filesystem would.
    Io(io::ErrorKind),
    /// Fails the step with the error SQLite reports for a corrupt database.
    Corrupt,
    /// Fails the step with the error SQLite reports when another writer holds the lock.
    Busy,
    /// Copies the data directory to this path, as a process abort leaves it, and lets the
    /// step continue.
    Snapshot(PathBuf),
    /// Copies the data directory to this path as a power loss leaves it: only what the
    /// seamed sync points made durable since arming survives. The step continues.
    #[cfg(unix)]
    PowerLoss(PathBuf),
}

struct Armed {
    data_dir: PathBuf,
    step: Step,
    skip: usize,
    fault: Fault,
}

static ARMED: Mutex<Vec<Armed>> = Mutex::new(Vec::new());

fn armed() -> std::sync::MutexGuard<'static, Vec<Armed>> {
    // A test that panicked while holding the lock leaves only plain data behind.
    ARMED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The same directory may be reached through a symlinked temporary path.
fn key(data_dir: &Path) -> PathBuf {
    data_dir
        .canonicalize()
        .unwrap_or_else(|_| data_dir.to_owned())
}

/// Arms `fault` for the `skip + 1`-th time the catalog in `data_dir` reaches `step`; it fires
/// once.
pub(crate) fn arm(data_dir: &Path, step: Step, skip: usize, fault: Fault) {
    let data_dir = key(data_dir);
    #[cfg(unix)]
    if matches!(fault, Fault::PowerLoss(_)) {
        let ledger = Ledger::baseline(&data_dir)
            .unwrap_or_else(|error| panic!("power-loss baseline failed: {error}"));
        ledgers().push((data_dir.clone(), ledger));
    }
    armed().push(Armed {
        data_dir,
        step,
        skip,
        fault,
    });
}

/// The number of faults armed for `data_dir` that have not fired yet.
pub(crate) fn pending(data_dir: &Path) -> usize {
    let data_dir = key(data_dir);
    armed()
        .iter()
        .filter(|armed| armed.data_dir == data_dir)
        .count()
}

/// The data directory of a catalog connection: the parent of its database file.
pub(crate) fn database_dir(connection: &rusqlite::Connection) -> PathBuf {
    connection
        .path()
        .and_then(|path| Path::new(path).parent())
        .map(Path::to_owned)
        .unwrap_or_default()
}

/// Returns the armed failure unclassified, so each call site maps it with the same
/// function as the real operation beside it and a test observes that site's mapping.
pub(crate) fn hit(data_dir: &Path, step: Step) -> Result<(), Injected> {
    let fired = {
        let mut armed = armed();
        if armed.is_empty() {
            return Ok(());
        }
        let data_dir = key(data_dir);
        let Some(index) = armed
            .iter()
            .position(|armed| armed.step == step && armed.data_dir == data_dir)
        else {
            return Ok(());
        };
        if armed[index].skip > 0 {
            armed[index].skip -= 1;
            return Ok(());
        }
        armed.remove(index)
    };
    match fired.fault {
        Fault::Io(kind) => Err(Injected::Io(io::Error::new(
            kind,
            format!("injected {step:?} failure"),
        ))),
        Fault::Corrupt => Err(Injected::Sqlite(sqlite(rusqlite::ffi::SQLITE_CORRUPT))),
        Fault::Busy => Err(Injected::Sqlite(sqlite(rusqlite::ffi::SQLITE_BUSY))),
        Fault::Snapshot(target) => {
            // A snapshot that cannot be taken must fail the test, not pass as a fault.
            if let Err(error) = copy_tree(&fired.data_dir, &target) {
                panic!("crash snapshot at {step:?} failed: {error}");
            }
            Ok(())
        }
        #[cfg(unix)]
        Fault::PowerLoss(target) => {
            let ledger = {
                let mut ledgers = ledgers();
                let index = ledgers
                    .iter()
                    .position(|(dir, _)| *dir == fired.data_dir)
                    .unwrap_or_else(|| panic!("power-loss ledger missing at {step:?}"));
                ledgers.remove(index).1
            };
            if let Err(error) = ledger.snapshot(&fired.data_dir, &target) {
                panic!("power-loss snapshot at {step:?} failed: {error}");
            }
            Ok(())
        }
    }
}

/// What a power loss would keep of one data directory, recorded while a
/// [`Fault::PowerLoss`] is armed for it.
#[cfg(unix)]
struct Ledger {
    /// Durable length per file, keyed by device and inode.
    durable: HashMap<(u64, u64), u64>,
    /// Directory entries whose parent has not been synced since, oldest first.
    volatile: Vec<Entry>,
}

#[cfg(unix)]
enum Entry {
    Created(PathBuf),
    Renamed { from: PathBuf, to: PathBuf },
}

#[cfg(unix)]
impl Entry {
    /// The directory whose sync makes this entry durable.
    fn parent(&self) -> Option<&Path> {
        match self {
            Self::Created(path) | Self::Renamed { to: path, .. } => path.parent(),
        }
    }
}

#[cfg(unix)]
static LEDGERS: Mutex<Vec<(PathBuf, Ledger)>> = Mutex::new(Vec::new());

#[cfg(unix)]
fn ledgers() -> std::sync::MutexGuard<'static, Vec<(PathBuf, Ledger)>> {
    LEDGERS.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(unix)]
fn inode(metadata: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

/// SQLite keeps its own durability; see the module documentation.
#[cfg(unix)]
fn sqlite_file(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy().starts_with("catalog.sqlite3")
}

/// Resolves `path` like [`key`], through its parent when the path itself is gone.
#[cfg(unix)]
fn resolve(path: &Path) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => key(parent).join(name),
        _ => path.to_owned(),
    }
}

/// Runs `record` on the ledger of the data directory that contains `path`, if one is armed.
#[cfg(unix)]
fn record(path: &Path, apply: impl FnOnce(&mut Ledger, PathBuf)) {
    let mut ledgers = ledgers();
    if ledgers.is_empty() {
        return;
    }
    let path = resolve(path);
    if let Some((_, ledger)) = ledgers.iter_mut().find(|(dir, _)| path.starts_with(dir)) {
        apply(ledger, path);
    }
}

/// Records that `file` in `data_dir` is durable at its current length, after its sync.
pub(crate) fn file_synced(data_dir: &Path, metadata: &fs::Metadata) {
    #[cfg(unix)]
    {
        let mut ledgers = ledgers();
        if ledgers.is_empty() {
            return;
        }
        let data_dir = key(data_dir);
        if let Some((_, ledger)) = ledgers.iter_mut().find(|(dir, _)| *dir == data_dir) {
            ledger.durable.insert(inode(metadata), metadata.len());
        }
    }
    #[cfg(not(unix))]
    let _ = (data_dir, metadata);
}

/// Records a new directory, durable only once its parent is synced.
#[cfg(unix)]
pub(crate) fn dir_created(path: &Path) {
    record(path, |ledger, path| {
        ledger.volatile.push(Entry::Created(path));
    });
}

/// Records a rename, durable only once the destination's directory is synced.
#[cfg(unix)]
pub(crate) fn renamed(from: &Path, to: &Path) {
    let from = resolve(from);
    record(to, |ledger, to| {
        ledger.volatile.push(Entry::Renamed { from, to });
    });
}

/// Records a directory sync, which makes the entries made in it durable.
#[cfg(unix)]
pub(crate) fn dir_synced(path: &Path) {
    record(path, |ledger, path| {
        ledger
            .volatile
            .retain(|entry| entry.parent() != Some(path.as_path()));
    });
}

#[cfg(unix)]
impl Ledger {
    /// Treats every file already in `data_dir` as durable at its current length.
    fn baseline(data_dir: &Path) -> io::Result<Self> {
        let mut ledger = Self {
            durable: HashMap::new(),
            volatile: Vec::new(),
        };
        let mut pending = vec![data_dir.to_owned()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                let metadata = entry.metadata()?;
                if metadata.is_dir() {
                    pending.push(entry.path());
                } else if metadata.is_file() {
                    ledger.durable.insert(inode(&metadata), metadata.len());
                }
            }
        }
        Ok(ledger)
    }

    fn snapshot(&self, data_dir: &Path, target: &Path) -> io::Result<()> {
        copy_tree(data_dir, target)?;
        self.truncate(data_dir, target)?;
        let relocate = |path: &Path| -> io::Result<PathBuf> {
            path.strip_prefix(data_dir)
                .map(|relative| target.join(relative))
                .map_err(|_| io::Error::other("volatile entry outside the data directory"))
        };
        for entry in self.volatile.iter().rev() {
            match entry {
                Entry::Created(path) => {
                    let path = relocate(path)?;
                    if path.exists() {
                        fs::remove_dir_all(path)?;
                    }
                }
                Entry::Renamed { from, to } => {
                    let to = relocate(to)?;
                    if to.exists() {
                        fs::rename(to, relocate(from)?)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Cuts each copied file in `target` back to the durable length of its source.
    fn truncate(&self, source: &Path, target: &Path) -> io::Result<()> {
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let name = entry.file_name();
            let path = target.join(&name);
            let metadata = entry.metadata()?;
            if metadata.is_dir() {
                self.truncate(&entry.path(), &path)?;
            } else if metadata.is_file() && !sqlite_file(&name) && path.exists() {
                let durable = self.durable.get(&inode(&metadata)).copied().unwrap_or(0);
                if durable < metadata.len() {
                    fs::OpenOptions::new()
                        .write(true)
                        .open(path)?
                        .set_len(durable)?;
                }
            }
        }
        Ok(())
    }
}

/// An injected failure, carrying the error type the real step would have produced.
#[derive(Debug)]
pub(crate) enum Injected {
    Io(io::Error),
    Sqlite(rusqlite::Error),
}

impl StorageCause for Injected {
    fn into_cause(self) -> BackendCause {
        match self {
            Self::Io(error) => error.into_cause(),
            Self::Sqlite(error) => error.into_cause(),
        }
    }

    fn classify(self) -> Classified {
        match self {
            Self::Io(error) => error.classify(),
            Self::Sqlite(error) => error.classify(),
        }
    }
}

fn sqlite(code: std::ffi::c_int) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
}

/// Copies what survives a process abort: files and directories, without the lock file or
/// SQLite's shared-memory index, which a new process rebuilds from the WAL.
fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "catalog.lock" || name.to_string_lossy().ends_with("-shm") {
            continue;
        }
        let path = target.join(&name);
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &path)?;
        } else {
            fs::copy(entry.path(), path)?;
        }
    }
    Ok(())
}
