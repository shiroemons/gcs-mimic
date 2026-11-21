//! The order of the stages of one live pass, and the ingestion of external changes.

use super::*;
use crate::{
    config::Config,
    diagnostics::{self, LogLevel},
    storage::{
        Catalog,
        sync_state::{
            root::RootLease,
            writeback::{Stop, Writeback},
        },
    },
    sync::{
        Reconciliation, SyncError,
        interrupt::{Interrupt, Interrupted},
    },
};

pub(super) struct Initialized {
    pub(super) state: LiveState,
    pub(super) watcher: DirectoryWatch,
    pub(super) writeback: Writeback,
}

pub(super) async fn initialize_live(
    run: LiveRun<'_>,
    stopping: &tokio::sync::watch::Receiver<bool>,
) -> Result<Initialized, SyncError> {
    let LiveRun {
        catalog,
        root,
        config,
        status,
        resume,
    } = run;
    let initial_root = resume
        .as_ref()
        .map(|request| root.with_authority(request.authority.clone()));
    let operation_root = initial_root.as_ref().unwrap_or(root);
    catalog.enable_change_journal().await?;
    let cursor = catalog.current_fence().await?;
    let watcher = DirectoryWatch::new(root).await?;
    let mut writeback = recover_writeback(operation_root).await?;
    crate::sync::naming::activate(&writeback).await?;
    // Before the inventory, which would take the files a rejection has not written back yet,
    // after a stop, a failure or a power loss, for files the user deleted.
    crate::sync::deletion::write_back_rejected(catalog, operation_root, Interrupt::new(stopping))
        .await?;
    root.invalidate_deletions().await?;
    let ingested = crate::sync::ingest::rescan_with(
        catalog,
        operation_root,
        config,
        &writeback,
        Interrupt::new(stopping),
    )
    .await?;
    diagnostics::emit(LogLevel::Info, "sync_ingested", &ingested);
    status.entries().report_pass(&ingested);
    let ingest_complete = BaselineGate::after_ingest(ingested.gate());
    let mut skipped = SkippedExports::default();
    let mut summary = reconcile_with(
        catalog,
        operation_root,
        Interrupt::new(stopping),
        ingest_complete,
        &writeback,
        &mut skipped,
    )
    .await?;
    summary.ingested = ingested.ingested;
    let baseline_complete = summary.baseline_complete;
    diagnostics::emit(LogLevel::Info, "sync_reconciled", &summary);
    observe_deletions(
        catalog,
        operation_root,
        config,
        summary.gate(),
        Interrupt::new(stopping),
        Inventorying::RunStart,
    )
    .await?;
    // Running only now, after this run reconciled the whole tree: until then a starting mirror
    // stays starting, a retrying one retrying and a paused one paused, even while its explicit
    // rescan runs.
    let reconciled = if resume.is_some() {
        Reconciled::Rescan
    } else {
        Reconciled::Run
    };
    status.reconciled(root, reconciled).await?;
    if let Some(request) = resume.take() {
        let _ = request.reply.send(Ok(summary));
        writeback = writeback.with_root(root)?;
    }
    #[cfg(test)]
    export_checkpoint("observation-ready").await;
    Ok(Initialized {
        state: LiveState {
            cursor,
            baseline: BaselineProgress {
                gate: ingest_complete,
                complete: baseline_complete,
            },
            skipped,
        },
        watcher,
        writeback,
    })
}

pub(super) async fn recover_writeback(root: &RootLease) -> Result<Writeback, SyncError> {
    let writeback = root.default_writeback().await?;
    writeback.recover_pending().await?;
    let recovered = writeback.preserve_protected().await?;
    if recovered.stop() != Stop::Ready {
        return Err(SyncError::Recovery(recovered.stop()));
    }
    Ok(writeback)
}

