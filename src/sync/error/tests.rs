use super::*;
use crate::error::Report;

fn admin() -> AdminError {
    AdminError::from(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "denied fixture",
    ))
}

fn root() -> RootError {
    RootError::Admin(admin())
}

fn native() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        io::Error::from_raw_os_error(libc::EACCES),
    )
}

fn export_failure() -> writeback::Error {
    writeback::Error::Root(root())
}

fn conflict() -> conflicts::Error {
    conflicts::Error::Admin(admin())
}

fn panicked() -> JobPanicked {
    crate::task::catch_job("sync-error-fixture", || panic!("sync error fixture panic"))
        .expect_err("the fixture job panics")
}

/// Diagnostics print a failure through `Report`; carrying it typed must not change the text the
/// `io::Error::other` wrapper produced.
#[test]
fn typed_failures_report_the_text_of_their_former_io_wrappers() {
    for (typed, wrapped) in [
        (SyncError::Admin(admin()), io::Error::other(admin())),
        (SyncError::Root(root()), io::Error::other(root())),
        (
            SyncError::Writeback(export_failure()),
            io::Error::other(export_failure()),
        ),
        (
            SyncError::Conflict(conflict()),
            io::Error::other(conflict()),
        ),
        (SyncError::Io(native()), io::Error::other(native())),
        (
            SyncError::Recovery(Stop::ConflictCapacity),
            io::Error::other(Stop::ConflictCapacity),
        ),
        (
            SyncError::Panicked(panicked()),
            io::Error::other(panicked()),
        ),
    ] {
        assert_eq!(Report(&typed).to_string(), Report(&wrapped).to_string());
        assert_eq!(typed.to_string(), wrapped.to_string());
    }
    assert_eq!(
        Report(&SyncError::Root(root())).to_string(),
        "synchronization root: admin operation failed: administrative operation: Io(PermissionDenied): denied fixture"
    );
}

/// Failures that had only a message keep it word for word, both alone and behind the public
/// `io::Error` boundary.
#[test]
fn message_only_failures_keep_their_former_text() {
    for (typed, message) in [
        (
            SyncError::WritebackUnfinished(Pass::Reconcile),
            "Unfinished write-back requires recovery; inspect retained conflicts and workspace findings, resolve the exact retained revision and rescan",
        ),
        (
            SyncError::WritebackUnfinished(Pass::Ingest),
            "Unfinished write-back blocks ingestion; inspect retained conflicts and recovery findings, preserve the workspace and resolve the exact retained revision before rescanning",
        ),
        (
            SyncError::RescanStopped(PauseReason::ConflictCapacity),
            "Explicit rescan stopped synchronization: ConflictCapacity",
        ),
        (
            SyncError::Invariant("invalid sync path"),
            "invalid sync path",
        ),
    ] {
        assert_eq!(
            Report(&typed).to_string(),
            Report(&io::Error::other(message)).to_string()
        );
        assert!(Error::source(&typed).is_none());
        let boundary = io::Error::from(typed);
        assert_eq!(boundary.kind(), io::ErrorKind::Other);
        assert_eq!(Report(&boundary).to_string(), message);
    }
}

/// The public boundary hands a native failure back unchanged, so callers still see its kind and
/// raw OS error; a typed failure stays reachable below an `Other` wrapper.
#[test]
fn native_failures_keep_their_kind_and_errno_at_the_io_boundary() {
    let error = io::Error::from(SyncError::Io(io::Error::from_raw_os_error(libc::EMFILE)));
    assert_eq!(error.raw_os_error(), Some(libc::EMFILE));
    let error = io::Error::from(SyncError::Io(native()));
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<io::Error>())
            .and_then(io::Error::raw_os_error),
        Some(libc::EACCES)
    );
    let error = io::Error::from(SyncError::Root(root()));
    assert_eq!(error.kind(), io::ErrorKind::Other);
    assert!(matches!(
        error.get_ref().unwrap().downcast_ref::<SyncError>(),
        Some(SyncError::Root(RootError::Admin(AdminError::Io(_))))
    ));
}

/// Only a precondition or a missing row reported by the root restarts a deletion pass; the same
/// failures from elsewhere, and storage failures, still fail it.
#[test]
fn only_root_preconditions_and_missing_rows_are_stale_deletions() {
    for error in [AdminError::Precondition, AdminError::NotFound] {
        assert!(SyncError::Root(RootError::Admin(error)).is_stale_deletion());
    }
    for error in [
        SyncError::Admin(AdminError::Precondition),
        SyncError::Admin(AdminError::NotFound),
        SyncError::Root(RootError::Admin(AdminError::Hold)),
        SyncError::Root(root()),
        SyncError::Root(RootError::RootChanged),
    ] {
        assert!(!error.is_stale_deletion(), "{error:?}");
    }
}
