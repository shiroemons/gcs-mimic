use super::*;
use crate::error::Report;
use crate::storage::failure::{IoCause, Source};
use crate::sync::failure::{Recorded, superseded, unless_superseded};

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

/// A running rescan that stopped synchronization for `reason`, whose caller has gone.
fn rescan_stopped(reason: PauseReason) -> SyncError {
    SyncError::RescanStopped(Box::new(StoppedRescan {
        reason,
        error: SyncError::Invariant("rescan fixture"),
        reply: tokio::sync::oneshot::channel().0,
    }))
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

/// The decisions read the typed chain and reports read `source()`; both visit the same errors,
/// node for node.
#[test]
fn typed_causes_visit_every_source_node() {
    for error in [
        SyncError::Admin(admin()),
        SyncError::Root(root()),
        SyncError::Writeback(export_failure()),
        SyncError::Conflict(conflict()),
        SyncError::Io(native()),
        SyncError::Recovery(Stop::IoFailure),
        SyncError::Panicked(panicked()),
        nested(writeback::Error::Displaced(Box::new(export_failure()))),
    ] {
        let typed: Vec<_> = error
            .typed_source()
            .into_iter()
            .flat_map(chain)
            .map(Link::as_error)
            .collect();
        let plain: Vec<_> =
            std::iter::successors(Error::source(&error), |&cause| cause.source()).collect();
        assert_eq!(typed.len(), plain.len(), "{error:?}");
        for (typed, plain) in typed.into_iter().zip(plain) {
            assert!(std::ptr::addr_eq(typed, plain), "{typed} is not {plain}");
        }
    }
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
            rescan_stopped(PauseReason::ConflictCapacity),
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
        wrapped(AdminError::backend(io::Error::from(
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

/// A failure that storage carries inside a backend cause, below another typed layer.
fn nested(error: impl Into<Source>) -> SyncError {
    SyncError::Root(RootError::Admin(AdminError::backend(error)))
}

#[test]
fn nested_typed_failures_keep_retry_classification_and_pause_reasons() {
    for error in [
        nested(AdminError::Busy {
            retry_after: std::time::Duration::from_secs(1),
        }),
        nested(CatalogError::Unavailable("database is locked".into())),
        nested(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        )),
        nested(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_LOCKED),
            None,
        )),
        nested(io::Error::from(io::ErrorKind::Interrupted)),
        nested(AdminError::from(io::Error::from_raw_os_error(libc::EMFILE))),
        nested(AdminError::from(io::Error::from_raw_os_error(libc::ENFILE))),
    ] {
        assert!(error.is_transient(), "{error:?}");
        assert!(error.is_watch_retryable(), "{error:?}");
    }
    let denied = nested(io::Error::from(io::ErrorKind::PermissionDenied));
    assert!(denied.is_watch_retryable());
    assert!(!denied.is_transient());
    for (error, reason) in [
        (
            nested(writeback::Error::Capacity),
            PauseReason::ConflictCapacity,
        ),
        (
            nested(writeback::Error::Protected),
            PauseReason::RecoveryRequired,
        ),
        (
            nested(AdminError::DiskFull(std::io::ErrorKind::StorageFull.into())),
            PauseReason::IoFailure,
        ),
        (nested(AdminError::Forbidden), PauseReason::ObserverFailure),
    ] {
        assert_eq!(error.pause_reason(), reason, "{error:?}");
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

/// A displaced installation and a failed conflict store are typed write-back variants; the
/// write-back retry still leaves them alone, and the mirror still sees the failure inside them.
#[test]
fn typed_displaced_and_conflict_failures_decide_retry_and_pause_like_backend_causes() {
    let displaced = || {
        writeback::Error::Displaced(Box::new(writeback::Error::Admin(AdminError::Io(
            io::ErrorKind::TimedOut.into(),
        ))))
    };
    for error in [
        SyncError::Writeback(displaced()),
        SyncError::Admin(AdminError::backend(displaced())),
    ] {
        assert!(error.is_transient(), "{error:?}");
        assert!(error.is_watch_retryable(), "{error:?}");
        assert_eq!(error.pause_reason(), PauseReason::IoFailure);
    }
    let capacity = SyncError::Writeback(writeback::Error::Conflict(conflicts::Error::Capacity));
    assert!(!capacity.is_transient());
    assert_eq!(capacity.pause_reason(), PauseReason::ConflictCapacity);
}

/// A native watch that fails while listing the tree hands the listing's typed failure back, and
/// the mirror keeps it inside a backend cause; it still retries descriptor exhaustion and gives up
/// on an invalid tree.
#[test]
fn tree_listing_failures_inside_native_watch_errors_keep_their_retry_decisions() {
    let watch = |error: AdminError| SyncError::Root(RootError::from(AdminError::backend(error)));
    let exhausted = watch(AdminError::from(io::Error::from_raw_os_error(libc::EMFILE)));
    assert!(exhausted.is_transient(), "{exhausted:?}");
    assert!(exhausted.is_watch_retryable(), "{exhausted:?}");
    let invalid = watch(AdminError::Invalid("tree entry is not valid".into()));
    assert!(!invalid.is_transient(), "{invalid:?}");
    assert!(!invalid.is_watch_retryable(), "{invalid:?}");
}

/// A native watch that reaches `max_sync_entries` or the kernel's watch budget names the limit to
/// raise, word for word as the `io::Error::other` wrapper it replaced, with the kernel's error
/// below it. No rebuild lifts a limit, so the mirror pauses as an observer failure.
#[test]
fn native_watch_limits_keep_their_text_and_pause_without_retrying() {
    use crate::source_tree::watch::{self, WatchLimit};
    use crate::storage::failure::BackendCause;
    let watch = |error: watch::Error| SyncError::Root(RootError::from(AdminError::backend(error)));
    let former = |error: io::Error| SyncError::Root(RootError::from(AdminError::backend(error)));
    let configured = "sync native watch limit (1 directories) exceeded; raise max_sync_entries or reduce the tree instead of leaving directories unobserved";
    let kernel = "the kernel's inotify watch limit is exhausted; raise fs.inotify.max_user_watches on the host or reduce the tree";
    let exhausted = || io::Error::from_raw_os_error(libc::ENOSPC);
    for (typed, wrapped, report) in [
        (
            watch(watch::Error::Limit(WatchLimit::Configured {
                directories: 1,
            })),
            former(io::Error::other(configured)),
            format!(
                "synchronization root: admin operation failed: administrative operation: Backend({configured:?}): {configured}"
            ),
        ),
        (
            watch(watch::Error::Limit(WatchLimit::Kernel(exhausted()))),
            former(io::Error::other(BackendCause::with_source(
                kernel,
                exhausted(),
            ))),
            format!(
                "synchronization root: admin operation failed: administrative operation: Backend({kernel:?}): {kernel}: {}",
                exhausted()
            ),
        ),
    ] {
        assert_eq!(Report(&typed).to_string(), report);
        assert_eq!(Report(&wrapped).to_string(), report);
        for error in [&typed, &wrapped] {
            assert!(!error.is_transient(), "{error:?}");
            assert!(!error.is_watch_retryable(), "{error:?}");
            assert_eq!(
                error.pause_reason(),
                PauseReason::ObserverFailure,
                "{error:?}"
            );
        }
    }
}

/// A write-back summary that fails while the observation is read travels inside the catalog's
/// I/O cause. Contention inside it still makes the pass transient, and the I/O cause that
/// carries it names the pause.
#[test]
fn write_back_summary_failures_inside_catalog_io_causes_retry_and_pause_on_io() {
    let summary = |error: writeback::Error| {
        SyncError::Root(RootError::from(AdminError::from(CatalogError::Io(
            IoCause::other(error),
        ))))
    };
    let busy = summary(writeback::Error::Admin(AdminError::Busy {
        retry_after: std::time::Duration::from_secs(1),
    }));
    assert!(busy.is_transient(), "{busy:?}");
    assert_eq!(busy.pause_reason(), PauseReason::IoFailure);
    let invalid = summary(writeback::Error::Admin(AdminError::Invalid(
        "write-back row".into(),
    )));
    assert!(!invalid.is_transient(), "{invalid:?}");
    assert_eq!(invalid.pause_reason(), PauseReason::IoFailure);
}

/// A catalog connection left unusable by a rollback that met a busy database is contention, so
/// the mirror retries the pass.
#[test]
fn a_busy_rollback_behind_an_unusable_catalog_connection_is_transient() {
    let error = SyncError::Admin(crate::storage::test_fixtures::unrecovered_busy_connection());
    assert!(matches!(error, SyncError::Admin(AdminError::Backend(_))));
    assert!(error.is_transient(), "{error:?}");
    assert!(error.is_watch_retryable(), "{error:?}");
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
        rescan_stopped(PauseReason::ConflictCapacity).pause_reason(),
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

/// A failure at one export's path keeps that object's finding for a later pass, while one of
/// the whole file system fails the pass, however deep in the chain it sits.
#[test]
fn only_a_failure_at_one_path_concerns_one_object() {
    for one in [
        SyncError::Admin(AdminError::Io(io::ErrorKind::PermissionDenied.into())),
        nested(AdminError::Io(io::ErrorKind::PermissionDenied.into())),
        SyncError::Writeback(writeback::Error::Admin(AdminError::Io(
            io::ErrorKind::InvalidFilename.into(),
        ))),
        SyncError::Conflict(conflicts::Error::TargetChanged),
    ] {
        assert!(one.concerns_one_object(), "{one:?}");
    }
    for kind in [
        io::ErrorKind::StorageFull,
        io::ErrorKind::QuotaExceeded,
        io::ErrorKind::ReadOnlyFilesystem,
    ] {
        for volume in [
            SyncError::Admin(AdminError::DiskFull(kind.into())),
            SyncError::Admin(AdminError::Io(kind.into())),
            nested(io::Error::from(kind)),
            SyncError::Writeback(writeback::Error::Admin(AdminError::Io(kind.into()))),
        ] {
            assert!(!volume.concerns_one_object(), "{volume:?}");
        }
    }
    let capacity = SyncError::Writeback(writeback::Error::Capacity);
    assert!(!capacity.concerns_one_object());
}

/// A native watch whose build meets a directory that keeps failing to open while its name still
/// holds one fails as a failure that passes, so the mirror builds the watch again rather than
/// stopping, as it does for a build that met exhausted descriptors.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_watch_build_that_meets_a_directory_that_keeps_failing_to_open_is_built_again() {
    let tree = crate::test_support::TestDir::new();
    std::fs::create_dir(tree.0.join("bucket")).unwrap();
    let root = crate::fsutil::open_directory_nofollow(&tree.0).unwrap();
    let _failing = crate::fsutil::FailingOpens::vanishing(&tree.0, "bucket", usize::MAX);
    let Err(error) =
        crate::source_tree::watch::build(&root, &tree.0, crate::source_tree::TreeLimits::default())
    else {
        panic!("the watch was built over a directory that does not open");
    };
    let error = SyncError::Root(RootError::from(AdminError::backend(error)));
    assert!(error.is_watch_retryable(), "{error:?}");
    assert!(error.is_transient(), "{error:?}");
}

/// A root marker that keeps failing to open while its name still holds it fails the activation
/// as a failure that passes, which the mirror tries again, and the marker stays.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[tokio::test]
async fn a_marker_that_keeps_failing_to_open_fails_activation_as_a_failure_that_passes() {
    use crate::storage::sync_state::root::Activation;
    use crate::sync::mapping::MARKER;
    let data = crate::test_support::TestDir::new();
    let tree = crate::test_support::TestDir::new();
    let catalog = crate::storage::Catalog::open(&data.0).await.unwrap();
    drop(
        catalog
            .sync_state()
            .activate_root(&tree.0, Activation::Open)
            .await
            .unwrap(),
    );
    let failing = crate::fsutil::FailingOpens::vanishing(&tree.0, MARKER, usize::MAX);
    let Err(error) = catalog
        .sync_state()
        .activate_root(&tree.0, Activation::Open)
        .await
    else {
        panic!("the root was activated over a marker that does not open");
    };
    drop(failing);
    let error = SyncError::Root(error);
    assert!(error.is_transient(), "{error:?}");
    assert!(tree.0.join(MARKER).exists());
    catalog.close().await.unwrap();
}