pub(super) async fn ingest_external(
    scope: LiveScope<'_>,
    baseline: &mut BaselineProgress,
    skipped: &mut SkippedExports,
) -> Result<Reconciliation, SyncError> {
    ingest_external_with(scope, Interrupt::new(scope.stopping), baseline, skipped).await
}
pub(super) async fn ingest_external_with(
    scope: LiveScope<'_>,
    interrupt: Interrupt<'_>,
    baseline: &mut BaselineProgress,
    skipped: &mut SkippedExports,
) -> Result<Reconciliation, SyncError> {
    let LiveScope {
        catalog,
        root,
        config,
        writeback,
        entries,
        ..
    } = scope;
    let _publication = root.begin_observation_publication().await?;
    // A rejection the run could not finish waits in the catalog; see `initialize_live`.
    crate::sync::deletion::write_back_rejected(catalog, root, interrupt).await?;
    // Before the scan, which then finds the exported files tracked as they are. Boxed like the
    // reconciliation below, which also runs exports inside this pass.
    let observed = Box::pin(skipped.observe_findings(scope, interrupt)).await?;
    let mut summary =
        crate::sync::ingest::rescan_with(catalog, root, config, writeback, interrupt).await?;
    summary.absorb_exports(&observed);
    summary.interrupted |= observed.interrupted;
    // The deleted object's file goes and the heir gets its own in the pass that took the edit
    // in, since no API change of another origin than ingestion follows to observe them.
    let heirs = std::mem::take(&mut summary.heirs);
    if !heirs.is_empty() {
        let settled = Box::pin(skipped.observe_heirs(scope, &heirs, interrupt)).await?;
        summary.absorb_exports(&settled);
        summary.interrupted |= settled.interrupted;
    }
    entries.report_pass(&summary);
    baseline.gate = BaselineGate::after_ingest(summary.gate());
    if !baseline.complete
        && baseline.gate == BaselineGate::Allowed
        && interrupt.check() == Interrupted::No
    {
        // Nested control rescans otherwise retain reconciliation state on the observer's stack.
        let merged = Box::pin(reconcile_with(
            catalog,
            root,
            interrupt,
            BaselineGate::Allowed,
            writeback,
            skipped,
        ))
        .await?;
        baseline.complete = merged.baseline_complete;
        // What kept the reconciliation from completing the baseline is this pass's hold too, so
        // a wait names it; with nothing of its own, only an API change during it remains.
        summary.absorb_hold(&merged);
    }
    root.revalidate().await?;
    summary.baseline_complete = baseline.complete;
    summary.interrupted |= interrupt.check() != Interrupted::No;
    if summary.interrupted {
        return Ok(summary);
    }
    observe_deletions(
        catalog,
        root,
        config,
        summary.gate(),
        interrupt,
        Inventorying::Running,
    )
    .await?;
    // Boxed like the reconciliation above, which also runs exports inside this pass.
    let retried = Box::pin(skipped.retry(scope, interrupt)).await?;
    summary.carry_held(&retried);
    summary.interrupted |= interrupt.check() != Interrupted::No;
    Ok(summary)
}

/// Which inventory of a run deletes objects, which decides how loudly the log names them.
#[derive(Clone, Copy, Debug)]
enum Inventorying {
    /// The first of a run, after a start, a retry or a resumed rescan. It deletes what was
    /// deleted from the tree while the mirror was not watching, such as while the server was
    /// stopped, which nobody may expect, so it warns.
    RunStart,
    /// A later one, which deletes what was just deleted from the tree.
    Running,
}

impl Inventorying {
    fn applied_level(self) -> LogLevel {
        match self {
            Self::RunStart => LogLevel::Warn,
            Self::Running => LogLevel::Info,
        }
    }
}

async fn observe_deletions(
    catalog: &Catalog,
    root: &RootLease,
    config: &Config,
    pass: PassGate,
    interrupt: Interrupt<'_>,
    inventorying: Inventorying,
) -> Result<(), SyncError> {
    let inventory = crate::sync::deletion::inventory_with(root, config, pass, interrupt).await;
    match inventory {
        Ok(Some(set)) => {
            crate::sync::deletion::execute_with(catalog, root, set, interrupt)
                .await?
                .report(inventorying.applied_level());
        }
        Ok(None) => {}
        // An API change moved the journal, or a bucket or the root changed, during the
        // inventory. A change that writes the tree raises native events for those writes, which
        // nothing filters out as the mirror's own, and the pass they bring inventories again. A
        // change to metadata alone writes nothing, so the missing files wait for the next change
        // to the tree, a rescan or a wait, and the log says why they still wait. The deletions
        // of a set end on such a change by themselves and leave it to the next inventory too.
        Err(error) if error.is_stale_deletion() => {
            diagnostics::emit(
                LogLevel::Info,
                "sync_deletion_inventory_superseded",
                serde_json::json!({
                    "reason":"An API or bucket change during the deletion inventory made it stale",
                    "next":"The next change to the directory, a rescan or a wait inventories the missing files again",
                }),
            );
        }
        Err(error) => return Err(error),
    }
    Ok(())
}

/// What the live observer knows about the baseline: whether the latest ingest lets a
/// reconciliation complete it, and whether one already has.
#[derive(Clone, Copy, Debug)]
pub(super) struct BaselineProgress {
    pub(super) gate: BaselineGate,
    pub(super) complete: bool,
}
