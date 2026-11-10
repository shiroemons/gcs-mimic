//! Read-only catalog connections that run beside the one writer.
//!
//! SQLite in WAL mode serves a reader from a snapshot without blocking the writer, so reads need
//! not line up on the catalog thread. They used to, which capped every object read at what that
//! one thread could do no matter how many workers served the connections.
//!
//! A snapshot is at least as new as every commit that finished before the read began, so a client
//! still reads its own writes. Two guarantees of the serialized path do not carry over by
//! themselves:
//!
//! - An exclusive job expects no SQL beside it. A pooled read therefore holds an activity lease
//!   while it runs, and takes the serialized path once a job has reserved the gate, so a steady
//!   stream of reads cannot keep the job out.
//! - A blob can be unlinked after a snapshot selected it, because reclamation follows the commit
//!   that retired the blob. A failed open then reports a transient error, and so does any SQLite
//!   failure or attempted write on the read-only connection. [`ReadOutcome`] sends all of those to
//!   the serialized path, whose answer stands.

use super::{Catalog, CatalogError, executor::Executor, sql};
use crate::object::ObjectError;
use rusqlite::{Connection, OpenFlags};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// More readers stop helping once the workers that feed them are saturated, and each one costs a
/// thread, three descriptors and its page cache.
const MAX_READERS: usize = 8;
/// Point reads prepare a few statements, unlike the writer's hundreds.
const READER_STATEMENT_CACHE: usize = 32;
/// WAL readers wait only on recovery and checkpoint locks, and the serialized path can answer
/// instead, so a short wait is enough.
const READER_BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// Point reads touch a few pages near the top of each index, and a write in another connection
/// invalidates the page cache anyway, so 1 MiB is plenty. `temp_store` keeps the one TEMP table
/// away from the file system, which a read-only container root does not offer.
const READER_SETUP: &str = "PRAGMA cache_size = -1024;
     PRAGMA mmap_size = 0;
     PRAGMA temp_store = MEMORY;
     CREATE TEMP TABLE parent_public_access_prevention (project TEXT PRIMARY KEY) WITHOUT ROWID;";

/// Whether a read's failure may be an artifact of its snapshot rather than the final answer.
pub(super) trait ReadOutcome {
    fn retry_serialized(&self) -> bool;
}

impl ReadOutcome for ObjectError {
    fn retry_serialized(&self) -> bool {
        matches!(self, Self::Unavailable(_) | Self::Io(_))
    }
}

impl ReadOutcome for CatalogError {
    fn retry_serialized(&self) -> bool {
        matches!(self, Self::Unavailable(_) | Self::Io(_))
    }
}

/// Which connection is running a read, and so whether it may also block on object data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReadPath {
    Pooled,
    /// Every other catalog call waits for this thread, so it reads metadata only.
    Serialized,
    /// The one connection of a RAM catalog, whose database no other connection can open.
    Memory,
}

impl ReadPath {
    /// Whether the read cache may keep this read's answer. A disk catalog's serialized path only
    /// stands in for a pool that is busy or failing, but a RAM catalog has no other path.
    pub(super) fn stores_answers(self) -> bool {
        match self {
            Self::Pooled | Self::Memory => true,
            Self::Serialized => false,
        }
    }
}

/// The projects under a simulated parent public access prevention policy. Every connection keeps
/// them in its own TEMP table, so each reader copies them when the revision moves.
#[derive(Default)]
pub(super) struct ParentPolicy {
    revision: AtomicU64,
    projects: Mutex<Vec<String>>,
}

impl ParentPolicy {
    /// Takes effect for reads that start after this returns.
    pub(super) fn publish(&self, projects: &[String]) {
        projects.clone_into(&mut self.projects.lock().unwrap_or_else(PoisonError::into_inner));
        self.revision.fetch_add(1, Ordering::Release);
    }
}

struct ReaderConnection {
    connection: Connection,
    parent_revision: u64,
}

