//! What the directory mirror is doing, kept in memory so that the readiness probe and the status
//! control read it without a catalog query. A probe runs every two seconds in the container image
//! and must answer while the catalog is busy.

use std::{fmt, str::FromStr};
use tokio::sync::watch;

/// The directory mirror's lifecycle, as `/_internal/ready` reports it in `sync.state`. The API
/// stays ready in every state: a stopped mirror stops only synchronization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorState {
    /// Until this process's first initialization, which ends with a full reconciliation,
    /// finishes. The baseline an earlier process completed survives a restart, so without this
    /// state the mirror would look caught up before it has compared the tree with the catalog.
    Starting,
    /// Observing the tree and replaying API changes.
    Running,
    /// Waiting to run again after a transient failure, or retrying a directory watch rebuild.
    Retrying,
    /// Stopped by a persistent failure; only an explicit rescan that succeeds resumes it.
    Paused,
    /// Ended for good, because the server is stopping or the catalog closed.
    Stopped,
}

impl MirrorState {
    const ALL: [Self; 5] = [
        Self::Starting,
        Self::Running,
        Self::Retrying,
        Self::Paused,
        Self::Stopped,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Retrying => "retrying",
            Self::Paused => "paused",
            Self::Stopped => "stopped",
        }
    }
}

impl fmt::Display for MirrorState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A `sync.state` value that names no mirror state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownMirrorState(String);

impl fmt::Display for UnknownMirrorState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown synchronization state {:?}", self.0)
    }
}

impl std::error::Error for UnknownMirrorState {}

impl FromStr for MirrorState {
    type Err = UnknownMirrorState;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|state| state.as_str() == value)
            .ok_or_else(|| UnknownMirrorState(value.to_owned()))
    }
}

/// The mirror's current state. The mirror alone changes it; clones read the same value.
#[derive(Clone, Debug)]
pub struct MirrorStatus {
    current: watch::Sender<MirrorState>,
    /// Every state entered, in order, so that a test sees a state the mirror left at once.
    #[cfg(test)]
    entered: std::sync::Arc<std::sync::Mutex<Vec<MirrorState>>>,
}

impl Default for MirrorStatus {
    fn default() -> Self {
        Self {
            current: watch::Sender::new(MirrorState::Starting),
            #[cfg(test)]
            entered: std::sync::Arc::new(std::sync::Mutex::new(vec![MirrorState::Starting])),
        }
    }
}

impl MirrorStatus {
    pub(crate) fn state(&self) -> MirrorState {
        *self.current.borrow()
    }

    /// Moves to `state`. `Stopped` is final: a late transition cannot make a stopped mirror
    /// look as if it still ran.
    pub(super) fn enter(&self, state: MirrorState) {
        self.current.send_if_modified(|current| {
            if *current == state || *current == MirrorState::Stopped {
                return false;
            }
            *current = state;
            #[cfg(test)]
            self.entered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(state);
            true
        });
    }

    /// A status already in `state`, for tests of what reads it.
    #[cfg(test)]
    pub(crate) fn in_state(state: MirrorState) -> Self {
        let status = Self::default();
        status.enter(state);
        status
    }

    /// Waits until the mirror is in `state`; the caller bounds the wait. Only the mirror's tests,
    /// which run where synchronization does, wait for one.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) async fn reached(&self, state: MirrorState) {
        let mut changes = self.current.subscribe();
        let _ = changes.wait_for(|current| *current == state).await;
    }

    /// The states entered so far, in order, starting with `Starting`.
    #[cfg(test)]
    pub(crate) fn entered(&self) -> Vec<MirrorState> {
        self.entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_parse_from_the_names_they_report() {
        for state in MirrorState::ALL {
            assert_eq!(state.as_str().parse::<MirrorState>(), Ok(state));
            assert_eq!(state.to_string(), state.as_str());
        }
        assert!("Running".parse::<MirrorState>().is_err());
        assert!("".parse::<MirrorState>().is_err());
    }

    #[test]
    fn a_stopped_mirror_stays_stopped() {
        let status = MirrorStatus::default();
        assert_eq!(status.state(), MirrorState::Starting);
        status.enter(MirrorState::Running);
        status.enter(MirrorState::Running);
        status.enter(MirrorState::Stopped);
        status.enter(MirrorState::Running);
        assert_eq!(status.state(), MirrorState::Stopped);
        assert_eq!(
            status.entered(),
            [
                MirrorState::Starting,
                MirrorState::Running,
                MirrorState::Stopped
            ]
        );
    }
}
