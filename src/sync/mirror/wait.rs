use super::*;
use crate::sync::control::{WaitFindings, WaitReason, WaitResult};

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
    /// the observation it reads. The scan's own counts decide the tree, since the baseline stays
    /// complete once reached; the observation decides what the catalog still holds. A name the
    /// tree cannot hold does not stop the wait: the object stays reachable through the API, and
    /// the findings it returns count it.
    pub(super) async fn observed_wait_reason(
        &self,
        summary: &Reconciliation,
        deadline: tokio::time::Instant,
    ) -> Result<(WaitReason, WaitFindings), SyncError> {
        let observation = self.scope.root.observation().await?;
        let deletion = self.scope.root.deletion_set().await?;
        let reason = if tokio::time::Instant::now() >= deadline {
            WaitReason::Timeout
        } else if summary.deferred > 0 || !self.state.skipped.is_empty() {
            // A skipped export runs again only once the tree settles, and one that a retry in
            // this pass skipped again counts nothing.
            WaitReason::SourceChanging
        } else if summary.unrepresentable > 0 {
            WaitReason::SourceUnrepresentable
        } else if deletion.is_some_and(|set| set.candidate_count > set.completed_count) {
            WaitReason::DeletionApproval
        } else if observation.blocking_findings > 0
            || observation.unresolved_conflicts > 0
            || observation.path_stops > 0
            || observation.pending_writeback > 0
            || observation
                .writeback
                .as_ref()
                .is_some_and(|value| value.state != Stop::Ready)
        {
            WaitReason::FindingsPending
        } else {
            WaitReason::Ready
        };
        let findings = WaitFindings {
            unmapped: observation.unmapped_findings,
            observation_revision: observation.revision,
        };
        Ok((reason, findings))
    }
}
