//! Decides from what the catalog holds what one start does for each entry. The catalog runs the
//! plan inside the transaction that applies it, so a rejected plan changes nothing.

use super::{Collected, InitEntry, InitOutcome, Reason, Written, written::Member};
use crate::{
    auth::{Policy, PublicAccess},
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    storage::admin::{
        AdminError, BucketDefinition,
        initialization::{Creation, InitTarget, Observed, Planned, Stored},
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
    pub(super) warnings: Diagnostics,
}

impl Report {
    pub(super) fn outcome(&self) -> InitOutcome {
        InitOutcome::Applied {
            created: self.created,
            existing: self.existing,
            project_mismatches: self.project_mismatches,
            unapplied: self.unapplied,
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
                "This entry changed after its bucket was created from it, and existing settings are kept: the bucket still differs in {fields}. Change the bucket through the API to match, or restore the entry.{location}"
            ),
        );
    }
}

/// Plans each entry in file order: a missing bucket is created, and an existing one is kept.
pub(super) fn plan(
    entries: Vec<Prepared>,
    observed: Vec<Observed>,
    bucket_limit: Option<NonZeroUsize>,
) -> Result<(Vec<Planned>, Report), PlanError> {
    let mut report = Report::default();
    let mut rejected = Collected::new(bucket_limit);
    let mut planned = Vec::with_capacity(entries.len());
    for (index, (entry, observed)) in entries.into_iter().zip(observed).enumerate() {
        let step = match observed.existing {
            Some(stored) => {
                report.existing += 1;
                existing(&mut report, index, &entry, &stored)
            }
            None => {
                if public_access_refused(&entry.creation, observed.parent) {
                    rejected.record(
                        Reason::Native,
                        format!("buckets[{index}].iamPolicy"),
                        PUBLIC_ACCESS_FIX,
                    );
                }
                report.created += 1;
                Planned::Create(Box::new(entry.creation))
            }
        };
        planned.push(step);
    }
    if rejected.is_err() {
        return Err(PlanError::Rejected(rejected.finish()));
    }
    Ok((planned, report))
}

fn public_access_refused(creation: &Creation, parent: PublicAccess) -> bool {
    let prevented = creation
        .definition
        .record()
        .public_access_prevention_enforced
        || parent == PublicAccess::Prevented;
    prevented
        && creation
            .policy
            .as_ref()
            .is_some_and(Policy::names_public_principal)
}

/// Keeps an existing bucket. Its recorded entry moves only to an entry that the bucket matches,
/// so a changed entry keeps warning until the bucket or the entry is brought back in line.
fn existing(report: &mut Report, index: usize, entry: &Prepared, stored: &Stored) -> Planned {
    let wanted = entry.creation.definition.record();
    if stored.record.project != wanted.project {
        // The entry describes another owner's bucket, so it is not this bucket's entry.
        report.project_mismatches += 1;
        report.warn(format!("buckets[{index}].project"), PROJECT_FIX);
        return Planned::Keep;
    }
    let current = entry.creation.digest;
    match stored.digest {
        // The API or an older release created the bucket, so no earlier entry exists to compare
        // with: the current one becomes the baseline without a warning.
        None => Planned::Record(current),
        Some(recorded) if recorded == current => Planned::Keep,
        Some(_) => {
            let differing: Vec<_> = entry
                .written
                .differences(wanted, entry.creation.policy.as_ref(), stored)
                .collect();
            if differing.is_empty() {
                Planned::Record(current)
            } else {
                report.unapplied(index, &differing);
                Planned::Keep
            }
        }
    }
}
