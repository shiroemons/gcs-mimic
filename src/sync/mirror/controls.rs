//! The live controls: rescan, approval, wait and conflict resolution.

use super::*;
use crate::{
    config::Config,
    diagnostics::LogLevel,
    storage::{
        Catalog,
        admin::AdminError,
        change_journal::JournalFence,
        sync_state::{
            root::{RootError, RootLease},
            writeback::Writeback,
        },
    },
    sync::{
        EntryReportSender, Reconciliation, SyncError, error::StoppedRescan, interrupt::Interrupt,
    },
};
use std::future::Future;

/// State a live run carries from one control to the next.
pub(super) struct LiveState {
    pub(super) cursor: JournalFence,
    pub(super) baseline: BaselineProgress,
    pub(super) skipped: SkippedExports,
}

/// What every control of one live run borrows. Each request copies it with its own scoped root.
#[derive(Clone, Copy)]
pub(in crate::sync) struct LiveScope<'a> {
    pub(in crate::sync) catalog: &'a Catalog,
    pub(in crate::sync) root: &'a RootLease,
    pub(in crate::sync) config: &'a Config,
    pub(in crate::sync) writeback: &'a Writeback,
    pub(in crate::sync) stopping: &'a tokio::sync::watch::Receiver<bool>,
    /// Where an ingestion names the entries it could not take in. It changes nothing else of the
    /// mirror's status, whose state changes only through the lifecycle events a run reports.
    pub(in crate::sync) entries: &'a EntryReportSender,
}

