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
        (Error::Admin(storage_full()), "Io"),
        (
            Error::Admin(AdminError::Corrupt(BackendCause::new("row"))),
            "Corrupt",
        ),
        (
            Error::Admin(AdminError::Backend(BackendCause::new("pool"))),
            "Unavailable",
        ),
        (Error::Admin(AdminError::Forbidden), "Forbidden"),
        (Error::Admin(AdminError::Precondition), "PreconditionFailed"),
        (Error::Admin(AdminError::NotFound), "PreconditionFailed"),
        (Error::TargetChanged, "PreconditionFailed"),
    ] {
        let mapped = restore_check_error(error);
        let variant = match &mapped {
            ObjectError::Io(_) => "Io",
            ObjectError::Corrupt(_) => "Corrupt",
            ObjectError::Unavailable(_) => "Unavailable",
            ObjectError::Forbidden => "Forbidden",
            ObjectError::PreconditionFailed => "PreconditionFailed",
            _ => "other",
        };
        assert_eq!(variant, expected, "{mapped:?}");
    }
    assert!(matches!(
        restore_check_error(Error::Admin(storage_full())),
        ObjectError::Io(cause) if cause.kind() == io::ErrorKind::StorageFull
    ));
}
