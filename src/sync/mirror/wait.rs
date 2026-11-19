use super::gate::Hold;
use super::*;
use crate::sync::control::{Unfinished, WaitFindings, WaitReason, WaitResult};
use crate::sync::outstanding::{Holdup, Outstanding, TreeSide};

impl LiveControl<'_> {
    /// Waits until a pass leaves nothing in progress or `deadline` passes. A wait that runs out
    /// of time answers with its last finished pass and what that pass was still waiting for, so
    /// that its answer names what remained.
    pub(super) async fn wait(
        &mut self,
        deadline: tokio::time::Instant,
    ) -> Result<WaitResult, SyncError> {
        let mut last: Option<WaitResult> = None;
        loop {
            let result = self.wait_barrier(deadline).await?;
            // A pass the deadline cut short knows less than the finished one before it.
            let result = match (result.reason, last.take()) {
                (WaitReason::Timeout, Some(previous)) => {
                    return Ok(previous.cut_short(Unfinished::Timeout));
                }
                (_, _) => result,
            };
            if result.reason != WaitReason::SourceChanging {
                return Ok(result);
            }
            let mut stopping = self.scope.stopping.clone();
            tokio::select! {
                _=tokio::time::sleep_until(deadline)=>return Ok(result.cut_short(Unfinished::Timeout)),
                _=stopping.changed()=>return Ok(result.cut_short(Unfinished::Stopped)),
                ()=tokio::time::sleep(Duration::from_millis(super::super::SETTLE_MS))=>{},
            }
            last = Some(result);
        }
    }

    /// Why an interrupted wait ended: the server is stopping, or the wait's deadline passed.
    fn interruption(&self) -> Unfinished {
        if *self.scope.stopping.borrow() {
            Unfinished::Stopped
        } else {
            Unfinished::Timeout
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
            return Ok(WaitResult::unfinished(
                self.interruption(),
                None,
                Some(summary),
            ));
        }
        #[cfg(test)]
        export_checkpoint("wait-ingested").await;
        let fence = self.scope.catalog.current_fence().await?;
        #[cfg(test)]
        export_checkpoint("wait-fence").await;
        let mut cut = None;
        while self.state.cursor != fence {
            if *self.scope.stopping.borrow() {
                cut = Some(Unfinished::Stopped);
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                cut = Some(Unfinished::Timeout);
                break;
            }
            let page = match self
                .scope
                .catalog
                .read_changes(self.state.cursor, Some(fence), 256)
                .await?
            {
                ChangeRead::Rescan { .. } => {
                    cut = Some(Unfinished::JournalChanged);
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
            summary.absorb_deferred(&replay);
            if replay.interrupted {
                return Ok(WaitResult::unfinished(
                    self.interruption(),
                    Some(fence.into()),
                    Some(summary),
                ));
            }
            self.state.cursor = next;
        }
        if let Some(why) = cut {
            return Ok(WaitResult::unfinished(
                why,
                Some(fence.into()),
                Some(summary),
            ));
        }
        let (decided, findings) = self.observed_wait_reason(&summary, deadline).await?;
        let result = match decided {
            Decided::Holdup(holdup) => WaitResult::held(holdup, Some(fence.into()), Some(summary)),
            Decided::Ended(why) => WaitResult::unfinished(why, Some(fence.into()), Some(summary)),
        };
        Ok(result.with_findings(Some(findings)))
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
    ) -> Result<(Decided, WaitFindings), SyncError> {
        let observation = self.scope.root.observation().await?;
        let deletion = self.scope.root.deletion_set().await?;
        let findings = WaitFindings {
            unmapped: observation.unmapped_findings,
            observation_revision: observation.revision,
        };
        if tokio::time::Instant::now() >= deadline {
            return Ok((Decided::Ended(Unfinished::Timeout), findings));
        }
        let hold = summary.gate().hold();
        let tree = match (hold, hold.tree_side()) {
            (_, None) => return Ok((Decided::Ended(self.interruption()), findings)),
            // A skipped export runs again only once the tree settles, and one that a retry in
            // this pass skipped again counts nothing. Exports held for approval stay queued too,
            // but they wait for the approval, not for the tree.
            (Hold::Clear, Some(TreeSide::Settled)) if !self.state.skipped.is_empty() => {
                TreeSide::Changing
            }
            (_, Some(side)) => side,
        };
        let outstanding = Outstanding::new(&observation, deletion.as_ref(), tree);
        Ok((Decided::Holdup(outstanding.first()), findings))
    }
}

/// How a wait whose pass finished decided: by the first condition that holds, or because it
/// ran out of time or the server is stopping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Decided {
    Holdup(Holdup),
    Ended(Unfinished),
}
