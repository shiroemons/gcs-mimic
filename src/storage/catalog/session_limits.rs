//! What the server configures about upload sessions.

use super::{Catalog, resumable::MAX_TERMINAL_SESSIONS};
use crate::{
    config::{Backend, Config},
    time_limit::TimeLimit,
};
use std::num::NonZeroU32;

/// The RAM catalog budget each finished resumable record may claim. A typical record takes about
/// 1.6 KiB, so the window of finished records stays under half the budget, and the default 16 MiB
/// budget still keeps the whole window. Records that outgrew the budget would fail every later
/// catalog write until they expire, days later, or the server restarts.
const MEMORY_CATALOG_BYTES_PER_FINISHED_SESSION: u64 = 4096;

/// How a client request treats a writer someone else holds.
#[derive(Clone, Copy, Debug, Default)]
pub(super) enum WriterWait {
    /// An embedded catalog has no request deadline to bound a wait, so it fails at once.
    #[default]
    Fail,
    /// A disabled limit waits until the request is cancelled or the catalog closes.
    Within(TimeLimit),
}

#[derive(Clone, Copy, Debug)]
pub(super) struct SessionLimits {
    pub(super) writer_wait: WriterWait,
    pub(super) active_resumable: Option<NonZeroU32>,
    pub(super) active_rewrite: Option<NonZeroU32>,
    /// How many finished resumable records stay after trimming.
    pub(super) finished_resumable: i64,
}

/// The defaults suit an embedded catalog: a busy writer fails at once and sessions are unlimited.
impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            writer_wait: WriterWait::default(),
            active_resumable: None,
            active_rewrite: None,
            finished_resumable: MAX_TERMINAL_SESSIONS,
        }
    }
}

impl From<&Config> for SessionLimits {
    fn from(config: &Config) -> Self {
        Self {
            writer_wait: WriterWait::Within(config.catalog_wait_timeout),
            active_resumable: config.max_resumable_sessions,
            active_rewrite: config.max_rewrite_sessions,
            finished_resumable: match config.backend {
                Backend::Disk => MAX_TERMINAL_SESSIONS,
                Backend::Memory => i64::try_from(
                    config.memory_catalog_max_bytes / MEMORY_CATALOG_BYTES_PER_FINISHED_SESSION,
                )
                .map_or(MAX_TERMINAL_SESSIONS, |fits| {
                    fits.min(MAX_TERMINAL_SESSIONS)
                }),
            },
        }
    }
}

impl Catalog {
    /// Applies the server's session settings, once, before it accepts requests. The first call
    /// decides: a limit that changed under running requests would apply to only some of them.
    pub fn apply_session_limits(&self, config: &Config) {
        self.0
            .session_limits
            .get_or_init(|| SessionLimits::from(config));
    }

    pub(super) fn session_limits(&self) -> SessionLimits {
        self.0.session_limits.get().copied().unwrap_or_default()
    }
}
