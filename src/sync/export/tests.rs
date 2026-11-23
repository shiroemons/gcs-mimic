#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]

use super::{
    reconcile::external_files,
    retry::{EXPORT_BACKOFF, retryable, with_retry},
};
use crate::{
    retry::RetryBudget,
    storage::{
        Catalog,
        admin::{AdminError, BucketName},
        sync_state::{root::RootLease, writeback},
    },
    sync::{Reconciliation, SyncError, backoff::Backoff, interrupt::Interrupt},
    test_support::TestDir,
};
use std::{fs, io, time::Duration};

async fn live_fixture() -> (TestDir, TestDir, Catalog, BucketName, RootLease) {
    let bucket = BucketName::new("test-bucket").unwrap();
    let (data, tree, catalog, root, _) = crate::sync::test_support::fixture(Some(&bucket)).await;
    (data, tree, catalog, bucket, root)
}

#[tokio::test]
async fn external_inventory_defers_unreadable_children_without_stopping_other_files() {
    use std::os::unix::fs::PermissionsExt;
    let (_data, tree, catalog, _bucket, root) = live_fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    let blocked = tree.0.join("test-bucket/blocked");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("hidden"), b"hidden").unwrap();
    fs::write(tree.0.join("test-bucket/working"), b"working").unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let mut counts = Reconciliation::default();
    let counted = external_files(
        &root,
        &root.sync_admin(),
        Interrupt::new(&stopping),
        &mut counts,
    )
    .await;
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    counted.unwrap();
    assert_eq!(
        (counts.external_only, counts.ignored, counts.unreadable()),
        (1, 0, 1)
    );
    assert_eq!(
        counts.entries().entries(),
        [crate::sync::entries::UnrepresentableEntry {
            path: "test-bucket/blocked".into(),
            problem: crate::sync::entries::EntryProblem::Unreadable,
        }]
    );
    drop(root);
    catalog.close().await.unwrap();
}

/// Each retry waits twice as long as the one before, up to the cap, and a reset starts over.
/// Export steps wait 1 s and then 2 s before a third failure pauses synchronization.
#[test]
fn retry_backoff_doubles_up_to_its_cap_and_starts_over_after_a_reset() {
    let mut backoff = Backoff::new(Duration::from_millis(10), Duration::from_millis(70));
    for expected in [10, 20, 40, 70, 70] {
        assert_eq!(backoff.next_delay(), Duration::from_millis(expected));
    }
    assert_eq!(backoff.retries(), 5);
    backoff.reset();
    assert_eq!(backoff.retries(), 0);
    assert_eq!(backoff.next_delay(), Duration::from_millis(10));

    let mut export = EXPORT_BACKOFF;
    assert_eq!(export.next_delay(), Duration::from_secs(1));
    assert_eq!(export.next_delay(), Duration::from_secs(2));
    assert_eq!(export.retries() + 1, RetryBudget::STANDARD.attempts());
    assert!(!RetryBudget::STANDARD.allows_retry(export.retries()));
}

#[test]
fn only_typed_transient_io_failures_are_retried() {
    assert!(!retryable(&writeback::Error::Admin(AdminError::Io(
        io::Error::other("permanent corruption").into()
    ))));
    for errno in [libc::EMFILE, libc::ENFILE, libc::EAGAIN] {
        let error = writeback::Error::Admin(io::Error::from_raw_os_error(errno).into());
        assert!(retryable(&error));
    }
}

/// A write-back step whose target, a directory above it or its private entry kept failing to
/// open while the name still held an entry is looked at again, at whatever step it failed: the
/// step is retried, and a run it still fails is run again, with write-back recovery first,
/// rather than pausing the mirror. A target the user changed is no such failure: the export
/// plans around it instead.
#[test]
fn a_step_whose_entry_kept_failing_to_open_is_retried_and_does_not_pause() {
    use crate::storage::sync_state::writeback::RaceReason;
    let unsettled = writeback::Error::Raced(RaceReason::Unopenable);
    assert!(retryable(&unsettled));
    assert!(SyncError::from(unsettled).is_transient());
    for reason in [
        RaceReason::ParentChanged,
        RaceReason::Respelled,
        RaceReason::TargetChanged,
        RaceReason::DirectoryChanged,
    ] {
        let changed = writeback::Error::Raced(reason);
        assert!(!retryable(&changed), "{reason:?}");
        assert!(!SyncError::from(changed).is_transient(), "{reason:?}");
    }
}

