//! The mirror's lifecycle. One table, [`effect`], decides what each event does to the mirror's
//! state and to the pause the catalog records, so that no caller can resume a paused mirror,
//! or report it retrying, by itself.

use super::descriptor;
use crate::{
    config::Config,
    storage::sync_state::{
        observation::PauseReason,
        root::{RootError, RootLease},
    },
    sync::{
        MirrorState, MirrorStatus,
        status::{EntryReportSender, MirrorStatusSender, Transition},
    },
};

/// What happens to the mirror. Only [`effect`] decides what an event changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Event {
    /// A run reconciled the whole tree at its start without an explicit rescan to resume a
    /// pause: this process's first pass, or the run after a retry.
    Reconciled,
    /// The explicit rescan that was to resume a paused mirror reconciled the whole tree.
    RescanSucceeded,
    /// A run, or the explicit rescan that was to resume a pause, failed transiently.
    FailedTransiently,
    /// A run, or an explicit rescan, failed in a way only an operator can repair.
    FailedPersistently(PauseReason),
    /// A rebuild of the directory watch failed and is tried again.
    WatchRetrying,
    /// The directory watch was rebuilt.
    WatchRebuilt,
    /// The mirror's thread ends.
    Stopping,
}

/// What an event does to the pause the catalog records, which status reads and which outlives a
/// restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PauseRecord {
    Unchanged,
    /// Records the reason synchronization paused for.
    Record(PauseReason),
    /// Removes the recorded pause, because a run reconciled the whole tree.
    Clear,
}

/// The state an event moves the mirror to, and what it does to the recorded pause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Effect {
    to: MirrorState,
    pause: PauseRecord,
}

/// What `event` does to a mirror in `from`; `None` leaves both the state and the recorded pause
/// as they are. Every pair is listed. Only an explicit rescan that succeeded leaves `Paused` for
/// `Running`; a transient failure keeps a mirror `Starting` through the retries of its first pass
/// and never leaves `Paused`; nothing leaves `Stopped`.
const fn effect(from: MirrorState, event: Event) -> Option<Effect> {
    use MirrorState::{Paused, Retrying, Running, Starting, Stopped};
    let (to, pause) = match (from, event) {
        (
            Stopped,
            Event::Reconciled
            | Event::RescanSucceeded
            | Event::FailedTransiently
            | Event::FailedPersistently(_)
            | Event::WatchRetrying
            | Event::WatchRebuilt
            | Event::Stopping,
        )
        | (Running | Paused, Event::Reconciled)
        | (Starting | Running | Retrying, Event::RescanSucceeded)
        | (Starting | Retrying | Paused, Event::FailedTransiently | Event::WatchRetrying)
        | (Starting | Running | Paused, Event::WatchRebuilt) => return None,
        (Starting | Retrying, Event::Reconciled) | (Paused, Event::RescanSucceeded) => {
            (Running, PauseRecord::Clear)
        }
        (Running, Event::FailedTransiently | Event::WatchRetrying) => {
            (Retrying, PauseRecord::Unchanged)
        }
        (Starting | Running | Retrying | Paused, Event::FailedPersistently(reason)) => {
            (Paused, PauseRecord::Record(reason))
        }
        (Retrying, Event::WatchRebuilt) => (Running, PauseRecord::Unchanged),
        (Starting | Running | Retrying | Paused, Event::Stopping) => {
            (Stopped, PauseRecord::Unchanged)
        }
    };
    Some(Effect { to, pause })
}

/// The mirror's state, which only events change, through [`effect`]. The mirror's thread keeps
/// it; a run borrows it only to report what happened, and readers get a [`MirrorStatus`].
#[derive(Clone, Debug, Default)]
pub(crate) struct Lifecycle {
    status: MirrorStatusSender,
    /// The working tree whose faults a test arms.
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    tree: Option<std::path::PathBuf>,
}

impl Lifecycle {
    pub(super) fn new(config: &Config) -> Self {
        #[cfg(not(all(test, any(target_os = "linux", target_os = "macos"))))]
        let _ = config;
        Self {
            status: MirrorStatusSender::default(),
            #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
            tree: config.sync_dir.clone(),
        }
    }

    pub(super) fn state(&self) -> MirrorState {
        self.status.state()
    }

    /// A reader of the state.
    pub(super) fn subscribe(&self) -> MirrorStatus {
        self.status.subscribe()
    }

    /// The sender of the entry report alone, for the passes the mirror runs, which changes no
    /// state.
    pub(super) fn entries(&self) -> &EntryReportSender {
        self.status.entries()
    }

