//! The errors a backend failure keeps as its `source()`, by type.
//!
//! A failure type that a storage, synchronization or adapter decision reads (backend guidance,
//! a memory budget, a retry or pause decision) has its own variant. Every other failure is a
//! leaf, and the one list of leaves below is the only way to build one, so a new error type
//! cannot travel inside a cause without the decision whether anything reads it.

use super::{BackendCause, BackendFailure, MemoryBudget, MemoryExhausted};
use crate::{
    bucket::CatalogError,
    object::ObjectError,
    source_tree::watch,
    storage::{
        admin::AdminError,
        catalog::{ConnectionUnavailable, ExecutorError, RollbackError},
        import_journal::ResumeError,
        memory::BlobError,
        sync_state::{conflicts, root::RootError, writeback},
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
    Root(RootError),
    Writeback(writeback::Error),
    Conflict(conflicts::Error),
    Connection(ConnectionUnavailable),
    Executor(ExecutorError),
    /// A journal refusal that an import resume names to the operator.
    Resume(ResumeError),
    /// A failure no storage, sync or adapter decision reads.
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
            Self::Root(error) => Link::Root(error),
            Self::Writeback(error) => Link::Writeback(error),
            Self::Conflict(error) => Link::Conflict(error),
            Self::Connection(error) => Link::Connection(error),
            Self::Executor(error) => Link::Executor(error),
            Self::Resume(error) => Link::Resume(error),
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
    Root(&'a RootError),
    Writeback(&'a writeback::Error),
    Conflict(&'a conflicts::Error),
    /// The write-back stop that recovery left in place; only a synchronization failure holds it.
    Stop(&'a writeback::Stop),
    Connection(&'a ConnectionUnavailable),
    Executor(&'a ExecutorError),
    Rollback(&'a RollbackError),
    Resume(&'a ResumeError),
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
            Self::Root(error) => error,
            Self::Writeback(error) => error,
            Self::Conflict(error) => error,
            Self::Stop(stop) => stop,
            Self::Connection(error) => error,
            Self::Executor(error) => error,
            Self::Rollback(error) => error,
            Self::Resume(error) => error,
            Self::Leaf(error) => error,
        }
    }

    /// The next link: the error `as_error().source()` returns, by type.
    pub(crate) fn source(self) -> Option<Link<'a>> {
        match self {
            Self::Failure(_) | Self::Stop(_) => None,
            Self::Exhausted(error) => error.typed_source(),
            Self::Context(error) => error.typed_source(),
            Self::Catalog(error) => error.typed_source(),
            Self::Object(error) => error.typed_source(),
            Self::Admin(error) => error.typed_source(),
            Self::Root(error) => error.typed_source(),
            Self::Writeback(error) => error.typed_source(),
            Self::Conflict(error) => error.typed_source(),
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
    Root(RootError),
    Writeback(writeback::Error),
    Conflict(conflicts::Error),
    Connection(ConnectionUnavailable),
    Executor(ExecutorError),
    Resume(ResumeError),
);

/// A RAM blob failure is the budget it exhausted or the I/O error it met.
impl From<BlobError> for Source {
    fn from(error: BlobError) -> Self {
        match error {
            BlobError::Exhausted => Self::Exhausted(MemoryExhausted::new(MemoryBudget::Blobs)),
            BlobError::Io(error) => Self::Io(error),
        }
    }
}

/// A native watch that could not be built keeps the I/O error or the tree listing failure it met,
/// so the mirror's retry decision still reads them; a watch limit is a leaf.
impl From<watch::Error> for Source {
    fn from(error: watch::Error) -> Self {
        match error {
            watch::Error::Io(error) => Self::Io(error),
            watch::Error::Tree(error) => Self::Admin(error),
            watch::Error::Limit(limit) => limit.into(),
        }
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
    crate::json::JsonError,
    crate::notification::NotificationDecodeError,
    crate::source_tree::watch::WatchLimit,
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
