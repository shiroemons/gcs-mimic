//! Why a supplied name is not accepted as a setting, and the fix each reason gives.
//!
//! Every source (the command line, the environment and the JSON file) reaches the same check,
//! so a reason added here applies to all three at once.

use super::Setting;
use crate::{
    config_input::Input, config_input::RenamedEnvironmentVariable, diagnostics::DiagnosticSource,
};
use std::fmt;

/// A supplied name that is not a setting from its source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Rejection {
    /// A client variable that moved to the `GCSM_` prefix.
    Renamed(RenamedEnvironmentVariable),
    /// `sync_reinitialize` names a one-time startup action, so only the command line takes it.
    StartupOnly,
    Unknown,
}

impl Input {
    /// The setting this input names, or why its source cannot name one.
    pub(super) fn setting(&self) -> Result<Setting, Rejection> {
        match (self.name.parse::<Setting>(), self.source) {
            // A configuration file cannot select another configuration file.
            (Ok(Setting::Config), DiagnosticSource::Config) => Err(Rejection::Unknown),
            (Ok(Setting::SyncReinitialize), source) if source != DiagnosticSource::Cli => {
                Err(Rejection::StartupOnly)
            }
            (Ok(setting), _) => Ok(setting),
            (Err(_), DiagnosticSource::Environment) => {
                Err(RenamedEnvironmentVariable::from_setting_name(&self.name)
                    .map_or(Rejection::Unknown, Rejection::Renamed))
            }
            (Err(_), _) => Err(Rejection::Unknown),
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Renamed(variable) => f.write_str(&variable.fix()),
            Self::StartupOnly => f.write_str(
                "sync_reinitialize is startup-only CLI input; remove it here and use --sync-dir DIRECTORY --sync-reinitialize CATALOG_ID once.",
            ),
            Self::Unknown => f.write_str(
                "Unknown setting; remove it and use --help for supported native settings. Reserved future settings are unsupported.",
            ),
        }
    }
}