pub(super) struct LiveControl<'a> {
    pub(super) scope: LiveScope<'a>,
    pub(super) state: &'a mut LiveState,
}
impl LiveControl<'_> {
    pub(super) async fn rescan(&mut self) -> Result<Reconciliation, SyncError> {
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        tests::state::injected_rescan_failure(self.scope.config)?;
        let writeback = recover_writeback(self.scope.root).await?;
        let scope = LiveScope {
            writeback: &writeback,
            ..self.scope
        };
        let summary =
            ingest_external(scope, &mut self.state.baseline, &mut self.state.skipped).await?;
        Ok(summary)
    }

    pub(super) fn handle<'a>(
        &'a mut self,
        request: Command,
        quiet: observation::Quiet,
        watcher: &'a mut DirectoryWatch,
    ) -> impl Future<Output = Result<(), SyncError>> + 'a {
        // The single worker boxes one control future to bound nested reconciliation stack use.
        Box::pin(async move {
            let history = matches!(&request, Command::Wait { .. } | Command::Rescan { .. })
                .then(|| watcher.history_scan(quiet));
            let identity = match &request {
                Command::Rescan { identity, .. }
                | Command::Decide { identity, .. }
                | Command::Wait { identity, .. }
                | Command::Resolve { identity, .. } => identity.clone(),
            };
            let scoped = self.scope.root.with_authority(
                crate::storage::sync_state::authority::ControlAuthority::new(
                    self.scope.config.seed_project.clone(),
                    identity,
                ),
            );
            let writeback = self.scope.writeback.with_root(&scoped)?;
            let mut control = LiveControl {
                scope: LiveScope {
                    root: &scoped,
                    writeback: &writeback,
                    ..self.scope
                },
                state: &mut *self.state,
            };
            control.handle_with_history(request, history).await
        })
    }

    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(super) async fn handle_authorized(&mut self, request: Command) -> Result<(), SyncError> {
        self.handle_with_history(request, None).await
    }
    pub(super) async fn handle_with_history(
        &mut self,
        request: Command,
        history: Option<observation::HistoryScan<'_>>,
    ) -> Result<(), SyncError> {
        match request {
            Command::Resolve {
                request,
                reply,
                identity,
            } => {
                let authority = crate::storage::sync_state::authority::ControlAuthority::new(
                    self.scope.config.seed_project.clone(),
                    identity,
                );
                let result = self
                    .scope
                    .root
                    .resolve_conflict(request, authority, self.scope.config.max_object_bytes)
                    .await;
                let _ = reply.send(result);
            }
            Command::Wait {
                deadline,
                reply,
                identity,
                ..
            } => {
                let result = match self
                    .scope
                    .root
                    .authorize_controls(self.scope.config.seed_project.clone(), identity)
                    .await
                {
                    Ok(()) => match self.wait(deadline).await {
                        Ok(result) => {
                            if let (Some(history), Some(summary)) =
                                (history, &result.reconciliation)
                            {
                                history.finish(self.scope.root, summary).await;
                            }
                            Ok(result)
                        }
                        Err(error) => Err(ControlError::Failed(error)),
                    },
                    Err(error) => Err(error.into()),
                };
                let _ = reply.send(result);
            }
            Command::Rescan { reply, identity } => {
                let summary = match self
                    .scope
                    .root
                    .authorize_controls(self.scope.config.seed_project.clone(), identity)
                    .await
                {
                    Ok(()) => match self.rescan().await {
                        Ok(summary) => {
                            if let Some(history) = history {
                                history.finish(self.scope.root, &summary).await;
                            }
                            Ok(summary)
                        }
                        Err(error) => Err(error),
                    },
                    Err(error) => {
                        let _ = reply.send(Err(error.into()));
                        return Ok(());
                    }
                };
                match summary {
                    // A failure that pauses ends the run, and the mirror answers the rescan only
                    // after it paused, so that whoever hears the answer finds the pause.
                    Err(error) if !error.is_transient() => {
                        let reason = error.pause_reason();
                        return Err(SyncError::RescanStopped(Box::new(StoppedRescan {
                            reason,
                            error,
                            reply,
                        })));
                    }
                    summary => {
                        let _ = reply.send(summary.map_err(ControlError::Failed));
                    }
                }
            }
            Command::Decide {
                decision,
                revision,
                reply,
                identity,
            } => self.decide(decision, revision, reply, identity).await?,
        }
        Ok(())
    }

    /// Authorizes `decision` on the deletion set `revision` for `identity`, then inventories the
    /// tree again if the set still waits for the decision, so that a native notification since
    /// the last pass that left its members as they were does not make it stale. A stop or an
    /// API change during that inventory refuses the decision: the set was not compared with the
    /// tree at the current fence, so recording a decision on it would apply to members nobody
    /// confirmed. The refusal does not rely on the decision's own check of the fence.
    async fn decidable(
        &self,
        decision: Decision,
        revision: crate::storage::sync_state::OpaqueId,
        identity: Option<crate::auth::Identity>,
    ) -> Result<(), ControlError> {
        use crate::sync::deletion::Rechecked;
        self.scope
            .root
            .authorize_controls(self.scope.config.seed_project.clone(), identity)
            .await?;
        match crate::sync::deletion::recheck(
            self.scope.root,
            self.scope.config,
            revision,
            Interrupt::new(self.scope.stopping),
        )
        .await
        .map_err(ControlError::Failed)?
        {
            Rechecked::Interrupted => Err(ControlError::Shutdown),
            Rechecked::Superseded(error) => Err(self.refuse(decision, revision, error).await),
            // The decision itself refuses a set that is no longer the one waiting.
            Rechecked::Confirmed | Rechecked::Replaced | Rechecked::NotAwaiting => Ok(()),
        }
    }

    /// Refuses `decision` on the set `revision`, which `error` found no longer decidable, with
    /// what became of the set, read right after on this worker, which alone takes inventories,
    /// so that the answer can say what to do instead of trying again.
    async fn refuse(
        &self,
        decision: Decision,
        revision: crate::storage::sync_state::OpaqueId,
        error: SyncError,
    ) -> ControlError {
        match self.scope.root.refusal(revision).await {
            Ok(refusal) => ControlError::Stale {
                decision,
                revision,
                refusal,
                error,
            },
            Err(failed) => failed.into(),
        }
    }

    /// Records `decision` on the set `revision`. A set that no longer waits for it is refused
    /// with what became of it.
    async fn apply(
        &self,
        decision: Decision,
        revision: crate::storage::sync_state::OpaqueId,
    ) -> Result<crate::storage::sync_state::deletions::DeletionSet, ControlError> {
        let root = self.scope.root;
        let applied = match decision {
            Decision::Approve => root.approve_deletions(revision).await,
            Decision::Reject => root.reject_deletions(revision).await,
        };
        let error = match applied {
            Ok(set) => return Ok(set),
            Err(error) => error,
        };
        if !matches!(error, RootError::Admin(AdminError::Precondition)) {
            return Err(error.into());
        }
        Err(self.refuse(decision, revision, error.into()).await)
    }

    /// Approves or rejects the deletion set `revision`, as `decision` says, answers, and then
    /// runs the approved set or writes the rejected set's files back from their objects.
    async fn decide(
        &mut self,
        decision: Decision,
        revision: crate::storage::sync_state::OpaqueId,
        reply: Reply<crate::storage::sync_state::deletions::DeletionSet>,
        identity: Option<crate::auth::Identity>,
    ) -> Result<(), SyncError> {
        let root = self.scope.root;
        let decided = match self.decidable(decision, revision, identity).await {
            Ok(()) => self.apply(decision, revision).await,
            Err(error) => Err(error),
        };
        let set = decided.as_ref().ok().cloned();
        let _ = reply.send(decided);
        let Some(set) = set else {
            return Ok(());
        };
        let interrupt = Interrupt::new(self.scope.stopping);
        match decision {
            Decision::Approve => {
                crate::sync::deletion::execute_with(self.scope.catalog, root, set, interrupt)
                    .await?
                    .report(LogLevel::Info);
            }
            Decision::Reject => {
                crate::sync::deletion::write_back_rejected(self.scope.catalog, root, interrupt)
                    .await?;
            }
        }
        // The members held back the exports into their bucket's missing directory, which the
        // deletions or the write-back brought back and no tree change would run again.
        Box::pin(self.state.skipped.retry(self.scope, interrupt))
            .await
            .map(drop)
    }
}
