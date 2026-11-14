//! The errors a backend failure keeps as its `source()`, by type.
//!
//! A failure type that a storage, synchronization or adapter decision reads (backend guidance,
//! a memory budget, a retry or pause decision) has its own variant. Every other failure is a
//! leaf, and the one list of leaves below is the only way to build one, so a new error type
//! cannot travel inside a cause without the decision whether anything reads it.

use super::{BackendCause, BackendFailure, MemoryExhausted};
use crate::{
    bucket::CatalogError,
    object::ObjectError,
    storage::{
        admin::AdminError,
        catalog::{ConnectionUnavailable, ExecutorError},
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
    /// A failure no storage, sync or adapter decision reads.
    Leaf(Leaf),
}

/// A failure no decision reads, kept so reports and logs still show it.
pub(crate) struct Leaf(Box<dyn Error + Send + Sync>);

impl Source {
    /// The held error itself, so a `source()` chain sees its own type and text.
    pub(crate) fn as_error(&self) -> &(dyn Error + 'static) {
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
            Self::Connection(error) => error,
            Self::Executor(error) => error,
            Self::Leaf(Leaf(error)) => &**error,
        }
    }
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
);

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
    crate::storage::opaque_id::InvalidOpaqueId,
    crate::storage::admin::NestedFileJob,
    crate::storage::catalog::resumable::InvalidOptions,
    crate::storage::import_journal::ResumeError,
    crate::storage::xml_multipart::Error,
);

// A copy checkpoint that tests arm and then abandon.
#[cfg(test)]
leaf_sources!(crate::server::copy::CheckpointError);

#[cfg(test)]
mod tests;
