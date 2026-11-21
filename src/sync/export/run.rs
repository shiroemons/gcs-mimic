//! Running the write-back of an export.

use super::{
    Content, LiveObject, LivePlan, SkippedExport,
    retry::{EXPORT_BACKOFF, advance_with_retry, concurrent_change, retryable, wait_retry},
};
#[cfg(test)]
use crate::sync::mirror::{LIVE_EXPORTS, export_checkpoint};
use crate::{
    diagnostics::{self, LogLevel},
    storage::{
        admin::AdminError,
        change_journal::JournalFence,
        sync_state::writeback::{self, InstallOutcome, LiveExport, LiveSource, Writeback},
    },
    sync::{SyncError, report::remedy::newer_edit_remedy},
};
use std::{fs::File, io};

impl LiveObject<'_> {
    /// Exports `plan`; `Some` is the export, skipped because preserving the file it would replace
    /// failed or that file changed again after preservation, or because the bucket's directory
    /// is held for a deletion approval, for a retry to finish.
    pub(in crate::sync) async fn export(
        &mut self,
        writeback: &Writeback,
        fence: JournalFence,
        mut plan: LivePlan,
    ) -> Result<Option<SkippedExport>, SyncError> {
        let (bucket, name) = (self.bucket, self.name);
        // Write-back creates the directories above the file it installs, and even a removal opens
        // them, so any export would bring the held directory back.
        if crate::sync::deletion::bucket_directory_held(self.root, bucket).await? {
            self.summary.hold_for_approval();
            return Ok(plan.skipped());
        }
        #[cfg(test)]
        export_checkpoint("after-get-path").await;
        let Some(mut operation) = self.begin_export(writeback, fence, &plan, None).await? else {
            self.defer();
            return Ok(None);
        };
        #[cfg(test)]
        export_checkpoint("after-prepare").await;
        #[cfg(test)]
        export_checkpoint("before-arm").await;
        #[cfg(test)]
        export_checkpoint("before-publish").await;
        let mut preserved = false;
        // An installed export stops once before it records durability and once before its
        // acknowledgement, the steps an external change can come between; a retry after
        // preservation passes them again without stopping, since the hook releases once.
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        let (mut durable_checkpoint, mut ack_checkpoint) = (false, false);
        loop {
            match advance_with_retry(writeback, operation.clone()).await? {
                InstallOutcome::Pending => {
                    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
                    {
                        let phase = writeback::live_pending(&operation).await.phase;
                        if phase == writeback::Phase::Armed && !durable_checkpoint {
                            durable_checkpoint = true;
                            export_checkpoint("before-durable").await;
                        } else if phase == writeback::Phase::Durable && !ack_checkpoint {
                            ack_checkpoint = true;
                            export_checkpoint("before-ack").await;
                        }
                    }
                }
                InstallOutcome::Complete => {
                    if plan.content == Content::Identical {
                        self.summary.identical += 1;
                    } else {
                        #[cfg(test)]
                        let _ = LIVE_EXPORTS.try_with(|count| {
                            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                        });
                        self.summary.exported += 1;
                    }
                    break;
                }
                InstallOutcome::Obsolete => {
                    self.defer();
                    break;
                }
                // A user's change to the tree overtook the installation, which cancelled. No API
                // change brings this export back, so it is kept as skipped, and the end of the
                // pass plans it again from what the tree holds then; the rest of the root keeps
                // synchronizing meanwhile.
                InstallOutcome::Raced => {
                    self.defer();
                    return Ok(plan.skipped());
                }
                InstallOutcome::Collision(obstruction) => {
                    self.root
                        .record_finding(
                            plan.binding.clone(),
                            name.clone(),
                            plan.generation,
                            obstruction.into(),
                        )
                        .await?;
                    // The object is what the tree cannot hold, as for the collision a target
                    // check finds: the directory's own entries are scanned like any other, and
                    // a tracked file it replaced stops only its own path in the inventory.
                    self.summary.count_unmapped_object();
                    break;
                }
                InstallOutcome::Conflict => {
                    self.summary.conflicting += 1;
                    if !preserved {
                        let Some(retry) = self.preserve_export(writeback, fence, &mut plan).await?
                        else {
                            self.defer();
                            return Ok(plan.skipped());
                        };
                        operation = retry;
                        preserved = true;
                        continue;
                    }
                    self.root
                        .record_export_conflict(plan.binding.clone(), name.clone(), plan.generation)
                        .await?;
                    diagnostics::emit(
                        LogLevel::Warn,
                        "sync_external_conflict",
                        serde_json::json!({"bucket":bucket.as_str(),"object":name.as_str(),"remedy":crate::sync::report::command::logged(&newer_edit_remedy())}),
                    );
                    // Ingestion publishes the newer edit only while the tree tracks the API
                    // generation; otherwise the rescan the remedy names runs this export again.
                    return Ok(plan.skipped());
                }
            }
        }
        Ok(None)
    }

    async fn begin_export(
        &self,
        writeback: &Writeback,
        fence: JournalFence,
        plan: &LivePlan,
        captured: Option<crate::storage::sync_state::conflicts::CapturedConflict>,
    ) -> Result<Option<LiveExport>, SyncError> {
        let mut retry = EXPORT_BACKOFF;
        let mut protected = false;
        loop {
            let source = if protected {
                plan.source
                    .as_ref()
                    .map(File::try_clone)
                    .transpose()?
                    .map_or(
                        LiveSource::ProtectedObservation,
                        LiveSource::ProtectedRestore,
                    )
            } else {
                live_source(plan.source.as_ref(), captured.clone())?
            };
            match writeback
                .begin_live_export(
                    plan.binding.clone(),
                    self.name.clone(),
                    plan.generation,
                    plan.tracked.clone(),
                    fence,
                    source,
                )
                .await
            {
                Ok(operation) => return Ok(Some(operation)),
                // A broken invariant ends as a race does, as it did when it was reported as a
                // failed precondition, and leaves its record.
                Err(writeback::Error::Invariant(broken)) => {
                    writeback::record_invariant(broken);
                    return Ok(None);
                }
                Err(error) if concurrent_change(&error) => return Ok(None),
                Err(writeback::Error::Admin(AdminError::Hold))
                    if !protected && captured.is_none() =>
                {
                    protected = true;
                }
                Err(error) if retryable(&error) => {
                    if !wait_retry(&error, &mut retry, self.root).await {
                        return Err(error.into());
                    }
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

    async fn preserve_export(
        &self,
        writeback: &Writeback,
        fence: JournalFence,
        plan: &mut LivePlan,
    ) -> Result<Option<LiveExport>, SyncError> {
        let expected_generation = plan.api_generation();
        let captured = match self
            .root
            .capture_path(crate::storage::sync_state::conflicts::CaptureRequest {
                binding: plan.binding.clone(),
                name: self.name.clone(),
                expected_generation,
                source_op: None,
            })
            .await
        {
            Ok(captured) => captured,
            Err(
                crate::storage::sync_state::conflicts::Error::TargetChanged
                | crate::storage::sync_state::conflicts::Error::Admin(
                    AdminError::Precondition | AdminError::NotFound,
                ),
            ) => return Ok(None),
            Err(crate::storage::sync_state::conflicts::Error::Admin(AdminError::Io(cause)))
                if cause.kind() == io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        diagnostics::emit(
            LogLevel::Warn,
            "sync_conflict_preserved",
            serde_json::json!({"bucket":self.bucket.as_str(),"object":self.name.as_str(),"conflict_id":captured.id(),"remedy":"Inspect this retained revision before restoring it or explicitly discarding it"}),
        );
        if expected_generation.is_some() && plan.source.is_none() {
            match self
                .root
                .sync_admin()
                .open_object(self.bucket, self.name, expected_generation)
                .await
            {
                Ok((_, reader)) => plan.source = Some(reader.into_file()?),
                Err(AdminError::Precondition | AdminError::NotFound) => return Ok(None),
                Err(error) => return Err(error.into()),
            }
        }
        self.begin_export(writeback, fence, plan, Some(captured))
            .await
    }
}

fn live_source(
    source: Option<&File>,
    captured: Option<crate::storage::sync_state::conflicts::CapturedConflict>,
) -> Result<LiveSource, SyncError> {
    let source = source.map(File::try_clone).transpose()?;
    Ok(match captured {
        Some(conflict) => LiveSource::Preserved {
            source,
            conflict: Box::new(conflict),
        },
        None => source.map_or(LiveSource::Observe, LiveSource::ApiCopy),
    })
}
