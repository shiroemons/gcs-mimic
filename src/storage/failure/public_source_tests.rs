//! Display/source pairs retained for public callers or existing storage diagnostics.

use crate::cause::{BackendCause, BackendFailure, IoCause, Link, Source, TypedSource};
use crate::{
    bucket::CatalogError,
    import::ImportError,
    object::ObjectError,
    seed::SeedError,
    storage::{
        ResumableError,
        catalog::{ConnectionUnavailable, ExecutorError, RollbackError},
        hmac_keys::HmacKeyError,
        import_journal::ResumeError,
        xml_multipart,
    },
};
use std::{error::Error, io};

type Node = dyn Error + 'static;

struct Exception {
    matches: fn(&Node, &Node) -> bool,
    example: fn() -> Box<Node>,
}

// Embedders can log these errors directly and inspect their concrete cause. Each exception
// expires when Public facade settles that contract; a stale example fails immediately.
const PUBLIC_PAIRS: &[Exception] = &[
    // BackendCause::failure and sqlite_extended_code inspect the chain held by this context.
    Exception {
        matches: |outer, inner| {
            outer
                .downcast_ref::<BackendCause>()
                .is_some_and(|cause| classified(cause.typed_source(), inner))
        },
        example: || Box::new(BackendCause::from_error(BackendFailure::CatalogClosed)),
    },
    // Recovery preserves the original I/O kind and IoCause::memory_budget through its context.
    Exception {
        matches: |outer, inner| {
            outer
                .downcast_ref::<IoCause>()
                .is_some_and(|cause| classified(cause.typed_source(), inner))
        },
        example: || Box::new(IoCause::from(io::Error::from(io::ErrorKind::StorageFull))),
    },
    // Catalog corruption and unavailability expose the same concrete SQLite or backend cause.
    Exception {
        matches: |outer, inner| matches!(outer.downcast_ref(), Some(CatalogError::Corrupt(cause) | CatalogError::Unavailable(cause)) if classified(cause.typed_source(), inner)),
        example: || {
            Box::new(CatalogError::Corrupt(BackendCause::from_error(
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                    None,
                ),
            )))
        },
    },
    // The catalog's I/O classification retains the error behind the kind or recovery context.
    Exception {
        matches: |outer, inner| matches!(outer.downcast_ref(), Some(CatalogError::Io(cause)) if classified(cause.typed_source(), inner)),
        example: || {
            Box::new(CatalogError::from(io::Error::from(
                io::ErrorKind::StorageFull,
            )))
        },
    },
    // Startup follows nested seed failures to give disk, memory and fixture-specific remedies.
    Exception {
        matches: |outer, inner| matches!(outer.downcast_ref(), Some(SeedError::Entry { source, .. } | SeedError::Import(source)) if inner.downcast_ref().is_some_and(|cause| std::ptr::eq(source.as_ref(), cause))),
        example: || Box::new(SeedError::Import(Box::new(SeedError::MissingRoot))),
    },
];

// Multipart cleanup logs ObjectError directly and public catalog diagnostics embed its Display.
// Keep the concrete storage causes until Public facade revisits those diagnostics.
const OBJECT_PAIRS: &[Exception] = &[
    // Object errors carry the same fixed backend guidance and database codes through object_context.
    Exception {
        matches: |outer, inner| matches!(outer.downcast_ref(), Some(ObjectError::Corrupt(cause) | ObjectError::Unavailable(cause) | ObjectError::BodyAborted(cause)) if classified(cause.typed_source(), inner)),
        example: || {
            Box::new(ObjectError::Unavailable(
                BackendFailure::CatalogClosed.into(),
            ))
        },
    },
    // Recovery also preserves the object I/O kind and the underlying budget.
    Exception {
        matches: |outer, inner| matches!(outer.downcast_ref(), Some(ObjectError::Io(cause)) if classified(cause.typed_source(), inner)),
        example: || {
            Box::new(super::object_context(
                "blob",
                io::Error::from(io::ErrorKind::StorageFull),
            ))
        },
    },
];

