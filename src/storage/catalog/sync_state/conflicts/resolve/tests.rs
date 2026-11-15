use super::*;
use crate::storage::failure::BackendCause;

#[test]
fn restore_check_keeps_storage_failures_apart_from_a_changed_target() {
    let storage_full = || AdminError::DiskFull(io::ErrorKind::StorageFull.into());
    for (error, expected) in [
        (
            Error::Admin(AdminError::Io(io::ErrorKind::Other.into())),
            "Io",
        ),
        (Error::Admin(storage_full()), "DiskFull"),
        (
            Error::Admin(AdminError::Corrupt(BackendCause::new("row"))),
            "Corrupt",
        ),
        (
            Error::Admin(AdminError::Backend(BackendCause::new("pool"))),
            "Backend",
        ),
        (Error::Admin(AdminError::Forbidden), "Forbidden"),
        (Error::Admin(AdminError::Precondition), "Precondition"),
        (Error::Admin(AdminError::NotFound), "Precondition"),
        (Error::TargetChanged, "Precondition"),
    ] {
        // The publication batch stops on every storage failure and fails only the element on a
        // precondition.
        let mapped = restore_check_error(error);
        let variant = match &mapped {
            AdminError::Io(_) => "Io",
            AdminError::DiskFull(_) => "DiskFull",
            AdminError::Corrupt(_) => "Corrupt",
            AdminError::Backend(_) => "Backend",
            AdminError::Forbidden => "Forbidden",
            AdminError::Precondition => "Precondition",
            _ => "other",
        };
        assert_eq!(variant, expected, "{mapped:?}");
    }
    assert!(matches!(
        restore_check_error(Error::Admin(storage_full())),
        AdminError::DiskFull(cause) if cause.kind() == io::ErrorKind::StorageFull
    ));
}

/// Contention and a closing catalog are not a changed target: the batch stops with them as it
/// does when they happen anywhere else in a publication.
#[test]
fn restore_check_stops_the_batch_on_contention_and_a_closing_catalog() {
    let busy = restore_check_error(Error::Admin(AdminError::Busy {
        retry_after: std::time::Duration::from_secs(1),
    }));
    assert!(
        matches!(busy, AdminError::Busy { retry_after } if retry_after.as_secs() == 1),
        "{busy:?}"
    );
    let closed = restore_check_error(Error::Admin(AdminError::Closed));
    assert!(matches!(closed, AdminError::Closed), "{closed:?}");
}
