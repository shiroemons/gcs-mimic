//! The errors a backend failure keeps as its `source()`, by type.
//!
//! A failure type that a storage or adapter decision reads (backend guidance, a memory budget, a
//! retry or pause decision) has its own variant. Every other failure is a leaf, and the one list
//! of leaves below is the only way to build one, so a new error type cannot travel inside a cause
//! without the decision whether anything reads it.

use super::{BackendCause, BackendFailure, MemoryBudget, MemoryExhausted};
use crate::{
    bucket::CatalogError,
    fsutil::Unopenable,
    object::ObjectError,
    storage::{
        admin::AdminError,
        catalog::{ConnectionUnavailable, ExecutorError, RollbackError},
        import_journal::ResumeError,
        memory::BlobError,
    },
};
use std::{error::Error, fmt, io};

/// The error a backend cause keeps as its `source()`, by type.
pub(crate) enum Source {
    Failure(BackendFailure),
    Exhausted(MemoryExhausted),
    Context(BackendCause),
    Io(io::Error),
    Sqlite(rusqlite::Error),
    Catalog(CatalogError),
    Object(ObjectError),
    Admin(AdminError),
    Connection(ConnectionUnavailable),
    Executor(ExecutorError),
    /// A journal refusal that an import resume names to the operator.
    Resume(ResumeError),
    /// An entry that kept failing to open while its name still held one, which the retry
    /// decisions take for a failure that passes.
    Unopenable(Unopenable),
    /// A failure no storage or adapter decision reads.
    Leaf(Leaf),
}

/// A failure no decision reads, kept so reports and logs still show it.
pub(crate) struct Leaf(Box<dyn Error + Send + Sync>);

impl Source {
    /// The held error as the first link of its chain.
    pub(crate) fn link(&self) -> Link<'_> {
        match self {
            Self::Failure(error) => Link::Failure(error),
            Self::Exhausted(error) => Link::Exhausted(error),
            Self::Context(error) => Link::Context(error),
            Self::Io(error) => Link::Io(error),
            Self::Sqlite(error) => Link::Sqlite(error),
            Self::Catalog(error) => Link::Catalog(error),
            Self::Object(error) => Link::Object(error),
            Self::Admin(error) => Link::Admin(error),
            Self::Connection(error) => Link::Connection(error),
            Self::Executor(error) => Link::Executor(error),
            Self::Resume(error) => Link::Resume(error),
            Self::Unopenable(error) => Link::Unopenable(error),
            Self::Leaf(Leaf(error)) => Link::Leaf(&**error),
        }
    }

    /// The held error itself, so a `source()` chain sees its own type and text.
    pub(crate) fn as_error(&self) -> &(dyn Error + 'static) {
        self.link().as_error()
    }
}

