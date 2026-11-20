//! Findings whose cause can go without a journal change of their own object, such as another
//! API object or an entry in the tree that stands in its way. Each ingestion observes those
//! objects again and exports the ones whose cause went; a finding that still holds only reads.
//! An alias also ends with a change of the other object it names, so a replay observes the
//! alias findings of the buckets it changed again too.
use super::{
    LiveObject, LiveScope, MissingFile, Occasion, Reconciliation, SkippedExports, SyncError,
};
use crate::{
    diagnostics::{self, LogLevel},
    object::ObjectRecord,
    storage::{
        Catalog,
        admin::{BucketName, Generation, ObjectName},
        change_journal::JournalFence,
        sync_state::{
            MAX_PAGE, Ordinal, findings::alias::AliasHold, observation::Transition,
            root::RootLease, writeback::Writeback,
        },
    },
    sync::{
        control::Correction,
        error::superseded,
        interrupt::{Interrupt, Interrupted},
    },
};
use std::path::PathBuf;

/// What an object's alias does to its export.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Freeze {
    /// The object has no alias finding.
    None,
    /// Another object of the alias still lives, so the file stays as it is.
    Held,
    /// The alias ended with this observation.
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

impl SkippedExports {
    /// Observes again every finding that [`observed_again`] admits and exports each object
    /// whose cause went, so that removing what stood in its way needs no restart. A finding
    /// that cannot be observed now, such as behind a directory the pass may not open, keeps its
    /// row for a later pass and stops nothing else. Returns the counts of the exports this ran;
    /// the findings it records again are counted by the pass that first met them.
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
                self.observe(&mut observing, bucket, name).await?;
            }
            if !full {
                break;
            }
        }
        Ok(observed)
    }

    /// Observes again the alias findings of `bucket`, some of whose objects a replay just
    /// changed: the object that changed or went may have been the other half of an alias,
    /// which would otherwise stay frozen until the next ingestion.
    pub(super) async fn observe_aliases(
        &mut self,
        observing: &mut Observing<'_, '_>,
        bucket: &BucketName,
        interrupt: Interrupt<'_>,
    ) -> Result<(), SyncError> {
        let mut after = Ordinal::default();
        loop {
            let page = observing
                .root
                .alias_findings_page(bucket.clone(), after)
                .await?;
            let Some(last) = page.last() else {
                return Ok(());
            };
            after = last.ordinal;
            let full = page.len() == MAX_PAGE;
            for finding in page {
                if interrupt.check() != Interrupted::No {
                    observing.summary.interrupted = true;
                    return Ok(());
                }
                self.observe(observing, bucket.clone(), finding.name)
                    .await?;
            }
            if !full {
                return Ok(());
            }
        }
    }

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
        let outcome = match object.plan().await? {
            Some(plan) => {
                object
                    .export(observing.writeback, observing.fence, plan)
                    .await?
            }
            None => None,
        };
        self.record((bucket, name), outcome);
        Ok(())
    }
}

impl LiveObject<'_> {
    /// Records that the tree holds this object's file under `spelling`, an alias of its path,
    /// for this object and the one tracked at `spelling`, and reports each that became frozen
    /// now. A generation that changed or went since this export read it is the next replay's.
    pub(super) async fn alias(
        &mut self,
        record: &ObjectRecord,
        spelling: PathBuf,
    ) -> Result<(), SyncError> {
        let spelling = spelling
            .into_os_string()
            .into_string()
            .map_err(|_| SyncError::Invariant("invalid alias path"))?;
        let binding = self.catalog.sync_state().binding(self.bucket).await?;
        let generation = Generation::new(record.generation)?;
        let recorded = match self
            .root
            .record_alias(binding, self.name.clone(), generation, spelling.clone())
            .await
        {
            Ok(recorded) => recorded,
            Err(error) if superseded(&error) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        self.summary.count_unmapped_object();
        let partner = recorded.partner.as_ref().map(|(name, _)| name.as_str());
        if recorded.own == Some(Transition::New) {
            frozen(self.bucket, self.name.as_str(), &spelling, partner);
        }
        if let Some((name, Some(Transition::New))) = &recorded.partner {
            frozen(
                self.bucket,
                name.as_str(),
                &spelling,
                Some(self.name.as_str()),
            );
        }
        Ok(())
    }

    /// Whether an alias keeps this object, whose path the tree does not hold under another
    /// spelling, out of the tree, and reports an alias that ended now.
    pub(super) async fn hold_alias(&mut self, record: &ObjectRecord) -> Result<Freeze, SyncError> {
        let binding = self.catalog.sync_state().binding(self.bucket).await?;
        let generation = Generation::new(record.generation)?;
        let hold = match self
            .root
            .hold_alias(binding, self.name.clone(), generation)
            .await
        {
            Ok(hold) => hold,
            // The binding changed meanwhile; the replay of that change observes the object.
            Err(error) if superseded(&error) => return Ok(Freeze::Held),
            Err(error) => return Err(error.into()),
        };
        Ok(match hold {
            AliasHold::NotAliased => Freeze::None,
            AliasHold::Held => {
                self.summary.count_unmapped_object();
                Freeze::Held
            }
            AliasHold::Released { spelling } => {
                diagnostics::emit(
                    LogLevel::Info,
                    "sync_alias_released",
                    serde_json::json!({
                        "bucket":self.bucket.as_str(),"object":self.name.as_str(),"path":spelling
                    }),
                );
                Freeze::Released
            }
        })
    }
}

/// Reports once that the file at `path` stands for `object` and `partner` and stays frozen.
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
