use super::*;

fn sqlite_failure(code: std::ffi::c_int) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None)
}

#[test]
fn sqlite_corruption_and_decoding_failures_are_corrupt_with_their_text() {
    let corrupt = sqlite_failure(rusqlite::ffi::SQLITE_CORRUPT);
    let text = corrupt.to_string();
    let error = unavailable(corrupt);
    assert!(matches!(&error, CatalogError::Corrupt(cause) if cause.message() == text));
    assert_eq!(error.to_string(), format!("catalog corrupt: {text}"));
    assert!(
        error
            .source()
            .is_some_and(|source| source.to_string() == text)
    );

    let decoding = rusqlite::Error::InvalidColumnType(0, "x".into(), rusqlite::types::Type::Null);
    assert!(matches!(
        object_unavailable(decoding),
        ObjectError::Corrupt(_)
    ));
}

#[test]
fn full_sqlite_disk_and_filesystem_errors_are_io_with_their_kind() {
    let error = unavailable(sqlite_failure(rusqlite::ffi::SQLITE_FULL));
    assert!(
        matches!(&error, CatalogError::Io(cause) if cause.kind() == io::ErrorKind::StorageFull)
    );
    assert_eq!(error.to_string(), "catalog I/O: no storage space");
    assert!(error.source().is_some());

    let error = object_unavailable(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
    assert_eq!(
        error,
        ObjectError::Io(io::ErrorKind::PermissionDenied.into())
    );
    assert_eq!(error.to_string(), "object I/O: permission denied");
    assert_eq!(format!("{error:?}"), "Io(PermissionDenied)");
    assert!(
        error
            .source()
            .is_some_and(|source| source.to_string() == "denied")
    );
}

#[test]
fn other_failures_are_unavailable_with_the_stringified_message() {
    let busy = sqlite_failure(rusqlite::ffi::SQLITE_BUSY);
    let text = busy.to_string();
    let error = unavailable(busy);
    assert_eq!(error.to_string(), format!("catalog unavailable: {text}"));
    assert_eq!(format!("{error:?}"), format!("Unavailable({text:?})"));
    assert!(error.source().is_some());

    let error = object_unavailable("catalog is closed");
    assert!(matches!(&error, ObjectError::Unavailable(cause) if *cause == "catalog is closed"));
    assert!(error.source().is_none());
}

#[test]
fn classification_passes_through_already_classified_errors() {
    let nested = unavailable(ObjectError::Corrupt("bad row".into()));
    assert_eq!(nested, CatalogError::Corrupt("bad row".into()));
    let nested = object_unavailable(CatalogError::Io(io::ErrorKind::NotFound.into()));
    assert_eq!(nested, ObjectError::Io(io::ErrorKind::NotFound.into()));
    // Unclassified errors keep their full display text, as before sources were kept.
    let nested = unavailable(CatalogError::NotFound);
    assert_eq!(nested.to_string(), "catalog unavailable: bucket not found");
}
