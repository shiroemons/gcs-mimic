use super::PathStop;
use crate::{storage::catalog::sql, sync::mapping::Reason};

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

impl PauseReason {
    pub(crate) fn state(self) -> &'static str {
        match self {
            Self::ConflictCapacity | Self::IoFailure | Self::RecoveryRequired => self.as_str(),
            Self::ObserverFailure | Self::SourceChanging | Self::SourceUnrepresentable => "Paused",
        }
    }
}

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

#[derive(Debug)]
pub(crate) enum FindingReason {
    Mapping(Reason),
    RootStop(PauseReason),
    PathStop(PathStop),
    Writeback(String),
    Conflict(String),
}

impl FindingReason {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Mapping(reason) => reason.as_str(),
            Self::RootStop(reason) => reason.as_str(),
            Self::PathStop(reason) => reason.as_str(),
            Self::Writeback(phase) | Self::Conflict(phase) => phase,
        }
    }
}

crate::serde_util::serialize_as_str!(FindingReason);
