//! Decides from what the catalog holds what one start does for each entry. The catalog runs the
//! plan inside the transaction that applies it, so a rejected plan changes nothing.

use super::{
    Collected, InitEntry, InitOutcome, Reason, Written,
    written::{self, Member},
};
use crate::{
    auth::{Policy, PublicAccess},
    bucket::BucketRecord,
    config::BucketsFileMode,
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    storage::admin::{
        AdminError, BucketDefinition,
        initialization::{Creation, InitTarget, Observed, Planned, Reconciliation, Stored},
    },
};
use std::num::NonZeroUsize;

const PUBLIC_ACCESS_FIX: &str = "Public access prevention is enforced for this bucket or its project, so its policy cannot grant allUsers or allAuthenticatedUsers; remove those members, or set \"iamConfiguration\":{\"publicAccessPrevention\":\"inherited\"} when the project does not enforce it.";
const PROJECT_FIX: &str = "Existing bucket ownership is retained; align or remove this entry, or choose another bucket name, for example local-assets.";

/// Why a run stopped before it changed anything.
#[derive(Debug)]
pub(super) enum PlanError {
    Admin(AdminError),
    /// The file asks for something the stored state refuses; the diagnostics name each entry.
    Rejected(Diagnostics),
}

impl From<AdminError> for PlanError {
    fn from(error: AdminError) -> Self {
        Self::Admin(error)
    }
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Admin(_) => "bucket initialization stopped by a catalog failure",
            Self::Rejected(_) => "bucket initialization refused by stored state",
        })
    }
}

impl std::error::Error for PlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Admin(error) => Some(error),
            Self::Rejected(diagnostics) => Some(diagnostics),
        }
    }
}

/// An entry whose bucket definition the catalog accepts.
pub(super) struct Prepared {
    creation: Creation,
    written: Written,
}

/// Checks every definition before the catalog is touched and names the bucket each targets. The
/// targets go to the catalog job and the entries to the plan, so each needs its own names.
pub(super) fn prepare(
    entries: Vec<InitEntry>,
) -> Result<(Vec<InitTarget>, Vec<Prepared>), AdminError> {
    entries
        .into_iter()
        .map(|entry| {
            let target = InitTarget {
                name: entry.record.name.clone(),
                project: entry.record.project.clone(),
            };
            let digest = entry.written.digest(&entry.record, entry.policy.as_ref());
            let creation = Creation {
                definition: BucketDefinition::new(entry.record)?,
                policy: entry.policy,
                digest,
            };
            let written = entry.written;
            Ok((target, Prepared { creation, written }))
        })
        .collect()
}

#[derive(Default)]
pub(super) struct Report {
    created: usize,
    existing: usize,
    project_mismatches: usize,
    unapplied: usize,
    reconciled: usize,
    pub(super) warnings: Diagnostics,
}

impl Report {
    pub(super) fn outcome(&self) -> InitOutcome {
        InitOutcome::Applied {
            created: self.created,
            existing: self.existing,
            project_mismatches: self.project_mismatches,
            unapplied: self.unapplied,
            reconciled: self.reconciled,
        }
    }

    fn warn(&mut self, key: String, fix: impl Into<String>) {
        self.warnings
            .warnings
            .push(Diagnostic::new(DiagnosticSource::Initializer, key, fix));
    }

    /// One warning names every member the bucket kept, but never a value.
    fn unapplied(&mut self, index: usize, members: &[Member]) {
        self.unapplied += 1;
        let fields = members
            .iter()
            .map(|member| member.field())
            .collect::<Vec<_>>()
            .join(", ");
        let location = if members.contains(&Member::Location) {
            " An existing bucket keeps its location; delete and recreate the bucket to move it."
        } else {
            ""
        };
        self.warn(
            format!("buckets[{index}]"),
            format!(
                "This entry changed after its bucket was created or last reconciled, and existing settings are kept: the bucket still differs in {fields}. Set buckets_file_mode=reconcile to apply the written fields, change the bucket through the API, or restore the entry.{location}"
            ),
        );
    }

    fn location_kept(&mut self, index: usize) {
        self.unapplied += 1;
        self.warn(
            format!("buckets[{index}].location"),
            "An existing bucket keeps its location, so reconcile mode left it; delete and recreate the bucket to move it, or write the bucket's current location.",
        );
    }
}

/// Plans each entry in file order. A missing bucket is created; an existing one is kept, or in
/// reconcile mode given what its entry writes.
pub(super) fn plan(
    entries: Vec<Prepared>,
    observed: Vec<Observed>,
    bucket_limit: Option<NonZeroUsize>,
    mode: BucketsFileMode,
) -> Result<(Vec<Planned>, Report), PlanError> {
    let mut run = Run {
        report: Report::default(),
        rejected: Collected::new(bucket_limit),
        mode,
    };
    let planned = entries
        .into_iter()
        .zip(observed)
        .enumerate()
        .map(|(index, (entry, observed))| run.step(index, entry, observed))
        .collect::<Result<Vec<_>, _>>()?;
    if run.rejected.is_err() {
        return Err(PlanError::Rejected(run.rejected.finish()));
    }
    Ok((planned, run.report))
}

