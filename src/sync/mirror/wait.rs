use super::gate::Hold;
use super::*;
use crate::sync::control::{WaitFindings, WaitReason, WaitResult};
use crate::sync::outstanding::{Outstanding, TreeSide};

impl LiveControl<'_> {
    pub(super) async fn wait(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<WaitResult, SyncError> {
        loop {
            let result = self.wait_barrier(deadline).await?;
            if result.reason != WaitReason::SourceChanging {
                return Ok(result);
            }
            let mut stopping = self.scope.stopping.clone();
            tokio::select! {
                _=tokio::time::sleep_until(deadline)=>return Ok(WaitResult::timeout()),
                _=stopping.changed()=>return Ok(WaitResult::result(WaitReason::Stopped,result.journal_fence,result.reconciliation)),
                ()=tokio::time::sleep(Duration::from_millis(super::super::SETTLE_MS))=>{},
            }
        }
    }

    /// Why an interrupted wait ended: the server is stopping, or the wait's deadline passed.
    fn interruption(&self) -> WaitReason {
        if *self.scope.stopping.borrow() {
            WaitReason::Stopped
        } else {
            WaitReason::Timeout
        }
    }

    async fn wait_barrier(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<WaitResult, SyncError> {
        if tokio::time::Instant::now() >= deadline {
            return Ok(WaitResult::timeout());
        }
        let interrupt = Interrupt {
            stopping: self.scope.stopping,
            deadline: Some(deadline),
        };
        let mut summary = ingest_external_with(
            self.scope,
            interrupt,
            &mut self.state.baseline,
            &mut self.state.skipped,
        )
        .await?;
        if summary.interrupted {
            return Ok(WaitResult::result(self.interruption(), None, Some(summary)));
        }
        #[cfg(test)]
        export_checkpoint("wait-ingested").await;
        let fence = self.scope.catalog.current_fence().await?;
        #[cfg(test)]
        export_checkpoint("wait-fence").await;
        let mut reason = WaitReason::Ready;
        while self.state.cursor != fence {
            if *self.scope.stopping.borrow() {
                reason = WaitReason::Stopped;
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                reason = WaitReason::Timeout;
                break;
            }
            let page = match self
                .scope
                .catalog
                .read_changes(self.state.cursor, Some(fence), 256)
                .await?
            {
                ChangeRead::Rescan { .. } => {
                    reason = WaitReason::JournalChanged;
                    break;
                }
                ChangeRead::Page(page) => page,
            };
            let next = page.next;
            let replay = replay_page_with(
                self.scope.catalog,
                self.scope.root,
                self.scope.writeback,
                page,
                interrupt,
                &mut self.state.skipped,
            )
            .await?;
            summary.exported += replay.exported;
            summary.identical += replay.identical;
            summary.conflicting += replay.conflicting;
            summary.deferred += replay.deferred;
            if replay.interrupted {
                return Ok(WaitResult::result(
                    self.interruption(),
                    Some(fence.into()),
                    Some(summary),
                ));
            }
            self.state.cursor = next;
        }
        let (reason, findings) = if reason == WaitReason::Ready {
            let (reason, findings) = self.observed_wait_reason(&summary, deadline).await?;
            (reason, Some(findings))
        } else {
            (reason, None)
        };
        Ok(WaitResult::result(reason, Some(fence.into()), Some(summary)).with_findings(findings))
    }

    /// Decides a wait whose scan and replay finished, from `summary`, this pass's counts, and
    /// the observation it reads, through the judgment the status shares. The gate that decides
    /// deletion detection decides the tree side from this pass, since the baseline stays complete
    /// once reached; the observation decides what the catalog still holds. A name the tree cannot
    /// hold does not stop the wait: the object stays reachable through the API, and the findings
    /// it returns count it.
    pub(super) async fn observed_wait_reason(
        &self,
        summary: &Reconciliation,
        deadline: tokio::time::Instant,
    ) -> Result<(WaitReason, WaitFindings), SyncError> {
        let observation = self.scope.root.observation().await?;
        let deletion = self.scope.root.deletion_set().await?;
        let findings = WaitFindings {
            unmapped: observation.unmapped_findings,
            observation_revision: observation.revision,
        };
        if tokio::time::Instant::now() >= deadline {
            return Ok((WaitReason::Timeout, findings));
        }
        let tree = match summary.gate().hold() {
            Hold::Interrupted => return Ok((self.interruption(), findings)),
            Hold::Deferred => TreeSide::Changing,
            Hold::Conflicting => TreeSide::Conflicting,
            Hold::TreeEntries | Hold::Unimported => TreeSide::Unrepresentable,
            Hold::BaselineOpen => TreeSide::Observing,
            // A skipped export runs again only once the tree settles, and one that a retry in
            // this pass skipped again counts nothing.
            Hold::Clear if !self.state.skipped.is_empty() => TreeSide::Changing,
            // Exports held for approval stay queued, but the tree owes nothing else: the guarded
            // set the inventory offered, which the judgment reads as a deletion approval, is
            // what they wait for.
            Hold::AwaitingApproval | Hold::Clear => TreeSide::Settled,
        };
        let outstanding = Outstanding::new(&observation, deletion.as_ref(), tree);
        Ok((WaitReason::from(outstanding.first()), findings))
    }
}
