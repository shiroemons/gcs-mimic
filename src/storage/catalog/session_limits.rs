//! What the server configures about upload sessions.

use super::Catalog;
use crate::config::Config;
use std::time::Duration;

/// The defaults suit an embedded catalog: a busy writer fails at once.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SessionLimits {
    /// How long a client request waits for a writer someone else holds.
    pub(super) writer_wait: Option<Duration>,
}

impl From<&Config> for SessionLimits {
    fn from(config: &Config) -> Self {
        Self {
            writer_wait: Some(Duration::from_millis(config.catalog_wait_timeout_ms)),
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
