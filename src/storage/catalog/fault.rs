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
//!   length then; [`file_synced`] records a file's current length as durable, and
//!   [`file_synced_at`] does so for a seam that knows the file only by an open descriptor. The snapshot
//!   truncates every other copy to its durable length, or to empty when none was recorded.
//!   Writes are modeled as appends: a rewrite in place below the durable length survives.
//!   [`truncated`] lowers the durable length when a seamed path cuts a file back, so bytes
//!   written again past the cut stay volatile until the next recorded sync. Whether the cut
//!   itself survives is not modeled: the snapshot cannot bring back bytes already gone.
//! - Directory entries: [`dir_created`], [`file_created`], [`unlinked`], [`dir_removed`] and
//!   [`renamed`] record entries that stay volatile until [`dir_synced`] syncs their parent. The
//!   snapshot undoes them, newest first: a volatile rename moves the file back to its old name, a
//!   volatile new entry vanishes, a volatile unlink restores bytes from its retained test
//!   descriptor, and a volatile directory removal creates the empty directory again. Only an
//!   empty directory can be removed, so nothing inside it needs restoring; the snapshot fails if
//!   the directory's name is taken again, because skipping that undo would invent a state no
//!   power loss leaves. Entries made by paths outside these seams are not modeled; they survive
//!   as they are on disk.
//! - Renames that change two directory entries: [`exchanged`] records two names that swapped
//!   their files, and [`moved`] a file that moved to a new name in another directory. Either
//!   stays volatile until both directories are synced: no single directory sync is specified to
//!   make a change to two directories durable. The model assumes the rename itself is atomic, so
//!   a power loss keeps the change whole or undoes it whole, never half of it. The snapshot
//!   undoes it the same way, exchanging the two names again or moving the file back without
//!   replacing anything, and fails if either name is missing or the old name is taken again:
//!   skipping that undo would invent a state no power loss leaves.
//! - SQLite files keep their on-disk state: SQLite syncs them itself, and its WAL commit
//!   protocol is not what these tests check.
//! - Unix only, because directory fsync is the unix publication protocol.

use crate::storage::failure::{BackendCause, Classified, StorageCause};
use std::{
    cell::Cell,
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};
#[cfg(unix)]
use std::{collections::HashMap, os::unix::fs::PermissionsExt};

#[cfg(unix)]
mod tests;

