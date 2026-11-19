use crate::{
    storage::sync_state::{
        deletions::{DeletionAuthorization, DeletionSet},
        observation::{PauseReason, Summary},
        writeback::Stop,
    },
    sync::MirrorState,
};

/// The one `state` the status control reports, from a closed set. While the mirror is not
/// running, its own state decides; while it runs, the first condition of the observation that
/// holds. The server's readiness never decides it: the API stays ready while synchronization
/// stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ControlState {
    /// This process's first pass has not finished. A reason an earlier process recorded shows
    /// only in `observation.paused_reason`, since this process has not reached it yet.
    Starting,
    /// The mirror started and recovers from a transient failure.
    Retrying,
    /// Paused for `observer-failure`, `SourceChanging` or `SourceUnrepresentable`, or by a pause
    /// the mirror failed to record.
    Paused,
    /// Ended with the server, or after the mirror's thread ended.
    Stopped,
    /// A pause or the write-back state for this reason.
    ConflictCapacity,
    /// A pause or the write-back state for this reason.
    IoFailure,
    /// A pause or the write-back state for this reason.
    RecoveryRequired,
    /// The write-back state for this reason.
    ProtectedBody,
    /// The last scan stopped for this reason without pausing; the next scan tries again.
    SourceChanging,
    /// The last scan stopped for this reason without pausing; the next scan tries again.
    SourceUnrepresentable,
    /// Running without a complete baseline.
    Observing,
    /// External deletions wait for an approval.
    DeletionApproval,
    /// Findings that stop `sync wait`, path stops, pending write-back or unresolved conflicts
    /// remain.
    FindingsPending,
    /// Nothing waits. Objects whose names the tree cannot hold may remain; they stay reachable
    /// through the API, and the findings view lists them.
    Ready,
}

crate::serde_util::serialize_as_str!(ControlState);

impl ControlState {
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(super) const ALL: [Self; 14] = [
        Self::Starting,
        Self::Retrying,
        Self::Paused,
        Self::Stopped,
        Self::ConflictCapacity,
        Self::IoFailure,
        Self::RecoveryRequired,
        Self::ProtectedBody,
        Self::SourceChanging,
        Self::SourceUnrepresentable,
        Self::Observing,
        Self::DeletionApproval,
        Self::FindingsPending,
        Self::Ready,
    ];

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Retrying => "Retrying",
            Self::Paused => "Paused",
            Self::Stopped => "Stopped",
            Self::ConflictCapacity => "ConflictCapacity",
            Self::IoFailure => "IoFailure",
            Self::RecoveryRequired => "RecoveryRequired",
            Self::ProtectedBody => "ProtectedBody",
            Self::SourceChanging => "SourceChanging",
            Self::SourceUnrepresentable => "SourceUnrepresentable",
            Self::Observing => "Observing",
            Self::DeletionApproval => "DeletionApproval",
            Self::FindingsPending => "FindingsPending",
            Self::Ready => "Ready",
        }
    }

    pub(super) fn of(
        mirror: MirrorState,
        observation: &Summary,
        set: Option<&DeletionSet>,
    ) -> Self {
        match mirror {
            MirrorState::Starting => Self::Starting,
            MirrorState::Retrying => Self::Retrying,
            MirrorState::Stopped => Self::Stopped,
            MirrorState::Paused => observation.paused_reason.map_or(Self::Paused, Self::paused),
            MirrorState::Running => Self::running(observation, set),
        }
    }

    fn running(observation: &Summary, set: Option<&DeletionSet>) -> Self {
        if let Some(reason) = observation.paused_reason {
            Self::paused(reason)
        } else if let Some(stop) = observation
            .writeback
            .as_ref()
            .and_then(|writeback| Self::writeback(writeback.state))
        {
            stop
        } else if let Some(stop) = observation.scan_stop {
            Self::scan_stopped(stop)
        } else if !observation.baseline_complete {
            Self::Observing
        } else if set
            .is_some_and(|set| set.guarded && set.authorization == DeletionAuthorization::Withheld)
        {
            Self::DeletionApproval
        } else if observation.path_stops
            + observation.blocking_findings
            + observation.pending_writeback
            + observation.unresolved_conflicts
            > 0
        {
            Self::FindingsPending
        } else {
            Self::Ready
        }
    }

    /// A pause names the reasons that need their own repair; the others share `Paused`, and
    /// `observation.paused_reason` tells them apart.
    const fn paused(reason: PauseReason) -> Self {
        match reason {
            PauseReason::ConflictCapacity => Self::ConflictCapacity,
            PauseReason::IoFailure => Self::IoFailure,
            PauseReason::RecoveryRequired => Self::RecoveryRequired,
            PauseReason::ObserverFailure
            | PauseReason::SourceChanging
            | PauseReason::SourceUnrepresentable => Self::Paused,
        }
    }

    /// The catalog keeps only the two source reasons as a scan stop; another one would be a
    /// pause, so it answers as one.
    const fn scan_stopped(reason: PauseReason) -> Self {
        match reason {
            PauseReason::SourceChanging => Self::SourceChanging,
            PauseReason::SourceUnrepresentable => Self::SourceUnrepresentable,
            PauseReason::ConflictCapacity
            | PauseReason::IoFailure
            | PauseReason::RecoveryRequired
            | PauseReason::ObserverFailure => Self::paused(reason),
        }
    }

    const fn writeback(stop: Stop) -> Option<Self> {
        match stop {
            Stop::Ready => None,
            Stop::RecoveryRequired => Some(Self::RecoveryRequired),
            Stop::ProtectedBody => Some(Self::ProtectedBody),
            Stop::ConflictCapacity => Some(Self::ConflictCapacity),
            Stop::IoFailure => Some(Self::IoFailure),
        }
    }
}
