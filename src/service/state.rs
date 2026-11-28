//! The state one server shares across its listeners, connections and adapters.

use crate::{
    auth::Auth,
    clock::Clock,
    config::Config,
    service::{cors_warnings::CorsWarnings, read_handles::ReadHandles},
    storage::Catalog,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Semaphore;

mod project_names;

/// How many buffered batch envelopes one server admits at once.
const BATCH_CONCURRENCY: usize = 2;

pub struct State {
    pub(crate) pools: WorkPools,
    pub(crate) xml_cors_warned_buckets: CorsWarnings,
    pub(crate) grpc_read_handles: ReadHandles,
    pub ready: AtomicBool,
    pub(crate) clock: Arc<dyn Clock>,
    pub catalog: Catalog,
    pub(crate) api_base_url: String,
    pub(crate) default_location: String,
    pub(crate) auth: Auth,
}

impl State {
    /// A ready server state whose work pools and default location follow `config`.
    pub fn new(
        config: &Config,
        catalog: Catalog,
        clock: Arc<dyn Clock>,
        api_base_url: String,
        auth: Auth,
    ) -> Self {
        Self {
            pools: WorkPools::new(config),
            xml_cors_warned_buckets: Default::default(),
            grpc_read_handles: Default::default(),
            ready: AtomicBool::new(true),
            clock,
            catalog,
            api_base_url,
            default_location: config.default_location().to_owned(),
            auth,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(catalog: Catalog, auth: Auth) -> Self {
        Self::new(
            &Config::default(),
            catalog,
            Arc::new(crate::clock::SystemClock),
            "http://localhost:4443".into(),
            auth,
        )
    }

    pub fn is_ready(&self) -> bool {
        if !self.catalog.is_healthy() {
            self.ready.store(false, Ordering::Release);
        }
        // A background loop restarting after a panic is a transient gap, so it does not latch.
        self.ready.load(Ordering::Acquire) && self.catalog.background_tasks_running()
    }
}

/// Per-server admission for CPU work and buffered batch envelopes.
#[derive(Clone)]
pub(crate) struct WorkPools {
    /// Decoder permits for the compressed bodies of callers who authenticate.
    pub(crate) gzip_decoders: Arc<Semaphore>,
    /// The one decoder permit that callers who do not authenticate share. A compressed body is
    /// expanded before its caller is authorized, as Cloud Storage does, so with a common pool an
    /// anonymous client could keep every decoder busy and hold back the callers who authenticate.
    pub(crate) unauthenticated_gzip_decoders: Arc<Semaphore>,
    pub(crate) batches: Arc<Semaphore>,
}

impl WorkPools {
    fn new(config: &Config) -> Self {
        Self {
            gzip_decoders: Arc::new(Semaphore::new(
                config.max_blocking_threads().div_ceil(4).max(1),
            )),
            unauthenticated_gzip_decoders: Arc::new(Semaphore::new(1)),
            batches: Arc::new(Semaphore::new(BATCH_CONCURRENCY)),
        }
    }
}

#[cfg(test)]
mod tests;
