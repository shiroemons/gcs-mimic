//! Findings whose cause can go without a journal change of their own object, such as another
//! API object or an entry in the tree that stands in its way. Each ingestion observes those
//! objects again and exports the ones whose cause went; a finding that still holds only reads.
//! An API change can remove the cause too: deleting an object ends an alias it belonged to and
//! a collision with the objects above or below its name, so a replay observes those again.
use crate::storage::sync_state::{
    MAX_PAGE, Ordinal, findings::FindingKind, observation::Transition,
};
use crate::sync::export::{LiveObject, Occasion, SkippedExports};
use crate::sync::mirror::LiveScope;
use crate::{
    diagnostics::{self, LogLevel},
    storage::{
        Catalog,
        admin::{AdminError, BucketName, ObjectName},
        change_journal::JournalFence,
        sync_state::{root::RootLease, writeback::Writeback},
    },
    sync::{
        Reconciliation, SyncError,
        error::{superseded, unless_superseded},
        interrupt::{Interrupt, Interrupted},
    },
};
use std::collections::BTreeSet;
/// Where the exports of findings observed again run, and the counts they add to.
pub(in crate::sync) struct Observing<'a, 'b> {
    pub(in crate::sync) catalog: &'a Catalog,
    pub(in crate::sync) root: &'a RootLease,
    pub(in crate::sync) writeback: &'a Writeback,
    pub(in crate::sync) fence: JournalFence,
    pub(in crate::sync) summary: &'b mut Reconciliation,
}

/// What a replay changed in one bucket that can end findings of other objects: the alias
/// spellings of the objects it changed, read before the change, and the names it changed.
#[derive(Default)]
pub(in crate::sync) struct Changed {
    pub(in crate::sync) spellings: BTreeSet<String>,
    pub(in crate::sync) names: Vec<ObjectName>,
}

impl SkippedExports {
    /// Observes again every finding that [`observed_again`] admits and exports each object
    /// whose cause went, so that removing what stood in its way needs no restart. A finding
    /// that cannot be observed now, such as behind a directory the pass may not open or at a
    /// path the export fails to write, keeps its row for a later pass and stops nothing else.
    /// Returns the counts of the exports this ran; the findings it records again are counted by
    /// the pass that first met them.
    ///
    /// [`observed_again`]: crate::storage::sync_state::findings::FindingKind::observed_again
    pub(in crate::sync) async fn observe_findings(
        &mut self,
        scope: LiveScope<'_>,
        interrupt: Interrupt<'_>,
    ) -> Result<Reconciliation, SyncError> {
        let mut observed = Reconciliation::default();
        let mut observing = Observing {
            catalog: scope.catalog,
            root: scope.root,
            writeback: scope.writeback,
            fence: scope.catalog.current_fence().await?,
            summary: &mut observed,
        };
        let mut after = 0;
        loop {
            let page = scope.root.findings_page(after, MAX_PAGE).await?;
            let Some(last) = page.last() else {
                break;
            };
            after = last.ordinal;
            let full = page.len() == MAX_PAGE;
            for finding in page {
                if !finding.reason.observed_again() {
                    continue;
                }
                if interrupt.check() != Interrupted::No {
                    observing.summary.interrupted = true;
                    return Ok(observed);
                }
                // A finding names a bucket and an object the API accepted.
                let (Ok(bucket), Ok(name)) = (
                    BucketName::new(finding.bucket),
                    ObjectName::new(finding.name),
                ) else {
                    continue;
                };
                if finding.reason == FindingKind::Alias {
                    self.observe_alias(&mut observing, &bucket, name).await?;
                } else {
                    self.observe(&mut observing, bucket, name).await?;
                }
            }
            if !full {
                break;
            }
        }
        Ok(observed)
    }

    /// Observes again the findings of `bucket` that what a replay just `changed` there can have
    /// ended, which would otherwise wait for the next ingestion: the alias findings of the
    /// spellings it changed, and the collisions of the names above and below those it changed.
    pub(in crate::sync) async fn observe_changed(
        &mut self,
        observing: &mut Observing<'_, '_>,
        bucket: &BucketName,
        changed: &Changed,
        interrupt: Interrupt<'_>,
    ) -> Result<(), SyncError> {
        for spelling in &changed.spellings {
            let mut after = Ordinal::default();
            loop {
                let page = observing
                    .root
                    .alias_findings_page(bucket.clone(), spelling.clone(), after)
                    .await?;
                let full = page.len() == MAX_PAGE;
                for finding in page {
                    if interrupt.check() != Interrupted::No {
                        observing.summary.interrupted = true;
                        return Ok(());
                    }
                    after = finding.ordinal;
                    self.observe_alias(observing, bucket, finding.name).await?;
                }
                if !full {
                    break;
                }
            }
        }
        if changed.names.is_empty() {
            return Ok(());
        }
        let mut after = Ordinal::default();
        loop {
            let page = observing
                .root
                .collision_findings_page(bucket.clone(), after)
                .await?;
            let full = page.len() == MAX_PAGE;
            for finding in page {
                if interrupt.check() != Interrupted::No {
                    observing.summary.interrupted = true;
                    return Ok(());
                }
                after = finding.ordinal;
                if changed
                    .names
                    .iter()
                    .any(|name| nested(name, &finding.name) || nested(&finding.name, name))
                {
                    self.observe(observing, bucket.clone(), finding.name)
                        .await?;
                }
            }
            if !full {
                return Ok(());
            }
        }
    }

