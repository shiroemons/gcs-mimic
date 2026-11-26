//! Ownership of the catalog's resources and worker state.

use super::*;

pub(super) struct Policy {
    pub(super) principals: Option<principals::Profile>,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) session_limits: OnceLock<session_limits::SessionLimits>,
    /// Retention a new bucket without an explicit softDeletePolicy receives, an
    /// `Option<NonZeroU64>` in atomic form: read it through `default_soft_delete_retention()`.
    pub(super) default_soft_delete_retention: AtomicU64,
}

pub(super) struct Storage {
    pub(super) backend: BlobBackend,
    pub(super) data_dir: PathBuf,
    pub(super) data_dir_state: DataDirState,
    pub(super) reclamation: Arc<Semaphore>,
}

pub(super) struct Writer {
    pub(super) resources: Mutex<Option<Resources>>,
    pub(super) execution: Arc<Semaphore>,
    pub(super) executor: executor::Executor,
    /// Upload commits waiting for the writer, which its next holder publishes together.
    pub(super) upload_commits: objects::CommitQueue,
    #[cfg(test)]
    pub(super) fail_rollback: AtomicBool,
}

pub(super) struct Reads {
    /// Absent for a RAM catalog, whose database exists only inside the writer's connection.
    pub(super) pool: Option<read_pool::ReadPool>,
    pub(super) parent_policy: Arc<read_pool::ParentPolicy>,
    pub(super) cache: read_cache::ReadCache,
    pub(super) runtime: tokio::runtime::Handle,
}

pub(super) struct Admission {
    pub(super) slots: Arc<Semaphore>,
    pub(super) activity: ActivityGate,
    pub(super) uploads: Mutex<UploadState>,
    pub(super) uploads_idle: Notify,
    #[cfg(test)]
    pub(super) waiters: std::sync::atomic::AtomicUsize,
}

pub(super) struct AdminState {
    pub(super) uploads: Arc<Semaphore>,
    pub(super) reads: Arc<Semaphore>,
    pub(super) staged_handles: Arc<Semaphore>,
    pub(super) publication_batch: Arc<Semaphore>,
    /// Held by a seed batch from before it lets go of its lease until its commit ends, while
    /// nothing else keeps a reset from clearing the bodies it already put in the RAM store.
    pub(super) seed_commit: Arc<Semaphore>,
    /// Held by a publication request until its batch and every staged cleanup it left are done,
    /// which is what `close` waits for. Callers past the permits wait for one like any other slot.
    pub(super) publication_requests: Arc<Semaphore>,
    pub(super) file_workers: admin::FileWorkers,
    pub(super) grant: Option<admin::AdminGrant>,
    /// Shared with the task that runs the passes, which waits on it without holding the catalog.
    pub(super) seed_watch: Arc<admin::seed_watch::SeedWatch>,
    #[cfg(test)]
    pub(super) cleanup_tasks: std::sync::atomic::AtomicUsize,
}

pub(super) struct WriterRegistry {
    pub(super) sessions: Mutex<resumable::writers::SessionWriters>,
    /// Wakes requests waiting for a writer. One waker serves every session: a release with no
    /// waiter costs an atomic check, and waiters on a busy session are few and short-lived.
    pub(super) released: Notify,
}

pub(super) struct Maintenance {
    pub(super) wake: Arc<Notify>,
    pub(super) derived_wake: Notify,
    pub(super) lifecycle: lifecycle::LifecycleSchedule,
    /// Whether a bucket delete may have left XML multipart uploads behind.
    pub(super) multipart_orphan_scan: AtomicBool,
    pub(super) error: Mutex<Option<BackendCause>>,
    pub(super) started: AtomicBool,
    pub(super) health: crate::task::TaskHealth,
    #[cfg(test)]
    pub(super) panics: std::sync::atomic::AtomicUsize,
}

pub(super) struct Notifications {
    pub(super) wake: Arc<Notify>,
    pub(super) started: AtomicBool,
    /// The local Pub/Sub emulator, set once when the publisher starts; also answers topic lookups.
    pub(super) endpoint: OnceLock<crate::notification::Endpoint>,
    pub(super) attempts: Arc<Semaphore>,
}

pub(super) struct Shutdown {
    pub(super) admin_wake: Notify,
    /// Workers started beside the catalog whose health readiness also reports. Each worker
    /// registers once and is never removed; the server starts each such worker once per
    /// catalog, so the list stays as short as the set of workers.
    pub(super) background_health: Mutex<Vec<crate::task::TaskHealth>>,
}
