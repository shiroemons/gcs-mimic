//! Decides from what the catalog holds what one start does for each entry. The catalog runs the
//! plan inside the transaction that applies it, so a rejected plan changes nothing.

use super::{Collected, InitEntry, InitOutcome, Reason};
use crate::{
    auth::{Policy, PublicAccess},
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    storage::admin::{
        AdminError, BucketDefinition,
        initialization::{Creation, InitTarget, Observed, Planned},
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

/// Checks every definition before the catalog is touched and names the bucket each targets. The
/// targets go to the catalog job and the creations to the plan, so each needs its own names.
pub(super) fn prepare(
    entries: Vec<InitEntry>,
) -> Result<(Vec<InitTarget>, Vec<Creation>), AdminError> {
    entries
        .into_iter()
        .map(|entry| {
            let target = InitTarget {
                name: entry.record.name.clone(),
                project: entry.record.project.clone(),
            };
            let creation = Creation {
                definition: BucketDefinition::new(entry.record)?,
                policy: entry.policy,
            };
            Ok((target, creation))
        })
        .collect()
}

#[derive(Default)]
pub(super) struct Report {
    created: usize,
    existing: usize,
    project_mismatches: usize,
    pub(super) warnings: Diagnostics,
}

impl Report {
    pub(super) fn outcome(&self) -> InitOutcome {
        InitOutcome::Applied {
            created: self.created,
            existing: self.existing,
            project_mismatches: self.project_mismatches,
        }
    }

    fn warn(&mut self, key: String, fix: &str) {
        self.warnings
            .warnings
            .push(Diagnostic::new(DiagnosticSource::Initializer, key, fix));
    }
}

/// Plans each entry in file order. A missing bucket is created; an existing one is kept as it is.
pub(super) fn plan(
    entries: Vec<Creation>,
    observed: Vec<Observed>,
    bucket_limit: Option<NonZeroUsize>,
) -> Result<(Vec<Planned>, Report), PlanError> {
    let mut report = Report::default();
    let mut rejected = Collected::new(bucket_limit);
    let mut planned = Vec::with_capacity(entries.len());
    for (index, (entry, observed)) in entries.into_iter().zip(observed).enumerate() {
        let record = entry.definition.record();
        if let Some(existing) = observed.existing {
            report.existing += 1;
            if existing.project != record.project {
                report.project_mismatches += 1;
                report.warn(format!("buckets[{index}].project"), PROJECT_FIX);
            }
            planned.push(Planned::Keep);
            continue;
        }
        let prevented =
            record.public_access_prevention_enforced || observed.parent == PublicAccess::Prevented;
        if prevented
            && entry
                .policy
                .as_ref()
                .is_some_and(Policy::names_public_principal)
        {
            rejected.record(
                Reason::Native,
                format!("buckets[{index}].iamPolicy"),
                PUBLIC_ACCESS_FIX,
            );
        }
        report.created += 1;
        planned.push(Planned::Create(Box::new(entry)));
    }
    if rejected.is_err() {
        return Err(PlanError::Rejected(rejected.finish()));
    }
    Ok((planned, report))
}
