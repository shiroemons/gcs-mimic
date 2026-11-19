use super::PathStop;
use crate::storage::catalog::{
    sql,
    sync_state::{conflicts::ConflictState, findings::FindingKind, writeback::Phase},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PauseReason {
    ConflictCapacity,
    IoFailure,
    RecoveryRequired,
    ObserverFailure,
    SourceChanging,
    SourceUnrepresentable,
}
sql::text_enum!(pub(crate) PauseReason, "invalid synchronization pause reason", {
    ConflictCapacity => "ConflictCapacity", IoFailure => "IoFailure",
    RecoveryRequired => "RecoveryRequired", ObserverFailure => "observer-failure",
    SourceChanging => "SourceChanging", SourceUnrepresentable => "SourceUnrepresentable",
});

crate::serde_util::serialize_as_str!(PauseReason);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FindingCategory {
    Mapping,
    Writeback,
    RootStop,
    PathStop,
    Conflict,
}
sql::text_enum!(pub(crate) FindingCategory, "invalid synchronization finding category", {
    Mapping => "mapping", Writeback => "writeback", RootStop => "root-stop",
    PathStop => "path-stop", Conflict => "conflict",
});

crate::serde_util::serialize_as_str!(FindingCategory);

impl FindingCategory {
    /// The category's place in the findings page (the `kind` column of `PAGE`) and in a
    /// findings cursor.
    pub(crate) const fn rank(self) -> i64 {
        match self {
            Self::Mapping => 0,
            Self::Writeback => 1,
            Self::RootStop => 2,
            Self::PathStop => 3,
            Self::Conflict => 4,
        }
    }

    pub(crate) const fn from_rank(rank: i64) -> Option<Self> {
        match rank {
            0 => Some(Self::Mapping),
            1 => Some(Self::Writeback),
            2 => Some(Self::RootStop),
            3 => Some(Self::PathStop),
            4 => Some(Self::Conflict),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(crate) enum FindingReason {
    /// A `sync_findings` row, which also records an export that met an edited file.
    Mapping(FindingKind),
    RootStop(PauseReason),
    PathStop(PathStop),
    Writeback(Phase),
    Conflict(ConflictState),
}

impl FindingReason {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Mapping(kind) => kind.as_str(),
            Self::RootStop(reason) => reason.as_str(),
            Self::PathStop(reason) => reason.as_str(),
            Self::Writeback(phase) => phase.as_str(),
            Self::Conflict(state) => state.as_str(),
        }
    }
}

crate::serde_util::serialize_as_str!(FindingReason);
