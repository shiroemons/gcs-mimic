//! The naming rules of the filesystem that holds a synchronization root.
//!
//! The mirror probes them in the root's write-back workspace each time it activates the root,
//! after write-back recovery and before the first scan, and records them on the root lease. Every
//! name mapping, parent walk and target check reads that one value, so a name that one of them
//! accepts is never refused by another.

mod probe;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) use probe::DIRECTORY as PROBE_DIRECTORY;
pub(crate) use probe::run as probe;

use crate::{
    diagnostics::{self, LogLevel},
    storage::{
        failure::{Link, TypedSource},
        sync_state::{observation::PauseReason, writeback::Writeback},
    },
    sync::SyncError,
};
use serde::Serialize;
use std::{error::Error, ffi::OsStr, fmt, io};

/// The longest component the mirror maps, in bytes, even where a filesystem reports more. Linux
/// and macOS local filesystems stop at 255 (`NAME_MAX`), so a longer component on a network or
/// FUSE mount would make a tree that the host sharing it cannot hold.
const PORTABLE_NAME_MAX: usize = 255;

/// Whether the root's filesystem stores `:` in a file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Colon {
    Accepted,
    Rejected,
}

/// Whether two names that differ only in letter case name different files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Case {
    Sensitive,
    Insensitive,
}

/// Whether the filesystem lists a created name exactly as it was created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Spelling {
    Preserved,
    Rewritten,
}

/// The longest component, in bytes, that the root's filesystem reports through
/// `fpathconf(_PC_NAME_MAX)`; `None` when it reports no limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct NameMax(Option<usize>);

impl From<Option<usize>> for NameMax {
    fn from(limit: Option<usize>) -> Self {
        Self(limit)
    }
}

/// What one probe of a root observed. Only a probe, or a test that injects one, makes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Observation {
    pub(crate) colon: Colon,
    pub(crate) case: Case,
    pub(crate) spelling: Spelling,
    pub(crate) name_max: NameMax,
}

/// The naming rules of one root's filesystem. There is no default: the only way to make one is
/// from a probe's [`Observation`], so no caller maps names under rules nobody checked, and a
/// filesystem that rewrites names has no value at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct RootNaming {
    colon: Colon,
    case: Case,
    name_max: NameMax,
}

impl TryFrom<Observation> for RootNaming {
    type Error = ProbeError;

    fn try_from(observed: Observation) -> Result<Self, ProbeError> {
        let Observation {
            colon,
            case,
            spelling,
            name_max,
        } = observed;
        match spelling {
            Spelling::Preserved => Ok(Self {
                colon,
                case,
                name_max,
            }),
            Spelling::Rewritten => Err(ProbeError::RewritesNames),
        }
    }
}

impl RootNaming {
    /// The rules of a local POSIX filesystem that keeps every name byte, for tests below the
    /// mirror that do not probe a root.
    #[cfg(test)]
    pub(crate) fn posix() -> Self {
        Self {
            colon: Colon::Accepted,
            case: Case::Sensitive,
            name_max: NameMax(Some(PORTABLE_NAME_MAX)),
        }
    }

    /// The longest component the mirror maps, in bytes: the filesystem's limit, capped at
    /// [`PORTABLE_NAME_MAX`].
    pub(crate) fn component_limit(self) -> usize {
        self.name_max
            .0
            .map_or(PORTABLE_NAME_MAX, |limit| limit.min(PORTABLE_NAME_MAX))
    }

    /// Whether the root's filesystem can hold `component` as one name: within the component
    /// limit, and without a `:` where the filesystem refuses one. Each rule is checked on its
    /// own; a refused colon does not bring in any other host's restrictions.
    pub(crate) fn admits(self, component: &OsStr) -> bool {
        let bytes = component.as_encoded_bytes();
        let colon = match self.colon {
            Colon::Accepted => true,
            Colon::Rejected => !bytes.contains(&b':'),
        };
        colon && bytes.len() <= self.component_limit()
    }
}

/// Why the naming rules of a root could not be established.
#[derive(Debug)]
pub(crate) enum ProbeError {
    /// The probe could not create, list or remove its entries, as on a read-only or full
    /// filesystem.
    Io(io::Error),
    /// The filesystem listed a probe name under another spelling, such as another letter case
    /// or Unicode normalization. An exported object would come back from the next scan under
    /// that spelling and be ingested as a second object.
    RewritesNames,
}

impl ProbeError {
    /// The pause this failure leaves. The probe has no pause reason of its own: an unwritable
    /// root is an I/O failure, and a rewriting one stops like any failure that names no other
    /// reason.
    pub(crate) fn pause_reason(&self) -> PauseReason {
        match self {
            Self::Io(_) => PauseReason::IoFailure,
            Self::RewritesNames => PauseReason::ObserverFailure,
        }
    }
}

impl From<io::Error> for ProbeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<crate::fsutil::FsError> for ProbeError {
    fn from(error: crate::fsutil::FsError) -> Self {
        Self::Io(error.into())
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Io(_) => {
                "Cannot probe the file naming rules of sync_dir in its .gcs-mimic-sync-work directory; make sync_dir writable with free space, then request a rescan"
            }
            // The status reports the generic pause remedy, which asks for a rescan; this text has
            // to say that a rescan cannot help and name the step that does.
            Self::RewritesNames => {
                "sync_dir cannot be a synchronization root: its filesystem lists file names under another spelling than they were created with (letter case or Unicode normalization), so exported objects would come back as second objects. A rescan pauses again; set sync_dir to a directory on a filesystem that keeps file names as created and restart"
            }
        })
    }
}

impl Error for ProbeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.typed_source().map(Link::as_error)
    }
}

impl TypedSource for ProbeError {
    fn typed_source(&self) -> Option<Link<'_>> {
        match self {
            Self::Io(error) => Some(Link::Io(error)),
            Self::RewritesNames => None,
        }
    }
}

/// The rules a root recorded and the other rules a later probe of the same root observed.
#[derive(Debug, PartialEq, Eq, Serialize)]
struct NamingChange {
    recorded: RootNaming,
    observed: RootNaming,
}

/// Probes the root's naming rules in its workspace, records them on the lease that every clone
/// of it shares and reports them. A root whose filesystem rewrites names is refused here, before
/// a scan could ingest a rewritten spelling as a second object.
pub(super) async fn activate(writeback: &Writeback) -> Result<RootNaming, SyncError> {
    let observed = RootNaming::try_from(writeback.probe_naming().await??)?;
    let lease = writeback.root_lease();
    if let Err(error) = lease.record_naming(observed) {
        // `sync_root_naming` reports only recorded rules, so the stop names both sets here: the
        // operator sees which rule changed under the root.
        if let Ok(recorded) = lease.naming() {
            diagnostics::emit(
                LogLevel::Error,
                "sync_root_naming_changed",
                NamingChange { recorded, observed },
            );
        }
        return Err(error.into());
    }
    diagnostics::emit(LogLevel::Info, "sync_root_naming", observed);
    Ok(observed)
}
