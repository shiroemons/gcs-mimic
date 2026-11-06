//! Bounded live-server controls; the catalog keeps no strong root ownership cycle.

use super::Reconciliation;
use crate::{
    auth::Identity,
    storage::sync_state::{deletions::DeletionSet, root::RootLease},
};
use std::{
    io,
    sync::{Arc, Weak},
};
use tokio::sync::{mpsc, oneshot};

pub(crate) enum Command {
    Resolve {
        request: crate::storage::sync_state::conflicts::ResolveRequest,
        reply: oneshot::Sender<
            Result<
                crate::storage::sync_state::conflicts::ResolveOutcome,
                crate::storage::sync_state::conflicts::Error,
            >,
        >,
        identity: Option<Identity>,
    },
    Rescan {
        reply: oneshot::Sender<io::Result<Reconciliation>>,
        identity: Option<Identity>,
    },
    Approve {
        revision: String,
        reply: oneshot::Sender<io::Result<DeletionSet>>,
        identity: Option<Identity>,
    },
    Wait {
        deadline: tokio::time::Instant,
        settling: Arc<std::sync::atomic::AtomicBool>,
        reply: oneshot::Sender<io::Result<WaitResult>>,
        identity: Option<Identity>,
    },
}

#[derive(serde::Serialize)]
pub(crate) struct WaitResult {
    pub completed: bool,
    pub timed_out: bool,
    pub journal_fence: Option<WaitFence>,
    pub reconciliation: Option<Reconciliation>,
    pub reason: WaitReason,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) enum WaitReason {
    Ready,
    Timeout,
    Stopped,
    Paused,
    JournalChanged,
    SourceChanging,
    SourceUnrepresentable,
    DeletionApproval,
    FindingsPending,
}
#[derive(serde::Serialize)]
pub(crate) struct WaitFence {
    pub sequence: u64,
    pub epoch: i64,
}
impl WaitResult {
    pub(crate) fn result(
        reason: WaitReason,
        journal_fence: Option<WaitFence>,
        reconciliation: Option<Reconciliation>,
    ) -> Self {
        Self {
            completed: reason == WaitReason::Ready,
            timed_out: reason == WaitReason::Timeout,
            journal_fence,
            reconciliation,
            reason,
        }
    }
    pub(crate) fn timeout() -> Self {
        Self::result(WaitReason::Timeout, None, None)
    }
}

#[derive(Clone)]
pub(crate) struct Registration {
    pub session: String,
    pub root: Weak<RootLease>,
    pub sender: mpsc::Sender<Command>,
}
