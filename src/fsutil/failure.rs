//! What an I/O failure says about the file system beyond the one operation that met it. A step
//! that retries, pauses or gives up reads this class instead of matching error kinds of its own,
//! so two steps cannot disagree about whether the same failure passes.

use std::io;

/// How far an I/O failure reaches. A failure with no class, such as a missing permission or a
/// missing file, concerns only the path the operation named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IoFailureClass {
    /// It passes on its own: an interrupted or timed-out call, a busy resource, or descriptor or
    /// memory pressure.
    Transient,
    /// The whole file system's: a full disk, an exhausted quota or a read-only file system fails
    /// every later write there as well, so no one path is to blame.
    Volume,
}

impl IoFailureClass {
    /// What an I/O failure's kind alone says.
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "io::ErrorKind is non-exhaustive, and a kind this match does not name has no class"
    )]
    fn of_kind(kind: io::ErrorKind) -> Option<Self> {
        match kind {
            io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::Interrupted
            | io::ErrorKind::ResourceBusy => Some(Self::Transient),
            io::ErrorKind::StorageFull
            | io::ErrorKind::QuotaExceeded
            | io::ErrorKind::ReadOnlyFilesystem => Some(Self::Volume),
            _ => None,
        }
    }

    /// What an I/O failure says, with exhausted descriptors or memory, which its kind does not
    /// name, as transient.
    #[cfg_attr(not(test), expect(dead_code, reason = "the seed watcher reuses this"))]
    pub(crate) fn of(error: &io::Error) -> Option<Self> {
        Self::of_kind(error.kind()).or_else(|| exhausted(error).then_some(Self::Transient))
    }
}

/// Whether the failure's errno reports exhausted descriptors or memory.
#[cfg(unix)]
fn exhausted(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::EAGAIN | libc::ENOMEM)
    )
}

/// Elsewhere a raw OS error is no errno: Windows reports a Win32 error code, whose numbers
/// mean something else, so it names no exhaustion here.
#[cfg(not(unix))]
fn exhausted(_: &io::Error) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A busy resource passes on its own, each kind of a whole file system's failure is the
    /// volume's, and on Unix exhausted descriptors or memory pass although their kind names
    /// neither.
    #[test]
    fn each_io_kind_has_one_class() {
        for kind in [
            io::ErrorKind::TimedOut,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::Interrupted,
            io::ErrorKind::ResourceBusy,
        ] {
            assert_eq!(
                IoFailureClass::of_kind(kind),
                Some(IoFailureClass::Transient),
                "{kind:?}"
            );
            assert_eq!(
                IoFailureClass::of(&io::Error::from(kind)),
                Some(IoFailureClass::Transient),
                "{kind:?}"
            );
        }
        for kind in [
            io::ErrorKind::StorageFull,
            io::ErrorKind::QuotaExceeded,
            io::ErrorKind::ReadOnlyFilesystem,
        ] {
            assert_eq!(
                IoFailureClass::of_kind(kind),
                Some(IoFailureClass::Volume),
                "{kind:?}"
            );
        }
        #[cfg(unix)]
        for errno in [libc::EMFILE, libc::ENFILE, libc::EAGAIN, libc::ENOMEM] {
            assert_eq!(
                IoFailureClass::of(&io::Error::from_raw_os_error(errno)),
                Some(IoFailureClass::Transient),
                "{errno}"
            );
        }
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::NotFound] {
            assert_eq!(IoFailureClass::of_kind(kind), None, "{kind:?}");
            assert_eq!(IoFailureClass::of(&io::Error::from(kind)), None, "{kind:?}");
        }
    }

    /// 24 is EMFILE on Unix but ERROR_BAD_LENGTH on Windows, so a raw Windows error code is never
    /// read as an exhausted errno.
    #[cfg(windows)]
    #[test]
    fn a_windows_error_code_is_no_errno() {
        assert_eq!(IoFailureClass::of(&io::Error::from_raw_os_error(24)), None);
    }
}