/// One error of a failure's `source()` chain, by type.
#[derive(Clone, Copy)]
pub(crate) enum Link<'a> {
    Failure(&'a BackendFailure),
    Exhausted(&'a MemoryExhausted),
    Context(&'a BackendCause),
    Io(&'a io::Error),
    Sqlite(&'a rusqlite::Error),
    Catalog(&'a CatalogError),
    Object(&'a ObjectError),
    Admin(&'a AdminError),
    Connection(&'a ConnectionUnavailable),
    Executor(&'a ExecutorError),
    Rollback(&'a RollbackError),
    Resume(&'a ResumeError),
    Unopenable(&'a Unopenable),
    /// A failure no decision reads, or any error below one, an I/O error or an SQLite error.
    Leaf(&'a (dyn Error + 'static)),
}

impl<'a> Link<'a> {
    pub(crate) fn as_error(self) -> &'a (dyn Error + 'static) {
        match self {
            Self::Failure(error) => error,
            Self::Exhausted(error) => error,
            Self::Context(error) => error,
            Self::Io(error) => error,
            Self::Sqlite(error) => error,
            Self::Catalog(error) => error,
            Self::Object(error) => error,
            Self::Admin(error) => error,
            Self::Connection(error) => error,
            Self::Executor(error) => error,
            Self::Rollback(error) => error,
            Self::Resume(error) => error,
            Self::Unopenable(error) => error,
            Self::Leaf(error) => error,
        }
    }

    /// The next link: the error `as_error().source()` returns, by type.
    pub(crate) fn source(self) -> Option<Link<'a>> {
        match self {
            Self::Failure(_) | Self::Unopenable(_) => None,
            Self::Exhausted(error) => error.typed_source(),
            Self::Context(error) => error.typed_source(),
            Self::Catalog(error) => error.typed_source(),
            Self::Object(error) => error.typed_source(),
            Self::Admin(error) => error.typed_source(),
            Self::Connection(error) => error.typed_source(),
            Self::Executor(error) => error.typed_source(),
            Self::Rollback(error) => error.typed_source(),
            Self::Resume(error) => error.typed_source(),
            // Neither standard error nor a leaf holds a typed failure.
            Self::Io(error) => Error::source(error).map(Self::Leaf),
            Self::Sqlite(error) => Error::source(error).map(Self::Leaf),
            Self::Leaf(error) => error.source().map(Self::Leaf),
        }
    }

    /// The fixed backend failure this link is, if it is one.
    pub(crate) fn failure(self) -> Option<BackendFailure> {
        if let Self::Failure(failure) = self {
            Some(*failure)
        } else {
            None
        }
    }

    /// SQLite's extended result code, if this link is an SQLite failure.
    pub(crate) fn sqlite_extended_code(self) -> Option<std::ffi::c_int> {
        if let Self::Sqlite(rusqlite::Error::SqliteFailure(failure, _)) = self {
            Some(failure.extended_code)
        } else {
            None
        }
    }

    /// The `backend=memory` budget this link reports exhausted, if it is one.
    pub(crate) fn memory_budget(self) -> Option<MemoryBudget> {
        if let Self::Exhausted(exhausted) = self {
            Some(exhausted.budget)
        } else {
            None
        }
    }

    /// The import journal refusal this link is, if it is one.
    pub(crate) fn resume_refusal(self) -> Option<&'a ResumeError> {
        if let Self::Resume(refusal) = self {
            Some(refusal)
        } else {
            None
        }
    }
}

/// An error whose `source()` is derived from this typed link, so decisions that read the chain
/// by type and reports that read it through `source()` see the same errors.
pub(crate) trait TypedSource {
    fn typed_source(&self) -> Option<Link<'_>>;
}

/// `first` and every link below it, outermost first.
pub(crate) fn chain(first: Link<'_>) -> impl Iterator<Item = Link<'_>> {
    std::iter::successors(Some(first), |link| link.source())
}

impl fmt::Display for Source {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_error(), formatter)
    }
}

impl fmt::Debug for Source {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_error(), formatter)
    }
}

macro_rules! typed_sources {
    ($($variant:ident($error:ty)),* $(,)?) => {
        $(impl From<$error> for Source {
            fn from(error: $error) -> Self {
                Self::$variant(error)
            }
        })*
    };
}

typed_sources!(
    Failure(BackendFailure),
    Exhausted(MemoryExhausted),
    Context(BackendCause),
    Io(io::Error),
    Sqlite(rusqlite::Error),
    Catalog(CatalogError),
    Object(ObjectError),
    Admin(AdminError),
    Connection(ConnectionUnavailable),
    Executor(ExecutorError),
    Resume(ResumeError),
    Unopenable(Unopenable),
);

/// A RAM blob failure is the budget it exhausted or the I/O error it met.
impl From<BlobError> for Source {
    fn from(error: BlobError) -> Self {
        match error {
            BlobError::Exhausted => Self::Exhausted(MemoryExhausted::new(MemoryBudget::Blobs)),
            BlobError::Io(error) => Self::Io(error),
            BlobError::Size(error) => error.into(),
            BlobError::Allocation(error) => Self::Leaf(Leaf(Box::new(error))),
            other @ (BlobError::Missing(_) | BlobError::Poisoned(_) | BlobError::Closed) => {
                Self::Leaf(Leaf(Box::new(other)))
            }
        }
    }
}

/// Backend diagnostics for a stored resource retain the original status prefix.
#[derive(Debug, thiserror::Error)]
#[error("503 Service Unavailable: {0}")]
struct StoredTimestampError(crate::time_format::TimeOutOfRange);

impl From<crate::time_format::TimeOutOfRange> for Source {
    fn from(error: crate::time_format::TimeOutOfRange) -> Self {
        Self::Leaf(Leaf(Box::new(StoredTimestampError(error))))
    }
}

macro_rules! leaf_sources {
    ($($error:ty),* $(,)?) => {
        $(impl From<$error> for Source {
            fn from(error: $error) -> Self {
                Self::Leaf(Leaf(Box::new(error)))
            }
        })*
    };
}

leaf_sources!(
    std::num::TryFromIntError,
    std::array::TryFromSliceError,
    std::time::SystemTimeError,
    std::sync::PoisonError<()>,
    std::sync::mpsc::TrySendError<()>,
    getrandom::Error,
    rusqlite::types::FromSqlError,
    serde_json::Error,
    tokio::sync::AcquireError,
    tokio::sync::TryLockError,
    tokio::sync::oneshot::error::RecvError,
    tokio::task::JoinError,
    crate::task::BlockingFailed,
    crate::task::JobPanicked,
    crate::fsutil::FsError,
    crate::acl::AclError,
    crate::auth::InvalidUploadSnapshot,
    crate::auth::PolicyDecodeError,
    crate::auth::signature::VerificationError,
    crate::import::codec::CodecError,
    crate::import::codec::TimestampError,
    crate::notification::NotificationDecodeError,
    crate::object::HashMismatch,
    crate::storage::opaque_id::InvalidOpaqueId,
    crate::storage::admin::NestedFileJob,
    crate::storage::catalog::resumable::InvalidOptions,
    crate::storage::xml_multipart::Error,
);

// A copy checkpoint that tests arm and then abandon.
#[cfg(test)]
leaf_sources!(crate::server::copy::CheckpointError);

#[cfg(test)]
mod tests;
