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
use tokio::sync::{Semaphore, mpsc, oneshot};

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
    pub reason: &'static str,
}
#[derive(serde::Serialize)]
pub(crate) struct WaitFence {
    pub sequence: u64,
    pub epoch: i64,
}
impl WaitResult {
    pub(crate) fn timeout() -> Self {
        Self {
            completed: false,
            timed_out: true,
            journal_fence: None,
            reconciliation: None,
            reason: "Timeout",
        }
    }
}

#[derive(Clone)]
pub(crate) struct Registration {
    pub session: String,
    pub root: Weak<RootLease>,
    pub sender: mpsc::Sender<Command>,
    pub admission: Arc<Semaphore>,
}
