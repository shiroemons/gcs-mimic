//! What the server configures about upload sessions.

use super::Catalog;
use crate::{config::Config, time_limit::TimeLimit};
use std::num::NonZeroU32;

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
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SessionLimits {
    pub(super) writer_wait: WriterWait,
    pub(super) active_resumable: Option<NonZeroU32>,
    pub(super) active_rewrite: Option<NonZeroU32>,
}

impl From<&Config> for SessionLimits {
    fn from(config: &Config) -> Self {
        Self {
            writer_wait: WriterWait::Within(config.catalog_wait_timeout),
            active_resumable: config.max_resumable_sessions,
            active_rewrite: config.max_rewrite_sessions,
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