/// Whether public members may be granted: neither the bucket nor its parent project prevents it.
fn public_access(bucket: &BucketRecord, parent: PublicAccess) -> PublicAccess {
    if bucket.public_access_prevention_enforced {
        PublicAccess::Prevented
    } else {
        parent
    }
}

struct Run {
    report: Report,
    /// Entries whose stored state refuses them; every entry is checked before any is applied.
    rejected: Collected,
    mode: BucketsFileMode,
}

impl Run {
    fn step(
        &mut self,
        index: usize,
        entry: Prepared,
        observed: Observed,
    ) -> Result<Planned, PlanError> {
        let Some(stored) = observed.existing else {
            let creation = entry.creation;
            let access = public_access(creation.definition.record(), observed.parent);
            self.refuse_public_policy(index, creation.policy.as_ref(), access);
            self.report.created += 1;
            return Ok(Planned::Create(Box::new(creation)));
        };
        self.report.existing += 1;
        if stored.record.project != entry.creation.definition.record().project {
            // The entry describes another owner's bucket, so it is not this bucket's entry.
            self.report.project_mismatches += 1;
            self.report
                .warn(format!("buckets[{index}].project"), PROJECT_FIX);
            return Ok(Planned::Keep);
        }
        match self.mode {
            BucketsFileMode::Create => Ok(self.keep(index, &entry, &stored)),
            BucketsFileMode::Reconcile => self.reconcile(index, entry, &stored, observed.parent),
        }
    }

    fn refuse_public_policy(
        &mut self,
        index: usize,
        policy: Option<&Policy>,
        access: PublicAccess,
    ) {
        if access == PublicAccess::Prevented && policy.is_some_and(Policy::names_public_principal) {
            self.rejected.record(
                Reason::Native,
                format!("buckets[{index}].iamPolicy"),
                PUBLIC_ACCESS_FIX,
            );
        }
    }

    /// Keeps an existing bucket. Its recorded entry moves only to an entry that the bucket
    /// matches, so a changed entry keeps warning until the bucket or the entry is brought back
    /// in line.
    fn keep(&mut self, index: usize, entry: &Prepared, stored: &Stored) -> Planned {
        let current = entry.creation.digest;
        match stored.digest {
            // The API or an older release created the bucket, so no earlier entry exists to
            // compare with: the current one becomes the baseline without a warning.
            None => Planned::Record(current),
            Some(recorded) if recorded == current => Planned::Keep,
            Some(_) => {
                let differing: Vec<_> = entry
                    .written
                    .differences(
                        entry.creation.definition.record(),
                        entry.creation.policy.as_ref(),
                        stored,
                    )
                    .collect();
                if differing.is_empty() {
                    Planned::Record(current)
                } else {
                    self.report.unapplied(index, &differing);
                    Planned::Keep
                }
            }
        }
    }

    /// Writes each member the entry writes whose value the bucket does not hold, whether or not
    /// the entry changed, so an API change to a written member is undone and one to any other
    /// member stays. An unchanged bucket gets no write at all.
    fn reconcile(
        &mut self,
        index: usize,
        entry: Prepared,
        stored: &Stored,
        parent: PublicAccess,
    ) -> Result<Planned, PlanError> {
        let Prepared { creation, written } = entry;
        let wanted = creation.definition.record();
        let mut differing: Vec<_> = written
            .differences(wanted, creation.policy.as_ref(), stored)
            .collect();
        if let Some(location) = differing
            .iter()
            .position(|member| *member == Member::Location)
        {
            differing.remove(location);
            self.report.location_kept(index);
        }
        let current = creation.digest;
        if differing.is_empty() {
            return Ok(if stored.digest == Some(current) {
                Planned::Keep
            } else {
                Planned::Record(current)
            });
        }
        let policy = creation
            .policy
            .filter(|_| differing.contains(&Member::IamPolicy));
        // Public access prevention as it stands once this entry is applied.
        let prevention = if written.contains(Member::PublicAccessPrevention) {
            wanted
        } else {
            &stored.record
        };
        self.refuse_public_policy(index, policy.as_ref(), public_access(prevention, parent));
        let update = written::patch(&differing, wanted)?;
        self.report.reconciled += 1;
        Ok(Planned::Reconcile(Box::new(Reconciliation {
            update,
            policy,
            digest: current,
        })))
    }
}
