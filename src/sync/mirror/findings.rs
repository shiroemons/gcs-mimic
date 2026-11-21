//! Findings whose cause can go without a journal change of their own object, such as another
//! API object or an entry in the tree that stands in its way. Each ingestion observes those
//! objects again and exports the ones whose cause went; a finding that still holds only reads.
//! An API change can remove the cause too: deleting an object ends an alias it belonged to and
//! a collision with the objects above or below its name, so a replay observes those again.
use super::{
    LiveObject, LiveScope, MissingFile, Occasion, Reconciliation, SkippedExports, SyncError,
};
use crate::{
    diagnostics::{self, LogLevel},
    object::ObjectRecord,
    storage::{
        Catalog,
        admin::{AdminError, BucketName, Generation, ObjectName},
        change_journal::JournalFence,
        sync_state::{
            MAX_PAGE, Ordinal, SyncBinding,
            findings::{
                FindingKind,
                alias::{AliasHold, Marked},
            },
            observation::Transition,
            root::RootLease,
            writeback::Writeback,
        },
    },
    sync::{
        error::{superseded, unless_superseded},
        interrupt::{Interrupt, Interrupted},
        mapping::Reason,
        report::remedy::Correction,
    },
};
use std::collections::BTreeSet;

/// What an object's alias does to its export.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Freeze {
    /// No alias names the object or its file.
    NotAliased,
    /// Another live object stands for the same file, so it stays as it is.
    Held,
    /// The alias ended, and the export that settles the file removes its finding.
    Released,
}

/// Where the exports of findings observed again run, and the counts they add to.
pub(super) struct Observing<'a, 'b> {
    pub(super) catalog: &'a Catalog,
    pub(super) root: &'a RootLease,
    pub(super) writeback: &'a Writeback,
    pub(super) fence: JournalFence,
    pub(super) summary: &'b mut Reconciliation,
}

/// What a replay changed in one bucket that can end findings of other objects: the alias
/// spellings of the objects it changed, read before the change, and the names it changed.
#[derive(Default)]
pub(super) struct Changed {
    pub(super) spellings: BTreeSet<String>,
    pub(super) names: Vec<ObjectName>,
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
    pub(super) async fn observe_findings(
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
                if finding.reason == FindingKind::Unmapped(Reason::Alias) {
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
    pub(super) async fn observe_changed(
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
    pub(super) async fn observe_heirs(
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
            missing: MissingFile::Inventory,
            occasion: Occasion::Finding,
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

impl LiveObject<'_> {
    /// Records that the tree holds this object's file under `spelling`, an alias of its path,
    /// for this object and the one tracked at `spelling`, and reports each that became frozen
    /// now. A generation that changed or went since this export read it is the next replay's.
    pub(super) async fn alias(
        &mut self,
        binding: &SyncBinding,
        record: &ObjectRecord,
        spelling: String,
    ) -> Result<(), SyncError> {
        let generation = Generation::new(record.generation)?;
        let recorded = match self
            .root
            .record_alias(
                binding.clone(),
                self.name.clone(),
                generation,
                spelling.clone(),
            )
            .await
        {
            Ok(recorded) => recorded,
            Err(error) if superseded(&error) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        self.summary.count_unmapped_object();
        let partner = recorded.partner.as_ref().map(|(name, _)| name.as_str());
        if recorded.own == Marked::New {
            frozen(self.bucket, self.name.as_str(), &spelling, partner);
        }
        if let Some((name, Marked::New)) = &recorded.partner {
            frozen(
                self.bucket,
                name.as_str(),
                &spelling,
                Some(self.name.as_str()),
            );
        }
        Ok(())
    }

    /// Whether an alias keeps this object out of the tree, decided before its file is read, and
    /// reports an alias that ended now.
    pub(super) async fn hold_alias(
        &mut self,
        binding: &SyncBinding,
        record: &ObjectRecord,
    ) -> Result<Freeze, SyncError> {
        let generation = Generation::new(record.generation)?;
        let hold = match self
            .root
            .hold_alias(binding.clone(), self.name.clone(), generation)
            .await
        {
            Ok(hold) => hold,
            // The binding or the object changed meanwhile; the replay of that change observes
            // the object.
            Err(error) if superseded(&error) => return Ok(Freeze::Held),
            Err(error) => return Err(error.into()),
        };
        Ok(match hold {
            AliasHold::NotAliased => Freeze::NotAliased,
            AliasHold::Held => {
                self.summary.count_unmapped_object();
                Freeze::Held
            }
            AliasHold::Released {
                spelling,
                transition,
            } => {
                if transition == Transition::New {
                    diagnostics::emit(
                        LogLevel::Info,
                        "sync_alias_released",
                        serde_json::json!({
                            "bucket":self.bucket.as_str(),"object":self.name.as_str(),"path":spelling,
                            "next":RELEASED
                        }),
                    );
                }
                Freeze::Released
            }
        })
    }
}

/// What follows the end of an alias, which `sync_alias_released` reports.
const RELEASED: &str = "Only this object is left of the alias. It gets its own file under its own name, after an edit kept out while the file was frozen is uploaded for it, and its alias finding goes once that is done";

/// Reports once that the file at `path` stands for `object` and, where the tree tracks it,
/// `partner`, and stays frozen.
fn frozen(bucket: &BucketName, object: &str, path: &str, partner: Option<&str>) {
    diagnostics::emit(
        LogLevel::Warn,
        "sync_alias_frozen",
        serde_json::json!({
            "bucket":bucket.as_str(),"object":object,"path":path,"partner":partner,
            "remedy":Correction::AliasedObjects.remedy()
        }),
    );
}
