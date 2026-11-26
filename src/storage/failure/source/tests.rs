use super::*;
use crate::cause::MemoryBudget;
use crate::storage::catalog::{ConnectionUnavailable, ExecutorError};
use std::assert_matches;

/// The variant shows the held error's own text, debug text, source and type, so wrapping an
/// error in a `Source` changes no report, log line or downcast.
fn assert_delegates<E: Error + Send + Sync + 'static>(make: impl Fn() -> E)
where
    Source: From<E>,
{
    let source = Source::from(make());
    let held = make();
    crate::error::assert_distinct_causes(&held);
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
    assert_delegates(|| ConnectionUnavailable::Closed);
    assert_delegates(|| ExecutorError::Closed);
    assert_delegates(|| ResumeError::Catalog(CatalogError::Unavailable(closed())));
    assert_delegates(permanently_failed);
    assert_delegates(|| serde_json::from_str::<u8>("not json").unwrap_err());
}

#[test]
fn stored_timestamps_keep_the_range_error_as_the_source() {
    let source = Source::from(crate::time_format::TimeOutOfRange);
    assert!(source.as_error().is::<crate::time_format::TimeOutOfRange>());
    assert_eq!(
        crate::error::Report::chain(source.as_error()).to_string(),
        crate::time_format::TimeOutOfRange::MESSAGE
    );
}

fn permanently_failed() -> ResumeError {
    ResumeError::PermanentlyFailed {
        reason: crate::storage::import_journal::JournalFailure::ElementRejected,
    }
}

/// Public conversion preserves the journal refusal itself and unwraps real catalog failures.
#[test]
fn catalog_conversion_keeps_a_refusal_and_preserves_a_catalog_failure() {
    let refused = CatalogError::from(permanently_failed());
    assert_matches!(
        refused.source().unwrap().downcast_ref::<ResumeError>(),
        Some(ResumeError::PermanentlyFailed { .. })
    );
    let failed = CatalogError::from(ResumeError::Catalog(CatalogError::Unavailable(closed())));
    assert_eq!(failed, CatalogError::Unavailable(closed()));
    assert!(failed.source().unwrap().is::<BackendFailure>());
}

/// The typed chain from `first` visits exactly the errors `source()` returns, node for node.
fn assert_chain_matches(first: Link<'_>) {
    let typed: Vec<_> = chain(first).map(Link::as_error).collect();
    let plain: Vec<_> =
        std::iter::successors(Some(first.as_error()), |&error| error.source()).collect();
    let report = crate::error::Report::chain(first.as_error()).to_string();
    assert_eq!(typed.len(), plain.len(), "{report}");
    for (typed, plain) in typed.into_iter().zip(plain) {
        assert!(
            std::ptr::addr_eq(typed, plain),
            "{typed} is not {plain} in {report}"
        );
    }
}

fn admin_backend() -> AdminError {
    AdminError::Backend(closed())
}

#[test]
fn typed_chains_visit_every_source_node() {
    let io_cause = || crate::cause::IoCause::from(io::Error::other("disk failed"));
    let decoding = rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(io::Error::other("bad column")),
    );
    let sources: Vec<Source> = vec![
        BackendFailure::CatalogClosed.into(),
        MemoryExhausted {
            budget: MemoryBudget::Catalog,
            source: Some(sqlite_busy()),
        }
        .into(),
        BackendCause::from_error(closed()).into(),
        io::Error::other(io::Error::other("inner")).into(),
        decoding.into(),
        CatalogError::Io(io_cause()).into(),
        CatalogError::Corrupt(closed()).into(),
        CatalogError::Unavailable(closed()).into(),
        CatalogError::Metadata(crate::metadata_error::MetadataError::LongLanguage).into(),
        ObjectError::Io(io_cause()).into(),
        ObjectError::Metadata(crate::metadata_error::MetadataError::LongLanguage).into(),
        ObjectError::BodyAborted(closed()).into(),
        ObjectError::Corrupt(closed()).into(),
        ObjectError::Unavailable(closed()).into(),
        crate::storage::failure::object_context("staging cleanup", io::Error::other("disk")).into(),
        admin_backend().into(),
        AdminError::Corrupt(closed()).into(),
        AdminError::BodyAborted(closed()).into(),
        AdminError::Io(io_cause()).into(),
        AdminError::DiskFull(io::Error::from(io::ErrorKind::StorageFull).into()).into(),
        crate::fsutil::Unopenable.into(),
        ConnectionUnavailable::Closed.into(),
        ConnectionUnavailable::Unrecovered(RollbackError::Rollback(sqlite_busy())).into(),
        ExecutorError::Closed.into(),
        ResumeError::Catalog(CatalogError::Unavailable(closed())).into(),
        permanently_failed().into(),
        BackendCause::from_error(permanently_failed()).into(),
        crate::fsutil::FsError::Io(io::Error::other("below a leaf")).into(),
    ];
    for source in &sources {
        assert_chain_matches(source.link());
    }
}

/// Backend guidance is found through every typed layer that carries a fixed failure.
#[test]
fn fixed_failures_are_found_below_typed_layers() {
    let causes = [
        BackendCause::from_error(admin_backend()),
        BackendCause::from_error(ResumeError::Catalog(CatalogError::Unavailable(
            BackendCause::from_error(admin_backend()),
        ))),
        BackendCause::from_error(ConnectionUnavailable::Closed),
        BackendCause::from_error(ExecutorError::Closed),
        BackendCause::from_error(ObjectError::Unavailable(BackendCause::from_error(
            CatalogError::Unavailable(closed()),
        ))),
    ];
    for cause in causes {
        assert_eq!(
            cause.failure(),
            Some(BackendFailure::CatalogClosed),
            "{cause}"
        );
    }
    assert_eq!(BackendCause::from_error(sqlite_busy()).failure(), None);
}
