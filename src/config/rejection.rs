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
    Unknown,
}

/// A setting name that a release withdrew. It still stops startup, naming what replaced it, so
/// a configuration written for an older release fails visibly instead of losing its effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WithdrawnSetting {
    /// It also governs the inline `buckets` document, which its name hid.
    BucketsFileMode,
    /// The single timeout before 0.12.0 split it, kept as an alias that set all three. It
    /// never covered connection lifetime or `keep_alive_timeout_ms`, which its name suggested.
    ConnectionTimeoutMs,
    /// The directory synchronization settings. A configuration that still names one expected
    /// the server to write its objects back to a folder, which no setting does any more.
    SyncDir,
    SyncDeleteGuard,
    SyncReinitialize,
    MaxSyncEntries,
    MaxSyncDepth,
    /// The removed `gcs-mimic sync` command's token, which 0.18.0 had renamed to
    /// `GCSM_SYNC_TOKEN`; that name now has no reader to point to.
    SyncToken,
}

/// What took a withdrawn setting's place.
enum Replacement {
    Renamed(Setting),
    /// Several settings now each cover part of what it did.
    Split(&'static [Setting]),
    /// Nothing does: directory synchronization was removed, and `seed_dir` reads a folder
    /// without ever writing to it.
    Synchronization,
}

impl WithdrawnSetting {
    const ALL: [Self; 8] = [
        Self::BucketsFileMode,
        Self::ConnectionTimeoutMs,
        Self::SyncDir,
        Self::SyncDeleteGuard,
        Self::SyncReinitialize,
        Self::MaxSyncEntries,
        Self::MaxSyncDepth,
        Self::SyncToken,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::BucketsFileMode => "buckets_file_mode",
            Self::ConnectionTimeoutMs => "connection_timeout_ms",
            Self::SyncDir => "sync_dir",
            Self::SyncDeleteGuard => "sync_delete_guard",
            Self::SyncReinitialize => "sync_reinitialize",
            Self::MaxSyncEntries => "max_sync_entries",
            Self::MaxSyncDepth => "max_sync_depth",
            Self::SyncToken => "sync_token",
        }
    }

    const fn replacement(self) -> Replacement {
        match self {
            Self::BucketsFileMode => Replacement::Renamed(Setting::BucketsMode),
            Self::ConnectionTimeoutMs => Replacement::Split(&[
                Setting::HeaderReadTimeoutMs,
                Setting::BodyInactivityTimeoutMs,
                Setting::RequestTimeoutMs,
            ]),
            Self::SyncDir
            | Self::SyncDeleteGuard
            | Self::SyncReinitialize
            | Self::MaxSyncEntries
            | Self::MaxSyncDepth
            | Self::SyncToken => Replacement::Synchronization,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|setting| setting.name() == name)
    }

    /// Whether a release withdrew `name`, so that a config file member naming it stops with its
    /// advice whatever the type of its value, such as `"sync_delete_guard": false`.
    pub(super) fn is_withdrawn(name: &str) -> bool {
        Self::from_name(name).is_some()
    }
}

impl Input {
    /// The setting this input names, or why its source cannot name one.
    pub(super) fn setting(&self) -> Result<Setting, Rejection> {
        match (self.name.parse::<Setting>(), self.source) {
            // A configuration file cannot select another configuration file.
            (Ok(Setting::Config), DiagnosticSource::Config) => Err(Rejection::Unknown),
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
                Replacement::Split(settings) => {
                    write!(f, "{} was removed; set ", withdrawn.name())?;
                    for (index, setting) in settings.iter().enumerate() {
                        let separator = if index == 0 {
                            ""
                        } else if index + 1 == settings.len() {
                            " and "
                        } else {
                            ", "
                        };
                        write!(f, "{separator}{}", setting.spelled(*source))?;
                    }
                    f.write_str(" instead.")
                }
                Replacement::Synchronization => write!(
                    f,
                    "{} was removed with directory synchronization; remove it. {} reads a folder without writing to it.",
                    withdrawn.name(),
                    Setting::SeedDir.spelled(*source)
                ),
            },
            Self::Unknown => f.write_str(
                "Unknown setting; remove it and use --help for supported native settings. Reserved future settings are unsupported.",
            ),
        }
    }
}