    /// Observes again the `heirs` that ingestion just took an edit in for, each the one object
    /// left of an alias, with the deleted object whose file it was. Returns the counts of the
    /// exports this ran.
    pub(in crate::sync) async fn observe_heirs(
        &mut self,
        scope: LiveScope<'_>,
        heirs: &[(BucketName, ObjectName)],
        interrupt: Interrupt<'_>,
    ) -> Result<Reconciliation, SyncError> {
        let mut observed = Reconciliation::default();
        let mut observing = Observing {
            catalog: scope.catalog,
            root: scope.root,
            writeback: scope.writeback,
            fence: scope.catalog.current_fence().await?,
            summary: &mut observed,
        };
        for (bucket, heir) in heirs {
            if interrupt.check() != Interrupted::No {
                observing.summary.interrupted = true;
                break;
            }
            self.observe_alias(&mut observing, bucket, heir.clone())
                .await?;
        }
        Ok(observed)
    }

    /// Observes the object of an alias finding again, after the deleted object whose record
    /// still holds the file the finding names: that file goes, or ingestion takes it in, before
    /// the object left can get its own.
    async fn observe_alias(
        &mut self,
        observing: &mut Observing<'_, '_>,
        bucket: &BucketName,
        name: ObjectName,
    ) -> Result<(), SyncError> {
        let binding = match observing.root.sync_binding(bucket).await {
            Ok(binding) => binding,
            // The bucket changed meanwhile; the replay of that change observes its objects.
            Err(AdminError::NotFound | AdminError::Precondition) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        match observing
            .root
            .deleted_alias_holder(binding, name.clone())
            .await
        {
            Ok(Some(holder)) => self.observe(observing, bucket.clone(), holder).await?,
            Ok(None) => {}
            Err(error) if superseded(&error) => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        self.observe(observing, bucket.clone(), name).await
    }

    /// Exports one object observed again, if its cause went. A failure to write its own path
    /// that leaves no write-back operation behind keeps its finding for a later pass, rather
    /// than failing the pass for every other object, and stops the path until an observation
    /// no longer fails; it is reported when the stop begins.
    async fn observe(
        &mut self,
        observing: &mut Observing<'_, '_>,
        bucket: BucketName,
        name: ObjectName,
    ) -> Result<(), SyncError> {
        let mut object = LiveObject {
            catalog: observing.catalog,
            root: observing.root,
            bucket: &bucket,
            name: &name,
            summary: &mut *observing.summary,
            occasion: Occasion::Reevaluate,
        };
        let outcome = match object.plan().await {
            Ok(Some(plan)) => {
                object
                    .export(observing.writeback, observing.fence, plan)
                    .await
            }
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        };
        // A failure that left a write-back operation for recovery to finish holds every export
        // back, so it still fails the pass.
        let settled = match &outcome {
            Err(error) if error.concerns_one_object() => observing.writeback.status().await?.idle(),
            Ok(_) | Err(_) => false,
        };
        match outcome {
            Ok(outcome) => {
                self.record((bucket.clone(), name.clone()), outcome);
                export_failure(observing.root, &bucket, name, Failure::Gone).await
            }
            Err(error) if settled => {
                let failure = Failure::Kept(&error);
                export_failure(observing.root, &bucket, name, failure).await
            }
            Err(error) => Err(error),
        }
    }
}

/// What the observation of a finding found of a failure to write the object's own path.
enum Failure<'a> {
    /// The observation did not fail there.
    Gone,
    /// The observation failed there with this error and left no write-back operation behind.
    Kept(&'a SyncError),
}

/// Records what the observation of the finding of `name` found of a failure to write its
/// path: the stop it holds, reported when it begins, or none. A bucket or object that changed
/// meanwhile is left to the replay of that change.
async fn export_failure(
    root: &RootLease,
    bucket: &BucketName,
    name: ObjectName,
    failure: Failure<'_>,
) -> Result<(), SyncError> {
    let binding = match root.sync_binding(bucket).await {
        Ok(binding) => binding,
        Err(AdminError::NotFound | AdminError::Precondition) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let error = match failure {
        Failure::Gone => {
            unless_superseded(root.clear_export_failure(binding, name).await)?;
            return Ok(());
        }
        Failure::Kept(error) => error,
    };
    match root.record_export_failure(binding, name.clone()).await {
        Ok(Transition::New) => diagnostics::emit(
            LogLevel::Warn,
            "sync_finding_export_failed",
            serde_json::json!({
                "bucket":bucket.as_str(),"object":name.as_str(),
                "reason":crate::error::Report(error).to_string(),
                "remedy":"The finding stays, and the object's path stops the wait as IoFailure until an ingestion observes it again without this failure; correct what the reason names at the object's path"
            }),
        ),
        Ok(Transition::Kept) => {}
        Err(error) if superseded(&error) => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Whether `inner` names an object below `outer`, whose file `outer`'s would have to be a
/// directory for.
fn nested(outer: &ObjectName, inner: &ObjectName) -> bool {
    inner
        .as_str()
        .strip_prefix(outer.as_str())
        .is_some_and(|rest| rest.starts_with('/'))
}