/// A durable step boundary on a blob write path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Before probing a failed or rolled-back publication's reference.
    DiscardProbe,
    /// Before committing an expired soft-deleted batch and its deletion journal rows together.
    ExpiryCommit,
    /// Before committing lifecycle retirement, notification and deletion journal together.
    LifecycleCommit,
    /// After lifecycle retirement committed, before the writer returns its counts.
    LifecycleCommitted,
    /// Before unlinking a journaled blob.
    ReclaimUnlink,
    /// After unlinking a shard's blobs, before its directory sync.
    #[cfg(unix)]
    ReclaimSync,
    /// After durable removal, before committing journal removal.
    ReclaimForget,
    /// After publishing a restore's pinned bytes, before its metadata transaction.
    RestorePinned,
    /// Before removing an unretained staging entry during startup recovery.
    RecoveryStagingUnlink,
    /// After a durable replacement between directories renamed, exchanged or moved the target
    /// and the parent syncs inside that call ran. Only the `fsutil` test of that call hits it.
    #[cfg_attr(not(unix), expect(dead_code, reason = "the seed watcher reuses this"))]
    SyncCopyRenamed,
    /// Before reclaiming one namespace entry after a committed reset.
    ResetReclaim,
    /// Before syncing a directory after a committed reset's unlink.
    ResetReclaimSync,
    /// Before reclaiming a retired XML multipart staging entry.
    MultipartReclaim,
    /// Before syncing the multipart staging directory after reclamation (Unix only, like the sync).
    #[cfg(unix)]
    MultipartReclaimSync,
    /// After durable multipart removal, before deleting its journal rows.
    MultipartReclaimForget,
    /// Before restoring an active resumable upload's published file to staging.
    ResumableRecoveryRename,
    #[cfg(unix)]
    ResumableRecoveryShardSync,
    #[cfg(unix)]
    ResumableRecoveryStagingSync,
    ResumableRecoveryTruncate,
    ResumableRecoverySync,
    /// After the acknowledged resumable prefix and its staging entry are durable again.
    ResumableRecovered,
    /// Before one write of body bytes to the staged file.
    BodyWrite,
    /// Before a staged file sends the bytes written since its last sync to storage.
    BodyWriteBehind,
    /// Before the flush that reports tokio's background tail write.
    BodyFlush,
    /// Before the staged file's `sync_all`.
    BodySync,
    /// After a resumable chunk is durable, before the transaction that records its offset.
    Checkpoint,
    /// Before committing a terminal resumable error and its cleanup journal together.
    SessionFailureCommit,
    /// After committing a terminal resumable error, before reclaiming staged bytes.
    SessionFailureCommitted,
    /// The session row must not commit if its staging entry cannot be synced.
    #[cfg(unix)]
    SessionCreateSync,
    /// Acknowledging a session requires a durable staging entry.
    SessionCreated,
    /// Keep the cleanup journal until directory removal is durable.
    #[cfg(unix)]
    SessionCleanupSync,
    /// A removed entry must not reappear after its directory sync.
    #[cfg(unix)]
    SessionCleanupSynced,
    /// Before syncing an admin cleanup's blob-directory unlink.
    #[cfg(unix)]
    AdminCleanupSync,
    /// After the staged file is durable, before it is renamed into `blobs/`.
    BeforePublish,
    /// After the rename and shard sync, before the catalog transaction.
    AfterPublish,
    /// While one upload of a group commit publishes under its own savepoint.
    PublishUpload,
    /// Before the publication transaction commits.
    BeforeCommit,
    /// After the publication transaction committed.
    AfterCommit,
    /// After a derived draft is created, before its staging directory is synced.
    DerivedDraftSync,
    /// Before committing the derived transfer's durable draft and input references.
    DerivedBeginCommit,
    /// Before committing removal of a derived transfer and its input references.
    DerivedCleanupCommit,
    /// After discarding the derived transfer's references, before releasing staged data.
    DerivedCleanup,
    /// After releasing the discarded transfer's staging entries.
    DerivedCleaned,
    /// Before one write of appended bytes to an appendable data file.
    AppendWrite,
    /// Before releasing data no committed appendable row references.
    AppendCleanup,
    /// Before the appendable data file's `sync_data` on a flush.
    AppendSync,
    /// After the appended bytes are durable, before the transaction that publishes their size.
    AppendCommit,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[cfg_attr(
    target_os = "macos",
    expect(
        unsafe_code,
        reason = "test fault accounting resolves the pinned directory descriptor without changing production I/O"
    )
)]
pub(crate) fn descriptor_path(file: &fs::File) -> io::Result<PathBuf> {
    use std::os::fd::AsRawFd;
    #[cfg(target_os = "linux")]
    {
        fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut bytes = [0u8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH writes at most PATH_MAX bytes into the live output allocation.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, bytes.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let path = std::ffi::CStr::from_bytes_until_nul(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
    }
}

/// What an armed step does when it is reached.
#[derive(Clone, Debug)]
pub(crate) enum Fault {
    /// Holds an owned job at a durable boundary while its caller is cancelled.
    Block {
        entered: std::sync::Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
        release: std::sync::Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
    },
    /// Fails the step with an I/O error of this kind, as the filesystem would.
    Io(io::ErrorKind),
    /// Fails the step with the error SQLite reports for a corrupt database.
    Corrupt,
    /// Fails the step with the error SQLite reports when another writer holds the lock.
    Busy,
    /// Fails the step with an I/O error after rolling back the whole open transaction, as SQLite
    /// may after an I/O, full-disk or out-of-memory error. Only a step reached through
    /// [`hit_transaction`] has a transaction to roll back; any other step panics, so a test
    /// that arms it in the wrong place cannot pass on a plain I/O error.
    RollBack,
    /// Panics at the step, as a bug in the code there would.
    Panic,
    /// Copies the data directory to this path, as a process abort leaves it, and lets the
    /// step continue.
    Snapshot(PathBuf),
    /// Copies the data directory to this path as a power loss leaves it: only what the
    /// seamed sync points made durable since arming survives. The step continues.
    #[cfg(unix)]
    PowerLoss(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Crash {
    Abort,
    #[cfg(unix)]
    PowerLoss,
}

impl Crash {
    pub(crate) const ALL: &[Self] = &[
        Self::Abort,
        #[cfg(unix)]
        Self::PowerLoss,
    ];

    pub(crate) fn fault(self, path: PathBuf) -> Fault {
        match self {
            Self::Abort => Fault::Snapshot(path),
            #[cfg(unix)]
            Self::PowerLoss => Fault::PowerLoss(path),
        }
    }
}

struct Armed {
    data_dir: PathBuf,
    step: Step,
    skip: usize,
    fault: Fault,
    #[cfg(unix)]
    ledger: Option<Ledger>,
}

static ARMED: Mutex<Vec<Armed>> = Mutex::new(Vec::new());

thread_local! {
    static ON_CATALOG_THREAD: Cell<bool> = const { Cell::new(false) };
}

static CATALOG_IO: Mutex<Vec<(PathBuf, usize)>> = Mutex::new(Vec::new());

pub(crate) fn enter_catalog_thread() {
    ON_CATALOG_THREAD.set(true);
}

pub(crate) fn record_catalog_thread_io(path: &Path) {
    if !ON_CATALOG_THREAD.get() {
        return;
    }
    let Some(root) = path
        .ancestors()
        .find(|root| root.join("catalog.sqlite3").is_file())
    else {
        return;
    };
    let root = key(root);
    let mut counts = CATALOG_IO.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, count)) = counts.iter_mut().find(|(entry, _)| entry == &root) {
        *count += 1;
    } else {
        counts.push((root, 1));
    }
}

pub(crate) fn catalog_thread_io(root: &Path) -> usize {
    let root = key(root);
    CATALOG_IO
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|(entry, _)| entry == &root)
        .map_or(0, |(_, count)| *count)
}

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
    let ledger = matches!(fault, Fault::PowerLoss(_)).then(|| {
        Ledger::baseline(&data_dir)
            .unwrap_or_else(|error| panic!("power-loss baseline failed: {error}"))
    });
    armed().push(Armed {
        data_dir,
        step,
        skip,
        fault,
        #[cfg(unix)]
        ledger,
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
        .map_or_default(Path::to_owned)
}

/// Returns the armed failure unclassified, so each call site maps it with the same
/// function as the real operation beside it and a test observes that site's mapping.
pub(crate) fn hit(data_dir: &Path, step: Step) -> Result<(), Injected> {
    take(data_dir, step).map_or(Ok(()), |fired| fire(fired, step))
}

/// [`hit`] at a step inside the open transaction of `connection`, where a [`Fault::RollBack`]
/// rolls that transaction back before it fails the step.
pub(crate) fn hit_transaction(
    connection: &rusqlite::Connection,
    step: Step,
) -> Result<(), Injected> {
    let Some(fired) = take(&database_dir(connection), step) else {
        return Ok(());
    };
    let Fault::RollBack = fired.fault else {
        return fire(fired, step);
    };
    // A rollback that failed leaves the transaction open, and the test sees that instead.
    connection
        .execute_batch("ROLLBACK")
        .map_err(Injected::Sqlite)?;
    // A failed statement carries SQLite's message, which the log allowlist reads.
    Err(Injected::Sqlite(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
        Some("disk I/O error".into()),
    )))
}

/// Removes and returns the fault armed for this reach of `step`, counting down its skips.
fn take(data_dir: &Path, step: Step) -> Option<Armed> {
    let mut armed = armed();
    if armed.is_empty() {
        return None;
    }
    let data_dir = key(data_dir);
    let index = armed
        .iter()
        .position(|armed| armed.step == step && armed.data_dir == data_dir)?;
    if armed[index].skip > 0 {
        armed[index].skip -= 1;
        return None;
    }
    Some(armed.remove(index))
}

fn fire(fired: Armed, step: Step) -> Result<(), Injected> {
    match fired.fault {
        Fault::Block { entered, release } => {
            if let Some(entered) = entered
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take()
            {
                let _ = entered.send(());
            }
            release
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .recv_timeout(std::time::Duration::from_secs(5))
                .map_err(|error| Injected::Io(io::Error::other(error)))
        }
        Fault::Io(kind) => Err(Injected::Io(io::Error::new(
            kind,
            format!("injected {step:?} failure"),
        ))),
        Fault::Corrupt => Err(Injected::Sqlite(sqlite(rusqlite::ffi::SQLITE_CORRUPT))),
        Fault::Busy => Err(Injected::Sqlite(sqlite(rusqlite::ffi::SQLITE_BUSY))),
        Fault::RollBack => panic!("{step:?} holds no transaction to roll back"),
        Fault::Panic => panic!("injected {step:?} panic"),
        Fault::Snapshot(target) => {
            // A snapshot that cannot be taken must fail the test, not pass as a fault.
            if let Err(error) = copy_tree(&fired.data_dir, &target) {
                panic!("crash snapshot at {step:?} failed: {error}");
            }
            Ok(())
        }
        #[cfg(unix)]
        Fault::PowerLoss(target) => {
            let ledger = fired
                .ledger
                .unwrap_or_else(|| panic!("power-loss ledger missing at {step:?}"));
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
    /// Directory entries that a directory sync has yet to make durable, oldest first.
    volatile: Vec<Volatile>,
}

#[cfg(unix)]
enum Entry {
    Created(PathBuf),
    CreatedFile(PathBuf),
    Unlinked { path: PathBuf, file: fs::File },
    RemovedDirectory(PathBuf),
    Renamed { from: PathBuf, to: PathBuf },
    Exchanged { left: PathBuf, right: PathBuf },
    Moved { from: PathBuf, to: PathBuf },
}

#[cfg(unix)]
impl Entry {
    /// The directories whose syncs together make this entry durable.
    fn parents(&self) -> Vec<PathBuf> {
        let mut parents: Vec<PathBuf> = match self {
            Self::Created(path)
            | Self::CreatedFile(path)
            | Self::Unlinked { path, .. }
            | Self::RemovedDirectory(path)
            | Self::Renamed { to: path, .. } => vec![path],
            Self::Exchanged {
                left: first,
                right: second,
            }
            | Self::Moved {
                from: first,
                to: second,
            } => vec![first, second],
        }
        .into_iter()
        .filter_map(|path| path.parent().map(Path::to_owned))
        .collect();
        parents.dedup();
        parents
    }
}

/// A directory entry that a power loss would undo, with the directories still to be synced.
#[cfg(unix)]
struct Volatile {
    entry: Entry,
    unsynced: Vec<PathBuf>,
}

#[cfg(unix)]
impl Volatile {
    fn new(entry: Entry) -> Self {
        Self {
            unsynced: entry.parents(),
            entry,
        }
    }

    /// Counts the sync of `synced`; whether the entry still awaits the sync of another directory.
    fn volatile_after(&mut self, synced: &Path) -> bool {
        self.unsynced.retain(|parent| parent != synced);
        !self.unsynced.is_empty()
    }
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

/// Whether any entry, a dangling symlink included, has the name `path`.
#[cfg(unix)]
fn present(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
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

/// Every overlapping arm keeps its own baseline and receives each subsequent seam update.
#[cfg(unix)]
fn record(path: &Path, mut apply: impl FnMut(&mut Ledger, PathBuf)) {
    let mut armed = armed();
    if armed.is_empty() {
        return;
    }
    let path = resolve(path);
    for fault in armed
        .iter_mut()
        .filter(|fault| path.starts_with(&fault.data_dir))
    {
        if let Some(ledger) = &mut fault.ledger {
            apply(ledger, path.clone());
        }
    }
}

/// Records that `file` in `data_dir` is durable at its current length, after its sync.
pub(crate) fn file_synced(data_dir: &Path, metadata: &fs::Metadata) {
    #[cfg(unix)]
    {
        let mut armed = armed();
        if armed.is_empty() {
            return;
        }
        let data_dir = key(data_dir);
        for fault in armed.iter_mut().filter(|fault| fault.data_dir == data_dir) {
            if let Some(ledger) = &mut fault.ledger {
                ledger.durable.insert(inode(metadata), metadata.len());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (data_dir, metadata);
}

/// Like [`file_synced`], for the file at `path` in whichever armed data directory holds it: a
/// seam that sees only an open descriptor resolves its path instead of naming a data directory.
#[cfg(unix)]
pub(crate) fn file_synced_at(path: &Path, metadata: &fs::Metadata) {
    record(path, |ledger, _| {
        ledger.durable.insert(inode(metadata), metadata.len());
    });
}

/// Records that a file in `data_dir` was cut back to its current length. Bytes written past
/// the cut replace bytes that may have been durable, so they count as durable again only
/// after a later [`file_synced`].
pub(crate) fn truncated(data_dir: &Path, metadata: &fs::Metadata) {
    #[cfg(unix)]
    {
        let mut armed = armed();
        if armed.is_empty() {
            return;
        }
        let data_dir = key(data_dir);
        for fault in armed.iter_mut().filter(|fault| fault.data_dir == data_dir) {
            if let Some(ledger) = &mut fault.ledger
                && let Some(durable) = ledger.durable.get_mut(&inode(metadata))
            {
                *durable = (*durable).min(metadata.len());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = (data_dir, metadata);
}

/// Records a new directory, durable only once its parent is synced.
#[cfg(unix)]
pub(crate) fn dir_created(path: &Path) {
    record(path, |ledger, path| {
        ledger.volatile.push(Volatile::new(Entry::Created(path)));
    });
}

/// Records a new file directory entry, volatile until its parent is synced; content durability is tracked separately.
#[cfg(unix)]
pub(crate) fn file_created(path: &Path) {
    record(path, |ledger, path| {
        ledger
            .volatile
            .push(Volatile::new(Entry::CreatedFile(path)));
    });
}

/// Retains the removed inode only in the test model until the parent syncs.
#[cfg(unix)]
pub(crate) fn retain_before_unlink(path: &Path) -> io::Result<Option<fs::File>> {
    let path = resolve(path);
    let pending = armed()
        .iter()
        .any(|fault| path.starts_with(&fault.data_dir));
    if !pending {
        return Ok(None);
    }
    match fs::File::open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Records the removed inode until the parent directory is synced.
#[cfg(unix)]
pub(crate) fn unlinked(path: &Path, file: &fs::File) -> io::Result<()> {
    let path = resolve(path);
    let mut armed = armed();
    for fault in armed
        .iter_mut()
        .filter(|fault| path.starts_with(&fault.data_dir))
    {
        if let Some(ledger) = &mut fault.ledger {
            ledger.volatile.push(Volatile::new(Entry::Unlinked {
                path: path.clone(),
                file: file.try_clone()?,
            }));
        }
    }
    Ok(())
}

/// Records that the empty directory at `path` was removed, durable only once its parent is synced.
#[cfg(unix)]
pub(crate) fn dir_removed(path: &Path) {
    record(path, |ledger, path| {
        ledger
            .volatile
            .push(Volatile::new(Entry::RemovedDirectory(path)));
    });
}

/// Records a rename, durable only once the destination's directory is synced.
#[cfg(unix)]
pub(crate) fn renamed(from: &Path, to: &Path) {
    let from = resolve(from);
    record(to, |ledger, to| {
        ledger.volatile.push(Volatile::new(Entry::Renamed {
            from: from.clone(),
            to,
        }));
    });
}

/// Records that the files at `left` and `right` swapped names in one atomic exchange, durable
/// only once both of their directories are synced.
#[cfg(unix)]
pub(crate) fn exchanged(left: &Path, right: &Path) {
    let left = resolve(left);
    record(right, |ledger, right| {
        ledger.volatile.push(Volatile::new(Entry::Exchanged {
            left: left.clone(),
            right,
        }));
    });
}

/// Records that the file at `from` moved to the new name `to`, which it did not replace, durable
/// only once both of their directories are synced.
#[cfg(unix)]
pub(crate) fn moved(from: &Path, to: &Path) {
    let from = resolve(from);
    record(to, |ledger, to| {
        ledger.volatile.push(Volatile::new(Entry::Moved {
            from: from.clone(),
            to,
        }));
    });
}

/// Records a directory sync, which makes the entries made in it durable.
#[cfg(unix)]
pub(crate) fn dir_synced(path: &Path) {
    record(path, |ledger, path| {
        ledger
            .volatile
            .retain_mut(|change| change.volatile_after(&path));
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
        for change in self.volatile.iter().rev() {
            self.undo(&change.entry, &relocate)?;
        }
        Ok(())
    }

    /// Undoes one volatile entry in the snapshot, whose paths `relocate` maps.
    fn undo(
        &self,
        entry: &Entry,
        relocate: &impl Fn(&Path) -> io::Result<PathBuf>,
    ) -> io::Result<()> {
        match entry {
            Entry::Created(path) => {
                let path = relocate(path)?;
                if path.exists() {
                    fs::remove_dir_all(path)?;
                }
            }
            Entry::CreatedFile(path) => {
                let path = relocate(path)?;
                if path.exists() {
                    fs::remove_file(path)?;
                }
            }
            Entry::Unlinked { path, file } => {
                let path = relocate(path)?;
                if !path.exists() {
                    self.restore_unlinked(&path, file)?;
                }
            }
            Entry::RemovedDirectory(path) => {
                let path = relocate(path)?;
                if present(&path)? {
                    return Err(io::Error::other(format!(
                        "cannot undo the volatile removal of directory {}: its name is taken",
                        path.display()
                    )));
                }
                fs::create_dir(path)?;
            }
            Entry::Renamed { from, to } => {
                let to = relocate(to)?;
                if to.exists() {
                    fs::rename(to, relocate(from)?)?;
                }
            }
            Entry::Exchanged { left, right } => {
                let (left, right) = (relocate(left)?, relocate(right)?);
                if !present(&left)? || !present(&right)? {
                    return Err(io::Error::other(format!(
                        "cannot undo the volatile exchange of {} and {}: a name is missing",
                        left.display(),
                        right.display()
                    )));
                }
                crate::fsutil::exchange_paths(&left, &right)?;
            }
            Entry::Moved { from, to } => {
                let (from, to) = (relocate(from)?, relocate(to)?);
                if !present(&to)? || present(&from)? {
                    return Err(io::Error::other(format!(
                        "cannot undo the volatile move of {} to {}: the file is gone or its old name is taken",
                        from.display(),
                        to.display()
                    )));
                }
                crate::fsutil::rename_noreplace(&to, &from)?;
            }
        }
        Ok(())
    }

    /// Writes back the durable bytes of an unlinked file from its retained descriptor.
    fn restore_unlinked(&self, path: &Path, file: &fs::File) -> io::Result<()> {
        let mut target = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)?;
        let length = self
            .durable
            .get(&inode(&file.metadata()?))
            .copied()
            .unwrap_or(0);
        let mut buffer = [0; 65_536];
        let mut offset = 0;
        while offset < length {
            let count = crate::fsutil::read_at(
                file,
                &mut buffer[..(length - offset).min(65_536) as usize],
                offset,
            )?;
            if count == 0 {
                return Err(io::Error::other("short retained unlink snapshot"));
            }
            std::io::Write::write_all(&mut target, &buffer[..count])?;
            offset += count as u64;
        }
        target.set_times(times(&file.metadata()?)?)
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
                    let mut permissions = fs::metadata(&path)?.permissions();
                    if permissions.readonly() {
                        permissions.set_mode(permissions.mode() | 0o200);
                        fs::set_permissions(&path, permissions)?;
                    }
                    fs::OpenOptions::new()
                        .write(true)
                        .open(&path)?
                        .set_len(durable)?;
                    keep_times(&path, &metadata)?;
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

pub(crate) fn hit_io(path: &Path, step: Step) -> io::Result<()> {
    hit(path, step).map_err(|error| match error {
        Injected::Io(error) => error,
        Injected::Sqlite(error) => io::Error::other(error),
    })
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
            copy_with_times(&entry.path(), &path)?;
        }
    }
    Ok(())
}

/// Copies the file at `source` to `target` with its access and modification times. A snapshot
/// stands for the same file, and a tracked fingerprint records the modification time, which a
/// plain copy replaces on Linux; only the inode number and the change time differ.
pub(crate) fn copy_with_times(source: &Path, target: &Path) -> io::Result<()> {
    fs::copy(source, target)?;
    keep_times(target, &fs::metadata(source)?)
}

/// Gives the file at `target` the access and modification times `source` records. The handle asks
/// only for what setting them needs, so a read-only copy needs no write permission: reading on
/// Unix, and the attribute-writing right on Windows, where a handle opened for reading is
/// refused.
fn keep_times(target: &Path, source: &fs::Metadata) -> io::Result<()> {
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_ATTRIBUTES;
        fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .open(target)?
    };
    #[cfg(not(windows))]
    let file = fs::File::open(target)?;
    file.set_times(times(source)?)
}

fn times(source: &fs::Metadata) -> io::Result<fs::FileTimes> {
    Ok(fs::FileTimes::new()
        .set_accessed(source.accessed()?)
        .set_modified(source.modified()?))
}
