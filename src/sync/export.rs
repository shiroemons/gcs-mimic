//! Exporting the live state of objects to the working tree.

use crate::{
    object::ObjectRecord,
    storage::{
        Catalog,
        admin::{BucketName, Generation, ObjectName},
        change_journal::JournalFence,
        sync_state::{SyncBinding, SyncRecord, root::RootLease, writeback::Writeback},
    },
    sync::{
        Reconciliation, SyncError,
        failure::{Recorded, unless_superseded},
        mapping::{LexicalReason, Unplaced},
    },
};
use std::{fs::File, path::Path};
mod alias;
mod plan;
pub(super) mod reconcile;
pub(super) mod restore;
pub(super) mod retry;
mod run;
mod skipped;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
pub(super) use skipped::{SkippedExport, SkippedExports};

/// Exports one object's live state; `Some` is an export skipped for a retry to finish.
pub(in crate::sync) async fn reconcile_live_object(
    catalog: &Catalog,
    root: &RootLease,
    writeback: &Writeback,
    bucket: &BucketName,
    name: &ObjectName,
    fence: JournalFence,
    summary: &mut Reconciliation,
) -> Result<Option<SkippedExport>, SyncError> {
    let mut object = LiveObject {
        catalog,
        root,
        bucket,
        name,
        summary,
        occasion: Occasion::ApiChange,
    };
    let Some(plan) = object.plan().await? else {
        return Ok(None);
    };
    object.export(writeback, fence, plan).await
}

/// One object whose catalog state a live export reconciles with the working tree.
pub(in crate::sync) struct LiveObject<'a> {
    pub(in crate::sync) catalog: &'a Catalog,
    pub(in crate::sync) root: &'a RootLease,
    pub(in crate::sync) bucket: &'a BucketName,
    pub(in crate::sync) name: &'a ObjectName,
    /// The counts of the pass this object belongs to.
    pub(in crate::sync) summary: &'a mut Reconciliation,
    pub(in crate::sync) occasion: Occasion,
}

/// Why an export observes its object, which decides what a failure to read its target does,
/// what a missing tracked file means, and whether a file that differs is exported over.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::sync) enum Occasion {
    /// A journal change, a reconciliation or the retry of a skipped export. A failed read is
    /// retried and then fails the pass, since the object's change would otherwise be lost.
    ApiChange,
    /// A finding observed again by an ingestion. A failed read leaves the finding as it is for a
    /// later pass: the entry the read failed on is the scan's to report, and the object's own
    /// changes still come through the journal. No API change brings this export about, so it
    /// leaves a file that differs from the generation the tree tracks to ingestion.
    Reevaluate,
    /// The restore after a protected or rejected deletion, which writes a missing tracked file
    /// back. A failed read is retried and then fails the pass.
    Restore,
}

impl Occasion {
    /// Whether a missing tracked file of an API generation that has not changed since is left to
    /// the deletion inventory once the baseline is complete, rather than written back. The file
    /// was deleted from the tree, whether the mirror ran, was paused or the server was stopped
    /// meanwhile; writing it back would undo that deletion without asking anyone. Only the
    /// restore that a protected or rejected deletion asks for writes it back.
    const fn leaves_missing_to_inventory(self) -> bool {
        match self {
            Self::ApiChange | Self::Reevaluate => true,
            Self::Restore => false,
        }
    }

    /// Whether this export leaves a file that differs from the generation the tree tracks,
    /// which then holds an edit, to ingestion rather than planning an export over it: no API
    /// change brings a reevaluation about.
    const fn leaves_edits_to_ingestion(self) -> bool {
        match self {
            Self::Reevaluate => true,
            Self::ApiChange | Self::Restore => false,
        }
    }
}

/// Whether the working-tree file already holds the catalog generation's bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Content {
    Identical,
    Changed,
}

/// A live export decided from one read of the binding, tracked path and live object.
pub(in crate::sync) struct LivePlan {
    binding: SyncBinding,
    tracked: Option<SyncRecord>,
    generation: Generation,
    source: Option<File>,
    content: Content,
}

impl LivePlan {
    /// The API generation this export brings to the tree; `None` removes the tracked file.
    fn api_generation(&self) -> Option<Generation> {
        // Only a removal has neither a body to copy nor an identical one to observe.
        (self.content == Content::Identical || self.source.is_some()).then_some(self.generation)
    }

    /// This export as a skipped one to run again; `None` when the tree already tracks the API
    /// generation, because a divergent file there is ingestion's to publish.
    fn skipped(&self) -> Option<SkippedExport> {
        let skipped = SkippedExport {
            api: self.api_generation(),
            tracked: self.tracked.as_ref().map(|record| record.generation),
        };
        (skipped.api != skipped.tracked).then_some(skipped)
    }
}

impl LiveObject<'_> {
    /// Counts this export as left for a later pass, named by the path the object maps to most
    /// plainly, which a wait that ends meanwhile names.
    fn defer(&mut self) {
        let path = Path::new(self.bucket.as_str()).join(self.name.as_str());
        self.summary.count_deferred(&path);
    }

    /// Records why this object stays out of the working tree and counts it. A generation that
    /// changed or went since this export read it is the next replay's to record.
    async fn finding(&mut self, record: &ObjectRecord, reason: Unplaced) -> Result<(), SyncError> {
        let binding = self.catalog.sync_state().binding(self.bucket).await?;
        let recorded = unless_superseded(
            self.root
                .record_finding(
                    binding,
                    self.name.clone(),
                    Generation::new(record.generation)?,
                    reason,
                )
                .await,
        )?;
        if recorded == Recorded::Superseded {
            return Ok(());
        }
        if reason == Unplaced::Unmapped(LexicalReason::Ignored) {
            self.summary.ignored += 1;
        } else {
            self.summary.count_unmapped_object();
        }
        Ok(())
    }
}
