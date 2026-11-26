//! The errors a backend failure keeps as its `source()`, by type.
//!
//! A failure type that a storage or adapter decision reads (backend guidance, a memory budget, a
//! retry or pause decision) has its own variant. The leaf list names errors no decision reads.
//! RAM blob errors are matched separately because their budget and I/O failures are classified.

use super::{BackendCause, BackendFailure, MemoryBudget, MemoryExhausted};
use crate::{
    bucket::CatalogError,
    fsutil::Unopenable,
    object::ObjectError,
    storage::{
        admin::AdminError, catalog::RollbackError, import_journal::ResumeError, memory::BlobError,
    },
};
use std::{error::Error, fmt, io};

/// Owning a layer preserves the concrete error returned by `source()`.
pub(crate) struct Source(Box<dyn Layer>);

pub(crate) trait Layer: Error + Send + Sync + 'static {
    fn link(&self) -> Link<'_>;

    fn failure(&self) -> Option<BackendFailure> {
        None
    }

    fn next(&self) -> Option<Link<'_>> {
        self.source().map(Link::Leaf)
    }
}

impl<T: Layer> From<T> for Source {
    fn from(error: T) -> Self {
        Self(Box::new(error))
    }
}

impl Source {
    pub(crate) fn link(&self) -> Link<'_> {
        self.0.link()
    }

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
    Typed(&'a dyn Layer),
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
            Self::Typed(error) => error,
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
            Self::Typed(error) => error.next(),
            Self::Resume(error) => error.typed_source(),
            // Neither standard error nor a leaf holds a typed failure.
            Self::Io(error) => Error::source(error).map(Self::Leaf),
            Self::Sqlite(error) => Error::source(error).map(Self::Leaf),
            Self::Leaf(error) => error.source().map(Self::Leaf),
        }
    }

    /// The fixed backend failure this link is, if it is one.
    pub(crate) fn failure(self) -> Option<BackendFailure> {
        match self {
            Self::Failure(failure) => Some(*failure),
            Self::Typed(layer) => layer.failure(),
            Self::Exhausted(_)
            | Self::Context(_)
            | Self::Io(_)
            | Self::Sqlite(_)
            | Self::Resume(_)
            | Self::Unopenable(_)
            | Self::Leaf(_) => None,
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
        $(impl Layer for $error {
            fn link(&self) -> Link<'_> {
                Link::$variant(self)
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
    Resume(ResumeError),
    Unopenable(Unopenable),
);

macro_rules! classified_layers {
    ($($error:ty),* $(,)?) => {
        $(impl Layer for $error {
            fn link(&self) -> Link<'_> {
                Link::Typed(self)
            }

            fn next(&self) -> Option<Link<'_>> {
                self.typed_source()
            }
        })*
    };
}

classified_layers!(CatalogError, ObjectError, AdminError, RollbackError);

/// A full RAM store exposes the budget; its other failures expose the original error.
impl Layer for BlobError {
    fn link(&self) -> Link<'_> {
        static BLOBS: MemoryExhausted = MemoryExhausted::new(MemoryBudget::Blobs);
        match self {
            Self::Exhausted => Link::Exhausted(&BLOBS),
            Self::Io(error) => Link::Io(error),
            Self::Size(error) => Link::Leaf(error),
            Self::Allocation(error) => Link::Leaf(error),
            Self::ReadWorker(error) => Link::Leaf(error),
            #[cfg(unix)]
            Self::ShardParentMissing => Link::Leaf(self),
            Self::Missing(_)
            | Self::Poisoned(_)
            | Self::Closed
            | Self::SharedFile
            | Self::ShardNotDirectory => Link::Leaf(self),
        }
    }
}

macro_rules! leaf_sources {
    ($($error:ty),* $(,)?) => {
        $(impl Layer for $error {
            fn link(&self) -> Link<'_> {
                Link::Leaf(self)
            }
        })*
    };
}

leaf_sources!(
    crate::time_format::TimeOutOfRange,
    std::collections::TryReserveError,
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