impl ReaderConnection {
    fn open(path: &std::path::Path) -> rusqlite::Result<Self> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(READER_BUSY_TIMEOUT)?;
        connection.set_prepared_statement_cache_capacity(READER_STATEMENT_CACHE);
        sql::keep_plans_across_bindings(&connection)?;
        connection.execute_batch(READER_SETUP)?;
        Ok(Self {
            connection,
            parent_revision: 0,
        })
    }

    fn sync_parent_policy(&mut self, policy: &ParentPolicy) -> rusqlite::Result<()> {
        let revision = policy.revision.load(Ordering::Acquire);
        if self.parent_revision == revision {
            return Ok(());
        }
        let projects = policy
            .projects
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let transaction = self.connection.transaction()?;
        sql::exec(
            &transaction,
            "DELETE FROM parent_public_access_prevention",
            [],
        )?;
        for project in &projects {
            sql::exec(
                &transaction,
                "INSERT OR IGNORE INTO parent_public_access_prevention VALUES (?1)",
                [project],
            )?;
        }
        transaction.commit()?;
        self.parent_revision = revision;
        Ok(())
    }
}

/// One thread and, once it has served a read, the connection it uses.
struct Reader {
    executor: Arc<Executor>,
    connection: Option<ReaderConnection>,
}

/// What every job needs, apart from the catalog: a job must not keep the catalog alive on its own
/// account, and a reader on the idle list must not point back at what owns the list.
struct Shared {
    path: PathBuf,
    parent_policy: Arc<ParentPolicy>,
    /// Used as a stack: the fewest readers that keep up stay busy, and the others exist only if
    /// reads have ever overlapped that far.
    idle: Mutex<Vec<Reader>>,
    #[cfg(test)]
    served: std::sync::atomic::AtomicUsize,
}

impl Shared {
    fn idle(&self) -> std::sync::MutexGuard<'_, Vec<Reader>> {
        self.idle.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A reader away from the idle list. Dropping it returns the reader on every path out of a job,
/// including a caller that went away, and only then frees the slot, so a read granted the slot
/// finds either an idle reader or room to start one.
struct Checkout {
    reader: Option<Reader>,
    shared: Arc<Shared>,
    _slot: OwnedSemaphorePermit,
}

impl Checkout {
    /// Runs `operation` on the reader's connection, opening it on the reader's first job. `None`
    /// means the connection could not be prepared, which only costs this read its turn.
    fn run<T>(&mut self, operation: impl FnOnce(&Connection) -> T) -> Option<T> {
        let reader = self.reader.as_mut()?;
        let shared = &self.shared;
        let connection = match &mut reader.connection {
            Some(connection) => connection,
            empty => empty.insert(
                ReaderConnection::open(&shared.path)
                    .inspect_err(report_unavailable)
                    .ok()?,
            ),
        };
        connection
            .sync_parent_policy(&shared.parent_policy)
            .inspect_err(report_unavailable)
            .ok()?;
        let result = operation(&connection.connection);
        // A read leaves no transaction behind; one that did must not start the next read inside it.
        if !connection.connection.is_autocommit() {
            reader.connection = None;
        }
        #[cfg(test)]
        shared.served.fetch_add(1, Ordering::Relaxed);
        Some(result)
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        // A panic may have left the connection anywhere, so it goes down with the thread.
        if !std::thread::panicking()
            && let Some(reader) = self.reader.take()
        {
            self.shared.idle().push(reader);
        }
    }
}

pub(super) struct ReadPool {
    capacity: u32,
    slots: Arc<Semaphore>,
    shared: Arc<Shared>,
}

impl ReadPool {
    pub(super) fn new(path: PathBuf, parent_policy: Arc<ParentPolicy>) -> Self {
        let capacity = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(MAX_READERS);
        Self {
            capacity: u32::try_from(capacity).unwrap_or(u32::MAX),
            slots: Arc::new(Semaphore::new(capacity)),
            shared: Arc::new(Shared {
                path,
                parent_policy,
                idle: Mutex::default(),
                #[cfg(test)]
                served: std::sync::atomic::AtomicUsize::new(0),
            }),
        }
    }

    fn checkout(&self, slot: OwnedSemaphorePermit) -> std::io::Result<(Arc<Executor>, Checkout)> {
        let reader = match self.shared.idle().pop() {
            Some(reader) => reader,
            None => Reader {
                executor: Arc::new(Executor::start("gcs-mimic-catalog-read")?),
                connection: None,
            },
        };
        Ok((
            reader.executor.clone(),
            Checkout {
                reader: Some(reader),
                shared: self.shared.clone(),
                _slot: slot,
            },
        ))
    }

