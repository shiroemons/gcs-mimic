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
    /// A setting that a release withdrew, as named from this source.
    Withdrawn(WithdrawnSetting, DiagnosticSource),
    /// `sync_reinitialize` names a one-time startup action, so only the command line takes it.
    StartupOnly,
    Unknown,
}

/// A setting name that a release withdrew. It still stops startup, naming what replaced it, so
/// a configuration written for an older release fails visibly instead of losing its effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WithdrawnSetting {
    /// It also governs the inline `buckets` document, which its name hid.
    BucketsFileMode,
}

/// What took a withdrawn setting's place.
enum Replacement {
    Renamed(Setting),
}

impl WithdrawnSetting {
    const ALL: [Self; 1] = [Self::BucketsFileMode];

    const fn name(self) -> &'static str {
        match self {
            Self::BucketsFileMode => "buckets_file_mode",
        }
    }

    const fn replacement(self) -> Replacement {
        match self {
            Self::BucketsFileMode => Replacement::Renamed(Setting::BucketsMode),
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|setting| setting.name() == name)
    }
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
            (Err(_), source) => Err(match WithdrawnSetting::from_name(&self.name) {
                Some(withdrawn) => Rejection::Withdrawn(withdrawn, source),
                None if source == DiagnosticSource::Environment => {
                    RenamedEnvironmentVariable::from_setting_name(&self.name)
                        .map_or(Rejection::Unknown, Rejection::Renamed)
                }
                None => Rejection::Unknown,
            }),
        }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Renamed(variable) => f.write_str(&variable.fix()),
            Self::Withdrawn(withdrawn, source) => match withdrawn.replacement() {
                Replacement::Renamed(setting) => write!(
                    f,
                    "{} was renamed to {}; use {}.",
                    withdrawn.name(),
                    setting.as_str(),
                    setting.spelled(*source)
                ),
            },
            Self::StartupOnly => f.write_str(
                "sync_reinitialize is startup-only CLI input; remove it here and use --sync-dir DIRECTORY --sync-reinitialize CATALOG_ID once.",
            ),
            Self::Unknown => f.write_str(
                "Unknown setting; remove it and use --help for supported native settings. Reserved future settings are unsupported.",
            ),
        }
    }
}
