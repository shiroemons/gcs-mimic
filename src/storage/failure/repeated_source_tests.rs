//! Typed links retained until the Cause vocabulary slice can change their classification.

use super::{BackendCause, BackendFailure, IoCause, Link, TypedSource};
use crate::{
    bucket::CatalogError,
    object::ObjectError,
    seed::SeedError,
    storage::{
        catalog::{ConnectionUnavailable, ExecutorError, RollbackError},
        import_journal::{JournalFailure, ResumeError},
    },
};
use std::{error::Error, io};

type Node = dyn Error + 'static;

struct Exception {
    matches: fn(&Node, &Node) -> bool,
    example: fn() -> Box<Node>,
}

// Each row also constructs a real chain. The test below fails when a source or its wording
// changes so that the exception can be removed alongside the Cause vocabulary migration.
const CLASSIFIED: &[Exception] = &[
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
    // Catalog corruption and unavailability retain SQLite codes, backend guidance and resume refusals.
    Exception {
        matches: |outer, inner| matches!(outer.downcast_ref(), Some(CatalogError::Corrupt(cause) | CatalogError::Unavailable(cause)) if classified(cause.typed_source(), inner)),
        example: || {
            Box::new(CatalogError::from(ResumeError::PermanentlyFailed {
                reason: JournalFailure::ElementRejected,
            }))
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
    // A closed connection retains the backend failure used for retry advice.
    Exception {
        matches: |outer, inner| {
            matches!(outer.downcast_ref(), Some(ConnectionUnavailable::Closed))
                && inner.is::<BackendFailure>()
        },
        example: || Box::new(ConnectionUnavailable::Closed),
    },
    // The executor's closed queue returns the same classified retry advice.
    Exception {
        matches: |outer, inner| {
            matches!(outer.downcast_ref(), Some(ExecutorError::Closed))
                && inner.is::<BackendFailure>()
        },
        example: || Box::new(ExecutorError::Closed),
    },
    // An unrecovered connection's rollback cause leads sqlite_extended_code to the database error.
    Exception {
        matches: |outer, inner| {
            matches!(
                outer.downcast_ref(),
                Some(ConnectionUnavailable::Unrecovered(_))
            ) && inner.is::<RollbackError>()
        },
        example: || Box::new(ConnectionUnavailable::Unrecovered(RollbackError::StillOpen)),
    },
    // The database code on a failed rollback is used in backend failure events.
    Exception {
        matches: |outer, inner| {
            matches!(outer.downcast_ref(), Some(RollbackError::Rollback(_)))
                && inner.is::<rusqlite::Error>()
        },
        example: || {
            Box::new(RollbackError::Rollback(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                None,
            )))
        },
    },
    // Startup follows nested seed failures to give disk, memory and fixture-specific remedies.
    Exception {
        matches: |outer, inner| matches!(outer.downcast_ref(), Some(SeedError::Entry { source, .. } | SeedError::Import(source)) if inner.downcast_ref().is_some_and(|cause| std::ptr::eq(source.as_ref(), cause))),
        example: || Box::new(SeedError::Import(Box::new(SeedError::MissingRoot))),
    },
];

fn classified(link: Option<Link<'_>>, inner: &Node) -> bool {
    link.is_some_and(|link| match link {
        Link::Failure(_)
        | Link::Exhausted(_)
        | Link::Context(_)
        | Link::Io(_)
        | Link::Sqlite(_)
        | Link::Catalog(_)
        | Link::Object(_)
        | Link::Admin(_)
        | Link::Connection(_)
        | Link::Executor(_)
        | Link::Rollback(_)
        | Link::Resume(_)
        | Link::Unopenable(_) => std::ptr::addr_eq(link.as_error(), inner),
        Link::Leaf(_) => false,
    })
}

pub(crate) fn deferred(outer: &Node, inner: &Node) -> bool {
    // A transparent resume or I/O error can expose the catalog's existing classified link.
    if let Some(ResumeError::Catalog(catalog)) = outer.downcast_ref() {
        return deferred(catalog, inner);
    }
    if let Some(payload) = outer
        .downcast_ref::<io::Error>()
        .and_then(io::Error::get_ref)
    {
        return deferred(payload, inner);
    }
    CLASSIFIED.iter().any(|row| (row.matches)(outer, inner))
}

#[test]
fn deferred_classified_sources_still_repeat_their_text() {
    for row in CLASSIFIED {
        let outer = (row.example)();
        let inner = outer.source().expect("the classified link is retained");
        assert!((row.matches)(&*outer, inner), "{outer:?} -> {inner:?}");
        assert!(
            outer.to_string().ends_with(&inner.to_string()),
            "{outer} -> {inner}"
        );
    }
}
