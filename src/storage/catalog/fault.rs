//! Test-only storage fault injection for the durable write paths.
//!
//! Write paths call [`hit`] at their durable step boundaries. A test arms a fault for one
//! catalog's data directory; when that catalog reaches the step, the fault either fails it
//! with a chosen error or snapshots the data directory, so the test can reopen the snapshot
//! as the state a process abort at that boundary would leave. Faults are keyed by data
//! directory because tests run in parallel, each on its own catalog. The module and every
//! call site are compiled only under `cfg(test)`, so production builds carry no hook.

use crate::storage::failure::{BackendCause, Classified, StorageCause};
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
    /// Copies the data directory to this path and lets the step continue.
    Snapshot(PathBuf),
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
    armed().push(Armed {
        data_dir: key(data_dir),
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
