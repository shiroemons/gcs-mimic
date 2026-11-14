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

#[test]
fn only_contention_and_timeouts_are_transient() {
    let wrapped = SyncError::Admin;
    for transient in [
        wrapped(AdminError::Busy {
            retry_after: std::time::Duration::from_secs(1),
        }),
        wrapped(AdminError::Io(io::ErrorKind::TimedOut.into())),
        wrapped(AdminError::backend(CatalogError::Unavailable(
            "database is locked".into(),
        ))),
        SyncError::Io(io::Error::from(io::ErrorKind::TimedOut)),
        SyncError::Io(io::Error::other(io::Error::from(
            io::ErrorKind::Interrupted,
        ))),
    ] {
        assert!(transient.is_transient(), "{transient:?}");
    }
    for unrecoverable in [
        wrapped(AdminError::Invalid("object name is not valid".into())),
        wrapped(AdminError::Forbidden),
        wrapped(AdminError::Corrupt("catalog row".into())),
        wrapped(AdminError::Closed),
        wrapped(AdminError::DiskFull(std::io::ErrorKind::StorageFull.into())),
        SyncError::Io(io::Error::from(io::ErrorKind::PermissionDenied)),
        SyncError::Io(io::Error::other("injected live sync failure")),
    ] {
        assert!(!unrecoverable.is_transient(), "{unrecoverable:?}");
    }
}

/// Write-back that fails under descriptor or memory pressure is retried by the mirror too.
#[test]
fn descriptor_and_memory_pressure_below_write_back_is_transient() {
    for errno in [libc::EMFILE, libc::ENFILE, libc::EAGAIN] {
        let error = writeback::Error::Admin(io::Error::from_raw_os_error(errno).into());
        assert!(SyncError::Writeback(error).is_transient());
    }
}

#[test]
fn nested_io_wrappers_keep_retry_classification_and_pause_reasons() {
    for error in [
        io::Error::other(AdminError::Busy {
            retry_after: std::time::Duration::from_secs(1),
        }),
        io::Error::other(CatalogError::Unavailable("database is locked".into())),
        io::Error::other(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        )),
        io::Error::other(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
            None,
        )),
        io::Error::other(io::Error::from(io::ErrorKind::Interrupted)),
        io::Error::other(AdminError::from(io::Error::from_raw_os_error(libc::EMFILE))),
        io::Error::other(AdminError::from(io::Error::from_raw_os_error(libc::ENFILE))),
    ] {
        let nested = SyncError::Io(io::Error::other(error));
        assert!(nested.is_transient(), "{nested:?}");
        assert!(nested.is_watch_retryable(), "{nested:?}");
    }
    let denied = SyncError::Io(io::Error::other(io::Error::other(io::Error::from(
        io::ErrorKind::PermissionDenied,
    ))));
    assert!(denied.is_watch_retryable());
    assert!(!denied.is_transient());
    for (error, reason) in [
        (
            io::Error::other(writeback::Error::Capacity),
            PauseReason::ConflictCapacity,
        ),
        (
            io::Error::other(writeback::Error::Protected),
            PauseReason::RecoveryRequired,
        ),
        (
            io::Error::other(AdminError::DiskFull(std::io::ErrorKind::StorageFull.into())),
            PauseReason::IoFailure,
        ),
        (
            io::Error::other(AdminError::Forbidden),
            PauseReason::ObserverFailure,
        ),
    ] {
        assert_eq!(
            SyncError::Io(io::Error::other(error)).pause_reason(),
            reason
        );
    }
}

/// A displaced live copy keeps its write-back failure inside `AdminError::Backend` so that the
/// write-back retry leaves it alone; the mirror still sees the timeout below it, retries the
/// pass and, once it pauses, names the storage failure.
#[test]
fn write_back_failures_kept_as_backend_causes_still_decide_retry_and_pause() {
    let displaced = || {
        AdminError::backend(writeback::Error::Admin(AdminError::Io(
            io::ErrorKind::TimedOut.into(),
        )))
    };
    for error in [
        SyncError::Admin(displaced()),
        SyncError::Writeback(writeback::Error::Admin(displaced())),
    ] {
        assert!(error.is_transient(), "{error:?}");
        assert!(error.is_watch_retryable(), "{error:?}");
        assert_eq!(error.pause_reason(), PauseReason::IoFailure);
    }
    let capacity = SyncError::Admin(AdminError::backend(conflicts::Error::Capacity));
    assert!(!capacity.is_transient());
    assert_eq!(capacity.pause_reason(), PauseReason::ConflictCapacity);
}

#[test]
fn recovered_writeback_stops_pause_without_fabricating_io_causes() {
    for (stop, reason) in [
        (Stop::ConflictCapacity, PauseReason::ConflictCapacity),
        (Stop::IoFailure, PauseReason::IoFailure),
        (Stop::RecoveryRequired, PauseReason::RecoveryRequired),
        (Stop::ProtectedBody, PauseReason::RecoveryRequired),
    ] {
        let error = SyncError::Recovery(stop);
        assert_eq!(
            Error::source(&error).and_then(|cause| cause.downcast_ref::<Stop>()),
            Some(&stop)
        );
        assert!(!error.is_transient());
        assert!(!error.is_watch_retryable());
        assert_eq!(error.pause_reason(), reason);
        assert!(
            crate::error::causes(&error)
                .skip(1)
                .all(|cause| !cause.is::<io::Error>())
        );
    }
}

/// An explicit rescan's own reason wins over any cause, and failures that carry only a message
/// pause as observer failures.
#[test]
fn rescan_stops_keep_their_reason_and_messages_pause_as_observer_failures() {
    assert_eq!(
        SyncError::RescanStopped(PauseReason::ConflictCapacity).pause_reason(),
        PauseReason::ConflictCapacity
    );
    for error in [
        SyncError::WritebackUnfinished(Pass::Reconcile),
        SyncError::Invariant("invalid sync path"),
        SyncError::Panicked(panicked()),
    ] {
        assert!(!error.is_transient(), "{error:?}");
        assert_eq!(error.pause_reason(), PauseReason::ObserverFailure);
    }
}
