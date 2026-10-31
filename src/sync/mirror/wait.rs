use super::*;
use crate::sync::control::{WaitFence, WaitResult};

impl LiveControl<'_> {
    pub(super) async fn wait(&mut self, deadline: tokio::time::Instant) -> io::Result<WaitResult> {
        loop {
            let result = self.wait_barrier(deadline).await?;
            if result.reason != "SourceChanging" {
                return Ok(result);
            }
            let mut stopping = self.stopping.clone();
            tokio::select! {
                _=tokio::time::sleep_until(deadline)=>return Ok(WaitResult::timeout()),
                _=stopping.changed()=>return Ok(WaitResult {completed:false,timed_out:false,journal_fence:result.journal_fence,reconciliation:result.reconciliation,reason:"Stopped"}),
                ()=tokio::time::sleep(Duration::from_millis(super::super::SETTLE_MS))=>{},
            }
        }
    }

    async fn wait_barrier(&mut self, deadline: tokio::time::Instant) -> io::Result<WaitResult> {
        if tokio::time::Instant::now() >= deadline {
            return Ok(WaitResult::timeout());
        }
        let mut summary = ingest_external(
            self.catalog,
            self.root,
            self.config,
            self.stopping,
            self.ingest_complete,
            self.baseline_complete,
        )
        .await?;
        let fence = self
            .catalog
            .read_changes(None, None, 1)
            .await
            .map_err(io::Error::other)?
            .fence;
        #[cfg(test)]
        export_checkpoint("wait-fence").await;
        let writeback = self
            .root
            .writeback(
                Limits::new(
                    super::super::CONFLICT_MAX_BYTES,
                    super::super::CONFLICT_MAX_ENTRIES,
                )
                .map_err(io::Error::other)?,
            )
            .await
            .map_err(io::Error::other)?;
        let mut reason = "Ready";
        while *self.cursor != fence {
            if *self.stopping.borrow() {
                reason = "Stopped";
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                reason = "Timeout";
                break;
            }
            let page = self
                .catalog
                .read_changes(Some(*self.cursor), Some(fence), 256)
                .await
                .map_err(io::Error::other)?;
            if page.rescan_required {
                reason = "JournalChanged";
                break;
            }
            let next = page.next;
            let replay =
                replay_page(self.catalog, self.root, &writeback, page, self.stopping).await?;
            summary.exported += replay.exported;
            summary.identical += replay.identical;
            summary.conflicting += replay.conflicting;
            summary.deferred += replay.deferred;
            *self.cursor = next;
        }
        let observation = self.root.observation().await.map_err(io::Error::other)?;
        let deletion = self.root.deletion_set().await.map_err(io::Error::other)?;
        if reason == "Ready" {
            reason = if tokio::time::Instant::now() >= deadline {
                "Timeout"
            } else if summary.deferred > 0 {
                "SourceChanging"
            } else if !summary.baseline_complete {
                "SourceUnrepresentable"
            } else if deletion.is_some_and(|set| set.candidate_count > set.completed_count) {
                "DeletionApproval"
            } else if observation.mapping_findings > 0
                || observation.unresolved_conflicts > 0
                || observation.path_stops > 0
                || observation.pending_writeback > 0
                || observation
                    .writeback
                    .as_ref()
                    .is_some_and(|value| value.state != "Ready")
            {
                "FindingsPending"
            } else {
                "Ready"
            };
        }
        Ok(WaitResult {
            completed: reason == "Ready",
            timed_out: reason == "Timeout",
            journal_fence: Some(WaitFence {
                sequence: fence.sequence.0,
                epoch: fence.revision,
            }),
            reconciliation: Some(summary),
            reason,
        })
    }
}
