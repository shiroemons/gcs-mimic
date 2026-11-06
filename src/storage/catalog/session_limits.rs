//! What the server configures about upload sessions.

use super::Catalog;
use crate::{
    config::{Backend, Config},
    time_limit::TimeLimit,
};
use std::num::NonZeroU32;

/// Finished resumable records may fill one part in this many of a RAM catalog. The rest stays for
/// objects and active work: records that outgrew the budget would fail every later catalog write
/// until they expire, days later, or the server restarts.
const MEMORY_CATALOG_FINISHED_SESSION_DIVISOR: u64 = 2;

/// How long administrative work waits for its turn when no server configured the catalog.
const EMBEDDED_ADMIN_WAIT: TimeLimit = TimeLimit::from_millis(30_000);

/// How a client request treats a writer someone else holds.
#[derive(Clone, Copy, Debug, Default)]
pub(super) enum WriterWait {
    /// An embedded catalog has no request deadline to bound a wait, so it fails at once.
    #[default]
    Fail,
    /// A disabled limit waits until the request is cancelled or the catalog closes.
    Within(TimeLimit),
}

/// The defaults suit an embedded catalog: a busy writer fails at once and sessions are unlimited.
#[derive(Clone, Copy, Debug)]
pub(super) struct SessionLimits {
    pub(super) writer_wait: WriterWait,
    /// How long seeding, publication and management calls wait for admission and exclusive
    /// ownership when the caller has no budget of its own.
    pub(super) admin_wait: TimeLimit,
    pub(super) active_resumable: Option<NonZeroU32>,
    pub(super) active_rewrite: Option<NonZeroU32>,
    /// The bytes finished resumable records may fill, where the catalog has a budget.
    pub(super) finished_resumable_bytes: Option<i64>,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            writer_wait: WriterWait::default(),
            admin_wait: EMBEDDED_ADMIN_WAIT,
            active_resumable: None,
            active_rewrite: None,
            finished_resumable_bytes: None,
        }
    }
}

impl From<&Config> for SessionLimits {
    fn from(config: &Config) -> Self {
        Self {
            writer_wait: WriterWait::Within(config.catalog_wait_timeout),
            admin_wait: config.catalog_wait_timeout,
            active_resumable: config.max_resumable_sessions,
            active_rewrite: config.max_rewrite_sessions,
            finished_resumable_bytes: match config.backend {
                Backend::Disk => None,
                Backend::Memory => i64::try_from(
                    config.memory_catalog_max_bytes / MEMORY_CATALOG_FINISHED_SESSION_DIVISOR,
                )
                .ok(),
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

    /// When an administrative wait that starts now must end.
    pub(super) fn admin_deadline(&self) -> tokio::time::Instant {
        self.session_limits().admin_wait.deadline()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn administrative_waits_follow_the_catalog_wait_setting_and_default_to_thirty_seconds() {
        assert_eq!(
            SessionLimits::default().admin_wait,
            TimeLimit::from_millis(30_000)
        );
        for catalog_wait_timeout in [TimeLimit::from_millis(1), TimeLimit::DISABLED] {
            let limits = SessionLimits::from(&Config {
                catalog_wait_timeout,
                ..Default::default()
            });
            assert_eq!(limits.admin_wait, catalog_wait_timeout);
        }
    }
}