/// An export step is tried again, within its budget, on every failure that passes or is the
/// whole file system's: contention, and a full disk, an exhausted quota or a read-only file
/// system that an operator may clear meanwhile. A missing permission is retried as well, so that
/// a repaired file or parent permission lets the export go on. A conflict or a displaced target
/// is recovery's whatever failure it carries, and a closed catalog, a refusal or a change that
/// made the step moot is not retried.
#[test]
fn an_export_step_retries_failures_that_pass_or_are_the_volumes() {
    use crate::storage::sync_state::{conflicts, root::RootError};
    let io_failure = |kind: io::ErrorKind| AdminError::Io(kind.into());
    let retried = || {
        [
            AdminError::DiskFull(io::ErrorKind::StorageFull.into()),
            io_failure(io::ErrorKind::StorageFull),
            io_failure(io::ErrorKind::QuotaExceeded),
            io_failure(io::ErrorKind::ReadOnlyFilesystem),
            io_failure(io::ErrorKind::PermissionDenied),
            io_failure(io::ErrorKind::TimedOut),
            AdminError::Busy {
                retry_after: Duration::from_secs(1),
            },
        ]
    };
    for error in retried() {
        let error = writeback::Error::Admin(error);
        assert!(retryable(&error), "{error:?}");
    }
    for error in retried() {
        let error = writeback::Error::Root(RootError::Admin(error));
        assert!(retryable(&error), "{error:?}");
    }
    for error in [
        writeback::Error::Admin(AdminError::Closed),
        writeback::Error::Admin(AdminError::Forbidden),
        writeback::Error::Admin(AdminError::Precondition),
        writeback::Error::Admin(AdminError::NotFound),
        writeback::Error::Conflict(conflicts::Error::Admin(AdminError::Busy {
            retry_after: Duration::from_secs(1),
        })),
        writeback::Error::Displaced(Box::new(writeback::Error::Admin(io_failure(
            io::ErrorKind::TimedOut,
        )))),
        writeback::Error::Capacity,
        writeback::Error::WorkspaceChanged,
    ] {
        assert!(!retryable(&error), "{error:?}");
    }
}

#[tokio::test]
async fn retry_repeats_transient_errors_and_keeps_permanent_causes() {
    let (_data, _tree, catalog, _bucket, root) = live_fixture().await;
    let mut attempts = 0;
    let result = with_retry(&root, || {
        attempts += 1;
        std::future::ready(if attempts == 1 {
            Err(writeback::Error::Admin(AdminError::Io(
                io::ErrorKind::Interrupted.into(),
            )))
        } else {
            Ok(7)
        })
    })
    .await
    .unwrap();
    assert_eq!(result, 7);
    assert_eq!(attempts, 2);
    attempts = 0;
    let error = with_retry(&root, || {
        attempts += 1;
        std::future::ready(Err::<(), _>(writeback::Error::Admin(AdminError::Forbidden)))
    })
    .await
    .unwrap_err();
    assert_eq!(attempts, 1);
    assert!(matches!(
        error,
        SyncError::Writeback(writeback::Error::Admin(AdminError::Forbidden))
    ));
    drop(root);
    catalog.close().await.unwrap();
}

#[test]
fn writeback_retries_resource_pressure_without_retrying_unclassified_failures() {
    for error in [
        io::Error::from_raw_os_error(libc::EMFILE),
        io::Error::from_raw_os_error(libc::ENFILE),
        io::ErrorKind::TimedOut.into(),
    ] {
        assert!(retryable(&writeback::Error::Admin(error.into())));
    }
    assert!(!retryable(&writeback::Error::Admin(AdminError::Io(
        io::Error::other("invalid state").into()
    ))));
}
