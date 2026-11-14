//! The closed sets of the feature ledger and the typed view of one row.
//!
//! `compatibility/features.schema.json` is the persistent contract for these names;
//! `schema_enumerations_match_validator` keeps the enums and the schema in step.

use crate::{Result, UnknownName, ensure, named, string};
use serde_json::Value;
use std::str::FromStr;

named! {
    /// What a row tracks.
    pub(crate) enum Kind {
        ApiOperation => "api_operation",
        Behavior => "behavior",
        ProjectRequirement => "project_requirement",
    }
}

named! {
    /// How far a row's declared acceptance cases have been completed.
    pub(crate) enum Status {
        NotStarted => "NOT_STARTED",
        InProgress => "IN_PROGRESS",
        Partial => "PARTIAL",
        Implemented => "IMPLEMENTED",
        Blocked => "BLOCKED",
        NotApplicable => "NOT_APPLICABLE",
    }
}

named! {
    /// How a feature is reproduced.
    pub(crate) enum Fidelity {
        Exact => "EXACT",
        LocalSimulation => "LOCAL_SIMULATION",
        MetadataOnly => "METADATA_ONLY",
        External => "EXTERNAL",
        Project => "PROJECT",
    }
}

/// `actual_fidelity` may also be `NONE`: nothing has been achieved yet.
pub(crate) const NO_FIDELITY: &str = "NONE";

/// Parses `actual_fidelity`, where `NONE` is no fidelity rather than an unknown one.
pub(crate) fn achieved(text: &str) -> std::result::Result<Option<Fidelity>, UnknownName> {
    match text {
        NO_FIDELITY => Ok(None),
        _ => text.parse().map(Some),
    }
}

/// The text `actual_fidelity` holds for `achieved`.
pub(crate) fn achieved_text(achieved: Option<Fidelity>) -> &'static str {
    achieved.map_or(NO_FIDELITY, Fidelity::as_str)
}

impl Status {
    /// Work that must already be decomposed into acceptance cases.
    pub(crate) const fn is_active(self) -> bool {
        matches!(self, Self::InProgress | Self::Partial | Self::Implemented)
    }
}

/// The closed-set columns of a row, read in the order validation reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Classification {
    pub(crate) kind: Kind,
    pub(crate) status: Status,
    pub(crate) target: Fidelity,
    pub(crate) achieved: Option<Fidelity>,
}

impl Classification {
    pub(crate) fn parse(row: &Value) -> Result<Self> {
        Ok(Self {
            kind: parse(row, "kind", "invalid kind")?,
            status: parse(row, "status", "invalid status")?,
            target: parse(row, "target_fidelity", "invalid target fidelity")?,
            achieved: achieved(string(row, "actual_fidelity")?)
                .map_err(|UnknownName| "invalid actual fidelity".to_owned())?,
        })
    }

    /// Rejects fidelity claims that contradict the kind or the status: only project
    /// requirements target or achieve `PROJECT`, a simulation is never `EXACT`, and work
    /// that has not started has achieved nothing.
    pub(crate) fn check(self) -> Result<()> {
        let Self {
            kind,
            status,
            target,
            achieved,
        } = self;
        ensure(
            kind == Kind::ProjectRequirement || target != Fidelity::Project,
            "kind/target fidelity mismatch",
        )?;
        ensure(
            (achieved != Some(Fidelity::Project) || target == Fidelity::Project)
                && (target != Fidelity::Project
                    || matches!(achieved, None | Some(Fidelity::Project))),
            "kind/actual fidelity mismatch",
        )?;
        ensure(
            target != Fidelity::LocalSimulation || achieved != Some(Fidelity::Exact),
            "simulation cannot claim EXACT",
        )?;
        if status == Status::NotStarted {
            ensure(achieved.is_none(), "NOT_STARTED has achieved fidelity")?;
        }
        Ok(())
    }
}

/// The identity and classification of a ledger row, parsed once. The evidence fields
/// stay on `value` and are read where they are checked.
pub(crate) struct FeatureRow<'a> {
    pub(crate) id: &'a str,
    pub(crate) kind: Kind,
    pub(crate) status: Status,
    pub(crate) target: Fidelity,
    pub(crate) achieved: Option<Fidelity>,
    pub(crate) phase: &'a str,
    pub(crate) category: &'a str,
    pub(crate) value: &'a Value,
}

impl<'a> FeatureRow<'a> {
    /// Reads a row without validating the rest of it, for views of a ledger. A row that
    /// cannot be read is an error, not a panic, since such a view may skip `validate`.
    pub(crate) fn parse(value: &'a Value) -> Result<Self> {
        let id = string(value, "id")?;
        let Classification {
            kind,
            status,
            target,
            achieved,
        } = Classification::parse(value)?;
        Ok(Self {
            id,
            kind,
            status,
            target,
            achieved,
            phase: string(value, "phase")?,
            category: string(value, "category")?,
            value,
        })
    }
}

fn parse<T: FromStr<Err = UnknownName>>(row: &Value, key: &str, invalid: &str) -> Result<T> {
    string(row, key)?
        .parse()
        .map_err(|UnknownName| invalid.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn achieved_fidelity_reads_none_as_nothing_achieved() {
        assert_eq!(achieved("NONE"), Ok(None));
        assert_eq!(achieved("EXACT"), Ok(Some(Fidelity::Exact)));
        assert_eq!(achieved("none"), Err(UnknownName));
        for fidelity in [None]
            .into_iter()
            .chain(Fidelity::ALL.iter().copied().map(Some))
        {
            assert_eq!(achieved(achieved_text(fidelity)), Ok(fidelity));
        }
    }
}
