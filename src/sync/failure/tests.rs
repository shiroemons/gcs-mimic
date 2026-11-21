use super::*;
use crate::storage::sync_state::writeback::StaleReason;

/// A busy resource passes on its own wherever it is met, and each kind of a whole file system's
/// failure is the volume's, whether it comes as a kind or under an admin failure.
#[test]
fn each_io_kind_has_one_class() {
    for kind in [
        io::ErrorKind::TimedOut,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::Interrupted,
        io::ErrorKind::ResourceBusy,
    ] {
        assert_eq!(Failure::of_kind(kind), Some(Failure::Transient), "{kind:?}");
        assert_eq!(
            Failure::of_admin(&AdminError::Io(io::Error::from(kind).into())),
            Some(Failure::Transient),
            "{kind:?}"
        );
    }
    for kind in [
        io::ErrorKind::StorageFull,
        io::ErrorKind::QuotaExceeded,
        io::ErrorKind::ReadOnlyFilesystem,
    ] {
        assert_eq!(Failure::of_kind(kind), Some(Failure::Volume), "{kind:?}");
    }
    for errno in [libc::EMFILE, libc::ENFILE, libc::EAGAIN, libc::ENOMEM] {
        assert_eq!(
            Failure::of_io(&io::Error::from_raw_os_error(errno)),
            Some(Failure::Transient),
            "{errno}"
        );
    }
    for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::NotFound] {
        assert_eq!(Failure::of_kind(kind), None, "{kind:?}");
    }
}

/// An entry that kept failing to open is looked at again; any other race, a moot operation and
/// a broken invariant plan the step again; write-back stops are recovery's.
#[test]
fn a_write_back_failure_is_classified_by_what_it_says_about_the_step() {
    assert_eq!(
        Failure::of_writeback(&writeback::Error::Raced(RaceReason::Unopenable)),
        Some(Failure::Transient)
    );
    for error in [
        writeback::Error::Raced(RaceReason::TargetChanged),
        writeback::Error::Obsolete(StaleReason::Tracked),
        writeback::Error::Invariant("fixture"),
        writeback::Error::Admin(AdminError::Precondition),
        writeback::Error::Admin(AdminError::Io(
            io::Error::from(io::ErrorKind::AlreadyExists).into(),
        )),
    ] {
        assert_eq!(
            Failure::of_writeback(&error),
            Some(Failure::Superseded),
            "{error:?}"
        );
    }
    for error in [
        writeback::Error::RecoveryRequired,
        writeback::Error::Protected,
        writeback::Error::Root(RootError::Admin(AdminError::Precondition)),
    ] {
        assert_eq!(Failure::of_writeback(&error), None, "{error:?}");
    }
}
