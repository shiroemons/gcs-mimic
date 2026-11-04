use super::*;
use crate::sync::control::{WaitFence, WaitReason, WaitResult};

impl LiveControl<'_> {
    pub(super) async fn wait(&mut self, deadline: tokio::time::Instant) -> io::Result<WaitResult> {
        loop {
            let result = self.wait_barrier(deadline).await?;
            if result.reason != WaitReason::SourceChanging {
                return Ok(result);
            }
            let mut stopping = self.stopping.clone();
            tokio::select! {
                _=tokio::time::sleep_until(deadline)=>return Ok(WaitResult::timeout()),
                _=stopping.changed()=>return Ok(WaitResult::result(WaitReason::Stopped,result.journal_fence,result.reconciliation)),
                ()=tokio::time::sleep(Duration::from_millis(super::super::SETTLE_MS))=>{},
            }
        }
    }

    async fn wait_barrier(&mut self, deadline: tokio::time::Instant) -> io::Result<WaitResult> {
        if tokio::time::Instant::now() >= deadline {
            return Ok(WaitResult::timeout());
        }
        let mut summary = ingest_external_with(
            self.catalog,
            self.root,
            self.config,
            self.writeback,
            Interrupt {
                stopping: self.stopping,
                deadline: Some(deadline),
            },
            self.ingest_complete,
            self.baseline_complete,
        )
        .await?;
        if summary.interrupted {
            return Ok(WaitResult::result(
                if *self.stopping.borrow() {
                    WaitReason::Stopped
                } else {
                    WaitReason::Timeout
                },
                None,
                Some(summary),
            ));
        }
        let fence = self
            .catalog
            .read_changes(None, None, 1)
            .await
            .map_err(io::Error::other)?
            .fence;
        #[cfg(test)]
        export_checkpoint("wait-fence").await;
        let writeback = self.writeback;
        let mut reason = WaitReason::Ready;
        while *self.cursor != fence {
            if *self.stopping.borrow() {
                reason = WaitReason::Stopped;
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                reason = WaitReason::Timeout;
                break;
            }
            let page = self
                .catalog
                .read_changes(Some(*self.cursor), Some(fence), 256)
                .await
                .map_err(io::Error::other)?;
            if page.rescan_required {
                reason = WaitReason::JournalChanged;
                break;
            }
            let next = page.next;
            let replay = replay_page_with(
                self.catalog,
                self.root,
                writeback,
                page,
                Interrupt {
                    stopping: self.stopping,
                    deadline: Some(deadline),
                },
            )
            .await?;
            summary.exported += replay.exported;
            summary.identical += replay.identical;
            summary.conflicting += replay.conflicting;
            summary.deferred += replay.deferred;
            if replay.interrupted {
                return Ok(WaitResult::result(
                    if *self.stopping.borrow() {
                        WaitReason::Stopped
                    } else {
                        WaitReason::Timeout
                    },
                    Some(WaitFence {
                        sequence: fence.sequence.get(),
                        epoch: fence.revision,
                    }),
                    Some(summary),
                ));
            }
            *self.cursor = next;
        }
        if reason == WaitReason::Ready {
            reason = self.observed_wait_reason(&summary, deadline).await?;
        }
        Ok(WaitResult::result(
            reason,
            Some(WaitFence {
                sequence: fence.sequence.get(),
                epoch: fence.revision,
            }),
            Some(summary),
        ))
    }

    async fn observed_wait_reason(
        &self,
        summary: &Reconciliation,
        deadline: tokio::time::Instant,
    ) -> io::Result<WaitReason> {
        let observation = self.root.observation().await.map_err(io::Error::other)?;
        let deletion = self.root.deletion_set().await.map_err(io::Error::other)?;
        Ok(if tokio::time::Instant::now() >= deadline {
            WaitReason::Timeout
        } else if summary.deferred > 0 {
            WaitReason::SourceChanging
        } else if !summary.baseline_complete {
            WaitReason::SourceUnrepresentable
        } else if deletion.is_some_and(|set| set.candidate_count > set.completed_count) {
            WaitReason::DeletionApproval
        } else if observation.mapping_findings > 0
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
        })
    }
}
