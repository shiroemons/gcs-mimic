use super::*;
use crate::storage::sync_state::writeback::StaleReason;
use crate::sync::SyncError;

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
        assert_eq!(
            FailureClass::of_kind(kind),
            Some(FailureClass::Transient),
            "{kind:?}"
        );
        assert_eq!(
            FailureClass::of_admin(&AdminError::Io(io::Error::from(kind).into())),
            Some(FailureClass::Transient),
            "{kind:?}"
        );
    }
    for kind in [
        io::ErrorKind::StorageFull,
        io::ErrorKind::QuotaExceeded,
        io::ErrorKind::ReadOnlyFilesystem,
    ] {
        assert_eq!(
            FailureClass::of_kind(kind),
            Some(FailureClass::Volume),
            "{kind:?}"
        );
    }
    for errno in [libc::EMFILE, libc::ENFILE, libc::EAGAIN, libc::ENOMEM] {
        assert_eq!(
            FailureClass::of_io(&io::Error::from_raw_os_error(errno)),
            Some(FailureClass::Transient),
            "{errno}"
        );
    }
    for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::NotFound] {
        assert_eq!(FailureClass::of_kind(kind), None, "{kind:?}");
    }
}

/// An entry that kept failing to open is looked at again; any other race, a moot operation and
/// a broken invariant plan the step again; write-back stops are recovery's.
#[test]
fn a_write_back_failure_is_classified_by_what_it_says_about_the_step() {
    assert_eq!(
        FailureClass::of_writeback(&writeback::Error::Raced(RaceReason::Unopenable)),
        Some(FailureClass::Transient)
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
            FailureClass::of_writeback(&error),
            Some(FailureClass::Superseded),
            "{error:?}"
        );
    }
    for error in [
        writeback::Error::RecoveryRequired,
        writeback::Error::Protected,
        writeback::Error::Root(RootError::Admin(AdminError::Precondition)),
    ] {
        assert_eq!(FailureClass::of_writeback(&error), None, "{error:?}");
    }
}

fn root() -> RootError {
    RootError::Admin(AdminError::from(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "denied fixture",
    )))
}

/// A catalog write about one name that an API or bucket change made moot is done, not a
/// failure that pauses the mirror: one decision serves a finding, its removal and a path stop,
/// and a deletion pass that met the same change starts over. Any other failure still fails.
#[test]
fn a_write_that_a_concurrent_change_made_moot_is_done() {
    assert_eq!(unless_superseded(Ok(())).unwrap(), Recorded::Yes);
    for moot in [AdminError::Precondition, AdminError::NotFound] {
        let error = RootError::Admin(moot);
        assert!(superseded(&error), "{error:?}");
        assert_eq!(unless_superseded(Err(error)).unwrap(), Recorded::Superseded);
    }
    for moot in [AdminError::Precondition, AdminError::NotFound] {
        assert!(SyncError::Root(RootError::Admin(moot)).is_stale_deletion());
    }
    assert!(!superseded(&root()));
    assert!(unless_superseded(Err(root())).is_err());
    assert!(!SyncError::Root(root()).is_stale_deletion());
    assert!(unless_superseded(Err(RootError::InUse)).is_err());
}