fn classified(link: Option<Link<'_>>, inner: &Node) -> bool {
    link.is_some_and(|link| match link {
        Link::Failure(_)
        | Link::Exhausted(_)
        | Link::Context(_)
        | Link::Io(_)
        | Link::Sqlite(_)
        | Link::Unopenable(_) => std::ptr::addr_eq(link.as_error(), inner),
        Link::Typed(_) => {
            let error = link.as_error();
            (error.is::<CatalogError>()
                || error.is::<ObjectError>()
                || error.is::<ResumeError>()
                || error.is::<crate::storage::admin::AdminError>()
                || error.is::<ConnectionUnavailable>()
                || error.is::<ExecutorError>()
                || error.is::<RollbackError>())
                && std::ptr::addr_eq(error, inner)
        }
        Link::Leaf(_) => false,
    })
}

fn transparent_payload(outer: &Node) -> Option<&Node> {
    if let Some(ResumeError::Catalog(catalog)) = outer.downcast_ref() {
        return Some(catalog);
    }
    if let Some(HmacKeyError::Catalog(catalog)) = outer.downcast_ref() {
        return Some(catalog);
    }
    if let Some(ResumableError::Object(object)) = outer.downcast_ref() {
        return Some(object);
    }
    if let Some(xml_multipart::Error::Object(object)) = outer.downcast_ref() {
        return Some(object);
    }
    if let Some(ImportError::Catalog(catalog)) = outer.downcast_ref() {
        return Some(catalog);
    }
    if let Some(ImportError::Resume(resume)) = outer.downcast_ref() {
        return Some(resume);
    }
    outer
        .downcast_ref::<io::Error>()
        .and_then(io::Error::get_ref)
        .map(|payload| payload as &Node)
}

fn inherits_exception(outer: &Node, payload: &Node, inner: &Node) -> bool {
    outer.to_string() == payload.to_string()
        && payload
            .source()
            .is_some_and(|source| std::ptr::addr_eq(source, inner))
        && compatible_pair(payload, inner)
}

pub(crate) fn compatible_pair(outer: &Node, inner: &Node) -> bool {
    if let Some(payload) = transparent_payload(outer) {
        return inherits_exception(outer, payload, inner);
    }
    PUBLIC_PAIRS
        .iter()
        .chain(OBJECT_PAIRS)
        .any(|row| (row.matches)(outer, inner))
}

#[test]
fn closed_connection_and_executor_keep_their_guidance_without_a_synthetic_source() {
    for source in [
        Source::from(ConnectionUnavailable::Closed),
        ExecutorError::Closed.into(),
    ] {
        let error = source.as_error();
        assert!(error.source().is_none());
        assert_eq!(
            crate::error::Report::chain(error).to_string(),
            "catalog is closed"
        );
        assert_eq!(source.link().failure(), Some(BackendFailure::CatalogClosed));
    }
}

#[test]
fn rollback_context_preserves_every_cause_without_repeating_its_message() {
    let sqlite = rusqlite::Error::InvalidQuery;
    let message = sqlite.to_string();
    let error = ConnectionUnavailable::Unrecovered(RollbackError::Rollback(sqlite));
    let rollback = error
        .source()
        .unwrap()
        .downcast_ref::<RollbackError>()
        .unwrap();
    assert!(rollback.source().unwrap().is::<rusqlite::Error>());
    let expected =
        format!("catalog connection is unusable: transaction could not be rolled back: {message}");
    assert_eq!(crate::error::Report::chain(&error).to_string(), expected);
    let source = Source::from(error);
    assert_eq!(source.to_string(), expected);
    assert_eq!(BackendCause::from_error(source).to_string(), expected);
}

#[test]
fn a_failed_seed_pass_preserves_the_rollback_diagnostic() {
    let sqlite = rusqlite::Error::InvalidParameterName("key".into());
    let sqlite_text = sqlite.to_string();
    let rollback = format!("transaction could not be rolled back: {sqlite_text}");
    let unusable = format!("catalog connection is unusable: {rollback}");
    let error = ConnectionUnavailable::Unrecovered(RollbackError::Rollback(sqlite));
    let seed = SeedError::Admin(error.into());
    assert_eq!(
        seed.failed_pass_diagnostic(),
        format!(
            "administrative operation: Backend({unusable:?}): {unusable}: {rollback}: {sqlite_text}"
        )
    );
}

