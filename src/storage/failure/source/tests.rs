use super::*;
use crate::storage::failure::MemoryBudget;

/// The variant shows the held error's own text, debug text, source and type, so wrapping an
/// error in a `Source` changes no report, log line or downcast.
fn assert_delegates<E: Error + Send + Sync + 'static>(make: impl Fn() -> E)
where
    Source: From<E>,
{
    let source = Source::from(make());
    let held = make();
    assert_eq!(source.to_string(), held.to_string());
    assert_eq!(format!("{source:?}"), format!("{held:?}"));
    assert_eq!(
        source.as_error().source().map(ToString::to_string),
        held.source().map(ToString::to_string)
    );
    assert!(source.as_error().is::<E>(), "{held:?} keeps its own type");
}

fn sqlite_busy() -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY), None)
}

fn closed() -> BackendCause {
    BackendCause::with_source("offline", BackendFailure::CatalogClosed)
}

#[test]
fn every_source_shows_the_held_error() {
    assert_delegates(|| BackendFailure::CatalogClosed);
    assert_delegates(|| MemoryExhausted::new(MemoryBudget::Blobs));
    assert_delegates(|| MemoryExhausted {
        budget: MemoryBudget::Catalog,
        source: Some(sqlite_busy()),
    });
    assert_delegates(closed);
    assert_delegates(|| io::Error::other("disk failed"));
    assert_delegates(sqlite_busy);
    assert_delegates(|| CatalogError::Unavailable(closed()));
    assert_delegates(|| ObjectError::Io(io::Error::other("disk failed").into()));
    assert_delegates(|| AdminError::Corrupt(closed()));
    assert_delegates(|| RootError::Admin(AdminError::Backend(closed())));
    assert_delegates(|| writeback::Error::Admin(AdminError::Backend(closed())));
    assert_delegates(|| conflicts::Error::Admin(AdminError::Backend(closed())));
    assert_delegates(|| ConnectionUnavailable::Closed);
    assert_delegates(|| ExecutorError::Closed);
    assert_delegates(|| serde_json::from_str::<u8>("not json").unwrap_err());
}
