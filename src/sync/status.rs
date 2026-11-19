//! What the directory mirror is doing, kept in memory so that the readiness probe and the status
//! control read it without a catalog query. A probe runs every two seconds in the container image
//! and must answer while the catalog is busy.

use super::entries::EntryReport;
use std::{fmt, str::FromStr, sync::Arc};
use tokio::sync::watch;

/// The directory mirror's lifecycle, as `/_internal/ready` reports it in `sync.state`. The API
/// stays ready in every state: a stopped mirror stops only synchronization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MirrorState {
    /// Until this process's first initialization, which ends with a full reconciliation,
    /// finishes, through any retries on the way. The baseline an earlier process completed
    /// survives a restart, so without this state the mirror would look caught up before it has
    /// compared the tree with the catalog.
    Starting,
    /// Observing the tree and replaying API changes.
    Running,
    /// Recovering from a transient failure after the mirror has started: from the failure,
    /// through the retry delay, until the next run's full reconciliation finishes, or until a
    /// directory watch rebuild succeeds. A retry of the first pass stays [`Self::Starting`].
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

crate::serde_util::serialize_as_str!(MirrorState);

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

/// A change of the mirror's state, which the mirror reports once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Transition {
    pub(crate) from: MirrorState,
    pub(crate) to: MirrorState,
}

/// Every state entered, in order, so that a test sees a state the mirror left at once.
#[cfg(test)]
type History = std::sync::Arc<std::sync::Mutex<Vec<MirrorState>>>;

/// The mirror's current state, for those who only read it, such as the readiness probe and the
/// status control. Clones read the same value; only the mirror's [`MirrorStatusSender`] changes it.
#[derive(Clone, Debug)]
pub struct MirrorStatus {
    current: watch::Receiver<MirrorState>,
    entries: watch::Receiver<Arc<EntryReport>>,
    #[cfg(test)]
    entered: History,
}

impl MirrorStatus {
    pub(crate) fn state(&self) -> MirrorState {
        *self.current.borrow()
    }

    /// The working-tree entries the latest complete scan could not take in, which hold back
    /// the baseline and deletion detection until they are renamed, replaced or moved away.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the status control has yet to list these entries; until it does only tests read them"
        )
    )]
    pub(crate) fn unrepresentable_entries(&self) -> Arc<EntryReport> {
        Arc::clone(&self.entries.borrow())
    }

    /// A status already in `state`, for tests of what reads it.
    #[cfg(test)]
    pub(crate) fn in_state(state: MirrorState) -> Self {
        let sender = MirrorStatusSender::default();
        let _ = sender.enter(state);
        sender.subscribe()
    }

    /// Waits until the mirror is in `state`, and says whether it got there before the mirror
    /// went away; the caller bounds the wait. Only the mirror's tests, which run where
    /// synchronization does, wait for one.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) async fn reached(&self, state: MirrorState) -> bool {
        let mut current = self.current.clone();
        current.wait_for(|current| *current == state).await.is_ok()
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

/// Changes the mirror's state. The mirror keeps it and hands out [`MirrorStatus`] to readers,
/// so that no reader can change the state.
#[derive(Clone, Debug)]
pub(crate) struct MirrorStatusSender {
    current: watch::Sender<MirrorState>,
    entries: watch::Sender<Arc<EntryReport>>,
    #[cfg(test)]
    entered: History,
}

impl Default for MirrorStatusSender {
    fn default() -> Self {
        Self {
            current: watch::Sender::new(MirrorState::Starting),
            entries: watch::Sender::new(Arc::default()),
            #[cfg(test)]
            entered: std::sync::Arc::new(std::sync::Mutex::new(vec![MirrorState::Starting])),
        }
    }
}

impl MirrorStatusSender {
    pub(crate) fn state(&self) -> MirrorState {
        *self.current.borrow()
    }

    /// A reader of this state.
    pub(crate) fn subscribe(&self) -> MirrorStatus {
        MirrorStatus {
            current: self.current.subscribe(),
            entries: self.entries.subscribe(),
            #[cfg(test)]
            entered: std::sync::Arc::clone(&self.entered),
        }
    }

    /// Replaces the entries a complete scan could not take in with those of a newer one.
    pub(crate) fn report_entries(&self, report: EntryReport) {
        self.entries.send_if_modified(|current| {
            if **current == report {
                return false;
            }
            *current = Arc::new(report);
            true
        });
    }

    /// Moves to `state`, and returns the change, or `None` when the state stays. `Stopped` is
    /// final: a late transition cannot make a stopped mirror look as if it still ran.
    #[must_use = "the mirror reports each change of its state"]
    pub(crate) fn enter(&self, state: MirrorState) -> Option<Transition> {
        let mut changed = None;
        self.current.send_if_modified(|current| {
            if *current == state || *current == MirrorState::Stopped {
                return false;
            }
            changed = Some(Transition {
                from: *current,
                to: state,
            });
            *current = state;
            #[cfg(test)]
            self.entered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(state);
            true
        });
        changed
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

    /// Only a change is a transition, so the mirror reports each one once; nothing follows
    /// `Stopped`.
    #[test]
    fn a_stopped_mirror_stays_stopped() {
        use MirrorState::{Running, Starting, Stopped};
        let sender = MirrorStatusSender::default();
        let status = sender.subscribe();
        assert_eq!(status.state(), Starting);
        assert_eq!(
            sender.enter(Running),
            Some(Transition {
                from: Starting,
                to: Running
            })
        );
        assert_eq!(sender.enter(Running), None);
        assert_eq!(
            sender.enter(Stopped),
            Some(Transition {
                from: Running,
                to: Stopped
            })
        );
        assert_eq!(sender.enter(Running), None);
        assert_eq!(status.state(), MirrorState::Stopped);
        drop(sender);
        assert_eq!(
            status.state(),
            MirrorState::Stopped,
            "a reader keeps the last state after the mirror is gone"
        );
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