fn disk_full() -> CatalogError {
    io::Error::from(io::ErrorKind::StorageFull).into()
}

fn assert_reports_payload<E: Error + 'static, W: Error + 'static>(
    payload: E,
    wrap: impl FnOnce(E) -> W,
) {
    let expected = crate::error::Report::chain(&payload).to_string();
    assert_eq!(
        crate::error::Report::chain(&wrap(payload)).to_string(),
        expected
    );
}

#[test]
fn hmac_reports_a_classified_catalog_cause() {
    assert_reports_payload(disk_full(), HmacKeyError::Catalog);
}

#[test]
fn resumable_reports_a_classified_object_cause() {
    assert_reports_payload(disk_full().into(), ResumableError::Object);
}

#[test]
fn xml_multipart_reports_a_classified_object_cause() {
    assert_reports_payload(disk_full().into(), xml_multipart::Error::Object);
}

#[test]
fn import_reports_a_classified_catalog_cause() {
    assert_reports_payload(disk_full(), ImportError::Catalog);
}

#[test]
fn import_reports_a_classified_resume_cause() {
    assert_reports_payload(ResumeError::Catalog(disk_full()), ImportError::Resume);
}

#[test]
fn a_changed_message_does_not_inherit_the_payload_exception() {
    let payload = disk_full();
    let source = payload.source().unwrap();
    assert!(compatible_pair(&payload, source));
    assert!(!inherits_exception(
        &io::Error::other("different text"),
        &payload,
        source
    ));
}

#[test]
fn a_different_source_does_not_inherit_the_payload_exception() {
    let outer = ImportError::Catalog(disk_full());
    let other = disk_full();
    assert_eq!(outer.to_string(), other.to_string());
    assert!(compatible_pair(&outer, outer.source().unwrap()));
    assert!(!compatible_pair(&outer, other.source().unwrap()));
}

#[test]
fn classified_exception_examples_keep_the_concrete_layer_and_its_text() {
    for layer in [
        Source::from(CatalogError::PreconditionFailed),
        ObjectError::BucketNotFound.into(),
        crate::storage::admin::AdminError::Closed.into(),
        ResumeError::PermanentlyFailed {
            reason: crate::storage::import_journal::JournalFailure::ElementRejected,
        }
        .into(),
        ConnectionUnavailable::Panicked.into(),
        ExecutorError::Closed.into(),
        RollbackError::StillOpen.into(),
    ] {
        let cause = BackendCause::from_error(layer);
        let inner = cause.source().unwrap();
        assert_eq!(cause.to_string(), inner.to_string());
        assert!(compatible_pair(&cause, inner), "{inner:?}");
    }
}

#[test]
fn public_catalog_report_retains_its_historical_sqlite_suffix() {
    let sqlite = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        Some("malformed stored row".into()),
    );
    let ffi = sqlite.source().unwrap().to_string();
    let error = CatalogError::from(sqlite);
    assert_eq!(error.to_string(), "catalog corrupt: malformed stored row");
    assert!(error.source().unwrap().is::<rusqlite::Error>());
    assert_eq!(
        crate::error::Report::chain(&error).to_string(),
        format!("catalog corrupt: malformed stored row: malformed stored row: {ffi}")
    );
}

#[test]
fn documented_source_exceptions_still_match_the_held_error_and_its_text() {
    for row in PUBLIC_PAIRS.iter().chain(OBJECT_PAIRS) {
        let outer = (row.example)();
        let inner = outer.source().expect("the classified link is retained");
        assert!((row.matches)(&*outer, inner), "{outer:?} -> {inner:?}");
        assert!(
            outer.to_string().ends_with(&inner.to_string()),
            "{outer} -> {inner}"
        );
    }
}