    /// Waits out the running reads and closes every connection, so the writer closes last and
    /// still checkpoints and removes the WAL. An earlier close has already done all of this.
    pub(super) async fn close(&self) {
        let Ok(running) = self.slots.clone().acquire_many_owned(self.capacity).await else {
            return;
        };
        self.slots.close();
        let readers = std::mem::take(&mut *self.shared.idle());
        for Reader {
            executor,
            connection,
        } in readers
        {
            let _ = executor.run_last(move || drop(connection)).await;
        }
        drop(running);
    }

    #[cfg(test)]
    fn reader_count(&self) -> usize {
        self.shared
            .idle()
            .iter()
            .filter(|reader| reader.connection.is_some())
            .count()
    }
}

fn report_unavailable(error: &impl std::fmt::Display) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Debug,
        "catalog_reader_unavailable",
        serde_json::json!({"message": format!("catalog read served on the serialized path: {error}")}),
    );
}

impl Catalog {
    /// Runs a read that changes nothing, on a pooled connection when it can and on the serialized
    /// connection otherwise. `operation` may run twice, so it must have no side effects beyond its
    /// result. It keeps its statements on one snapshot through [`sql::read_snapshot`].
    pub(super) async fn run_read<T, E>(
        &self,
        operation: impl Fn(&Connection) -> Result<T, E> + Send + Sync + 'static,
    ) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<CatalogError> + ReadOutcome + Send + 'static,
    {
        self.run_read_on(move |connection, _| operation(connection))
            .await
    }

    /// As [`Self::run_read`], for an operation that depends on which path runs it.
    pub(super) async fn run_read_on<T, E>(
        &self,
        operation: impl Fn(&Connection, ReadPath) -> Result<T, E> + Send + Sync + 'static,
    ) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<CatalogError> + ReadOutcome + Send + 'static,
    {
        let operation = Arc::new(operation);
        if let Some(result) = self.read_on_pool(operation.clone()).await
            && !result.as_ref().is_err_and(ReadOutcome::retry_serialized)
        {
            return result;
        }
        let path = if self.0.read_pool.is_some() {
            ReadPath::Serialized
        } else {
            ReadPath::Memory
        };
        self.run(move |connection| operation(connection, path))
            .await
    }

    /// `None` means the pool did not run the read, for a reason that is not the read's answer. A
    /// closed catalog is one: the serialized path then reports it.
    async fn read_on_pool<T, E>(
        &self,
        operation: Arc<impl Fn(&Connection, ReadPath) -> Result<T, E> + Send + Sync + 'static>,
    ) -> Option<Result<T, E>>
    where
        T: Send + 'static,
        E: Send + 'static,
    {
        let pool = self.0.read_pool.as_ref()?;
        // A writer whose rollback could not be confirmed fails every call closed, and the
        // serialized path is where a later call can confirm it.
        if self.0.resources.is_poisoned() {
            return None;
        }
        // Admission still bounds all catalog work, so what `close` must wait for stays bounded.
        let admission = self.admit().await.ok()?;
        let slot = pool.slots.clone().acquire_owned().await.ok()?;
        // Taken only now, because a lease held while queueing would keep an exclusive job waiting.
        let lease = self.read_lease()?;
        let (executor, mut checkout) = pool.checkout(slot).inspect_err(report_unavailable).ok()?;
        // The job's captures drop before its reply, so a caller woken by the answer finds the
        // reader back on the list and the slot free.
        executor
            .run(move || {
                let _held = (admission, lease);
                let result = checkout.run(|connection| operation(connection, ReadPath::Pooled));
                drop(checkout);
                result
            })
            .await
            .ok()
            .flatten()
    }

    /// Reads answered by the pool, for tests that must know which path ran.
    #[cfg(test)]
    pub(crate) fn pooled_reads(&self) -> usize {
        self.0
            .read_pool
            .as_ref()
            .map_or(0, |pool| pool.shared.served.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests;
