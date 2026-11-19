//! The mirror's lifecycle. Two tables, [`after_reconciled`] and [`effect`], decide what each
//! event does to the mirror's state and to the pause the catalog records, so that no caller can
//! resume a paused mirror, or report it retrying, by itself. Only a pass that reconciled the
//! whole tree clears the recorded pause, so only applying one can fail.

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

/// A pass that reconciled the whole tree, after which the recorded pause is cleared wherever the
/// mirror moves to `Running`. Only [`after_reconciled`] decides what it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Reconciled {
    /// A run reconciled the whole tree at its start without an explicit rescan to resume a
    /// pause: this process's first pass, or the run after a retry.
    Run,
    /// The explicit rescan that was to resume a paused mirror reconciled the whole tree.
    Rescan,
}

/// What else happens to the mirror. None of these clears the recorded pause, so applying one
/// cannot fail. Only [`effect`] decides what an event changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Event {
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

/// The state an event moves the mirror to, and the reason it records for a pause, which status
/// reads and which outlives a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Effect {
    to: MirrorState,
    records: Option<PauseReason>,
}

/// The state a mirror in `from` moves to once `reconciled`; `None` leaves it as it is. Every pair
/// is listed. Only an explicit rescan that succeeded leaves `Paused` for `Running`, and nothing
/// leaves `Stopped`.
const fn after_reconciled(from: MirrorState, reconciled: Reconciled) -> Option<MirrorState> {
    use MirrorState::{Paused, Retrying, Running, Starting, Stopped};
    match (from, reconciled) {
        (Starting | Retrying, Reconciled::Run) | (Paused, Reconciled::Rescan) => Some(Running),
        (Stopped, Reconciled::Run | Reconciled::Rescan)
        | (Running | Paused, Reconciled::Run)
        | (Starting | Running | Retrying, Reconciled::Rescan) => None,
    }
}

/// What `event` does to a mirror in `from`; `None` leaves both the state and the recorded pause
/// as they are. Every pair is listed. A transient failure keeps a mirror `Starting` through the
/// retries of its first pass and never leaves `Paused`; nothing leaves `Stopped`.
const fn effect(from: MirrorState, event: Event) -> Option<Effect> {
    use MirrorState::{Paused, Retrying, Running, Starting, Stopped};
    let (to, records) = match (from, event) {
        (
            Stopped,
            Event::FailedTransiently
            | Event::FailedPersistently(_)
            | Event::WatchRetrying
            | Event::WatchRebuilt
            | Event::Stopping,
        )
        | (Starting | Retrying | Paused, Event::FailedTransiently | Event::WatchRetrying)
        | (Starting | Running | Paused, Event::WatchRebuilt) => return None,
        (Running, Event::FailedTransiently | Event::WatchRetrying) => (Retrying, None),
        (Starting | Running | Retrying | Paused, Event::FailedPersistently(reason)) => {
            (Paused, Some(reason))
        }
        (Retrying, Event::WatchRebuilt) => (Running, None),
        (Starting | Running | Retrying | Paused, Event::Stopping) => (Stopped, None),
    };
    Some(Effect { to, records })
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

    /// Applies `reconciled`, and returns the change of state. A mirror that moves to `Running`
    /// clears the recorded pause first; a failure to clear it leaves the state as it was and fails
    /// the caller, so that a running mirror never shows a pause it left.
    pub(super) async fn reconciled(
        &self,
        root: &RootLease,
        reconciled: Reconciled,
    ) -> Result<Option<Transition>, RootError> {
        let Some(to) = after_reconciled(self.state(), reconciled) else {
            return Ok(None);
        };
        root.observation_pause(None).await?;
        Ok(self.enter(to))
    }

    /// Applies `event`, and returns the change of state. A failure to record a pause leaves the
    /// mirror paused all the same and is logged as `sync_pause_unrecorded`: status shows the
    /// mirror's own `Paused` without a reason, and only a rescan that succeeds resumes it.
    pub(super) async fn on(&self, root: &RootLease, event: Event) -> Option<Transition> {
        let effect = effect(self.state(), event)?;
        let transition = self.enter(effect.to);
        if let Some(reason) = effect.records
            && let Err(error) = self.record(root, reason).await
        {
            descriptor::pause_unrecorded(&error);
        }
        transition
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

    /// Anything that happens to the mirror, in the order of the table's columns.
    #[derive(Clone, Copy, Debug)]
    enum Happening {
        Reconciled(Reconciled),
        Event(Event),
    }

    impl Happening {
        /// The state a mirror in `from` moves to, and what that does to the recorded pause: `C`
        /// clears it, `R` records it, and `-` leaves it.
        fn applied(self, from: MirrorState) -> Option<(MirrorState, char)> {
            match self {
                Self::Reconciled(reconciled) => {
                    after_reconciled(from, reconciled).map(|to| (to, 'C'))
                }
                Self::Event(event) => effect(from, event)
                    .map(|effect| (effect.to, if effect.records.is_some() { 'R' } else { '-' })),
            }
        }
    }

    const HAPPENINGS: [Happening; 7] = [
        Happening::Reconciled(Reconciled::Run),
        Happening::Reconciled(Reconciled::Rescan),
        Happening::Event(Event::FailedTransiently),
        Happening::Event(Event::FailedPersistently(PauseReason::IoFailure)),
        Happening::Event(Event::WatchRetrying),
        Happening::Event(Event::WatchRebuilt),
        Happening::Event(Event::Stopping),
    ];

    /// The expected effect of everything that happens in every state, one row per state in the
    /// order of [`HAPPENINGS`]: `-` leaves the recorded pause, `C` clears it, `R` records it.
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
            for (happening, expected) in HAPPENINGS.into_iter().zip(expected) {
                assert_eq!(
                    happening.applied(from),
                    expected,
                    "{from:?} on {happening:?}"
                );
            }
        }
    }

    /// The table above, read the other way: whatever happens, only an explicit rescan that
    /// succeeded resumes a paused mirror, and only a stop records nothing while ending it.
    #[test]
    fn only_a_successful_rescan_resumes_a_paused_mirror() {
        for happening in HAPPENINGS {
            let resumes = happening
                .applied(Paused)
                .is_some_and(|(to, _)| to != Paused);
            assert_eq!(
                resumes,
                matches!(
                    happening,
                    Happening::Reconciled(Reconciled::Rescan) | Happening::Event(Event::Stopping)
                ),
                "{happening:?}"
            );
        }
        for from in [Starting, Running, Retrying, Paused, Stopped] {
            assert!(
                effect(from, Event::Stopping).is_none_or(|effect| effect.records.is_none()),
                "{from:?}"
            );
        }
    }
}