    /// Applies `event`, the one place that changes the state or the recorded pause, and returns
    /// the change of state. A failure to record a pause leaves the mirror paused all the same and
    /// is logged as `sync_pause_unrecorded`: status shows the mirror's own `Paused` without a
    /// reason, and only a rescan that succeeds resumes it. A failure to clear one leaves the state
    /// as it was and fails the caller, so that a running mirror never shows a pause it left.
    pub(super) async fn on(
        &self,
        root: &RootLease,
        event: Event,
    ) -> Result<Option<Transition>, RootError> {
        let Some(effect) = effect(self.state(), event) else {
            return Ok(None);
        };
        let transition = match effect.pause {
            PauseRecord::Clear => {
                root.observation_pause(None).await?;
                self.enter(effect.to)
            }
            PauseRecord::Record(reason) => {
                let transition = self.enter(effect.to);
                if let Err(error) = self.record(root, reason).await {
                    descriptor::pause_unrecorded(&error);
                }
                transition
            }
            PauseRecord::Unchanged => self.enter(effect.to),
        };
        Ok(transition)
    }

    /// Applies [`Event::Stopping`], which leaves the recorded pause as it is, so that a guard
    /// running while the thread unwinds needs no catalog.
    pub(super) fn stop(&self) {
        if let Some(effect) = effect(self.state(), Event::Stopping) {
            self.enter(effect.to);
        }
    }

    /// Moves to `state` and logs the change once; the status itself only keeps it.
    fn enter(&self, state: MirrorState) -> Option<Transition> {
        let transition = self.status.enter(state);
        if let Some(transition) = transition {
            descriptor::state_changed(transition);
        }
        transition
    }

    async fn record(&self, root: &RootLease, reason: PauseReason) -> Result<(), RootError> {
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        super::tests::state::injected_pause_record_failure(self.tree.as_deref())?;
        root.observation_pause(Some(reason)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use MirrorState::{Paused, Retrying, Running, Starting, Stopped};

    const EVENTS: [Event; 7] = [
        Event::Reconciled,
        Event::RescanSucceeded,
        Event::FailedTransiently,
        Event::FailedPersistently(PauseReason::IoFailure),
        Event::WatchRetrying,
        Event::WatchRebuilt,
        Event::Stopping,
    ];

    /// The expected effect of every event in every state, one row per state in the order of
    /// [`EVENTS`]: `-` leaves the state and the recorded pause, `C` clears the pause, `R` records
    /// it.
    #[test]
    fn every_event_in_every_state_has_the_listed_effect() {
        type Row = (MirrorState, [Option<(MirrorState, char)>; 7]);
        let keep = None;
        let table: [Row; 5] = [
            (
                Starting,
                [
                    Some((Running, 'C')),
                    keep,
                    keep,
                    Some((Paused, 'R')),
                    keep,
                    keep,
                    Some((Stopped, '-')),
                ],
            ),
            (
                Running,
                [
                    keep,
                    keep,
                    Some((Retrying, '-')),
                    Some((Paused, 'R')),
                    Some((Retrying, '-')),
                    keep,
                    Some((Stopped, '-')),
                ],
            ),
            (
                Retrying,
                [
                    Some((Running, 'C')),
                    keep,
                    keep,
                    Some((Paused, 'R')),
                    keep,
                    Some((Running, '-')),
                    Some((Stopped, '-')),
                ],
            ),
            (
                Paused,
                [
                    keep,
                    Some((Running, 'C')),
                    keep,
                    Some((Paused, 'R')),
                    keep,
                    keep,
                    Some((Stopped, '-')),
                ],
            ),
            (Stopped, [keep; 7]),
        ];
        for (from, expected) in table {
            for (event, expected) in EVENTS.into_iter().zip(expected) {
                let expected = expected.map(|(to, pause)| Effect {
                    to,
                    pause: match pause {
                        'C' => PauseRecord::Clear,
                        'R' => PauseRecord::Record(PauseReason::IoFailure),
                        _ => PauseRecord::Unchanged,
                    },
                });
                assert_eq!(effect(from, event), expected, "{from:?} on {event:?}");
            }
        }
    }

    /// The table above, read the other way: whatever happens, only an explicit rescan that
    /// succeeded resumes a paused mirror, and only a stop records nothing while ending it.
    #[test]
    fn only_a_successful_rescan_resumes_a_paused_mirror() {
        for event in EVENTS {
            let resumes = effect(Paused, event).is_some_and(|effect| effect.to != Paused);
            assert_eq!(
                resumes,
                matches!(event, Event::RescanSucceeded | Event::Stopping),
                "{event:?}"
            );
        }
        for from in [Starting, Running, Retrying, Paused, Stopped] {
            assert!(
                effect(from, Event::Stopping)
                    .is_none_or(|effect| effect.pause == PauseRecord::Unchanged),
                "{from:?}"
            );
        }
    }
}
