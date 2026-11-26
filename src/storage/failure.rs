//! Backend failure causes shared by the bucket and object error types.
//!
//! Every storage module maps SQLite, filesystem and task failures through this module so
//! that the corrupt / I/O / unavailable classification and the client-visible messages stay
//! identical across modules, and so that the original error remains available as `source()`.

use crate::bucket::CatalogError;
use crate::object::ObjectError;
use crate::storage::memory::BlobError;
use std::{cell::Cell, fmt, io, num::TryFromIntError, sync::PoisonError};

use crate::cause::{BackendCause, BackendFailure, IoCause, MemoryBudget, MemoryExhausted, Source};
#[cfg(test)]
use std::error::Error;
mod source;
pub(crate) mod wording;

impl From<BlobError> for IoCause {
    fn from(error: BlobError) -> Self {
        Self::with_source(error.kind(), error)
    }
}

/// Where the pages of the catalog database that a job on this thread uses live.
#[derive(Clone, Copy)]
enum CatalogPages {
    Disk,
    Memory,
}

thread_local! {
    /// SQLite reports a memory catalog's page budget and a full disk with the same code, and
    /// only the job running on the connection knows which database it uses.
    static CATALOG_PAGES: Cell<CatalogPages> = const { Cell::new(CatalogPages::Disk) };
}

/// Marks the calling thread as running a job on a memory catalog's connection until dropped, so
/// a full database there reports the `memory_catalog_max_bytes` budget instead of a full disk.
#[must_use = "dropping the guard ends the memory catalog job"]
pub(in crate::storage) struct MemoryCatalogJob {
    previous: CatalogPages,
}

impl MemoryCatalogJob {
    pub(in crate::storage) fn enter() -> Self {
        Self {
            previous: CATALOG_PAGES.replace(CatalogPages::Memory),
        }
    }
}

impl Drop for MemoryCatalogJob {
    fn drop(&mut self) {
        CATALOG_PAGES.set(self.previous);
    }
}

/// A full SQLite database: the disk, or on a memory catalog its page budget.
fn full_database(error: rusqlite::Error) -> IoCause {
    let source = match CATALOG_PAGES.get() {
        CatalogPages::Disk => Source::from(error),
        CatalogPages::Memory => Source::from(MemoryExhausted {
            budget: MemoryBudget::Catalog,
            source: Some(error),
        }),
    };
    IoCause::with_source(io::ErrorKind::StorageFull, source)
}

/// A backend failure sorted into the variants both error enums share. A clone shares the
/// underlying error, so every request that shared a failed transaction can report it.
#[derive(Clone)]
pub(crate) enum Classified {
    Corrupt(BackendCause),
    Io(IoCause),
    Unavailable(BackendCause),
}

/// A failure that storage code maps into `CatalogError` or `ObjectError`.
///
/// `classify` decides by the error's type and value: SQLite corruption and decoding failures
/// are corrupt, filesystem errors and a full SQLite disk are I/O, and everything else is
/// unavailable.
pub(crate) trait StorageCause: Sized {
    fn into_cause(self) -> BackendCause;

    fn classify(self) -> Classified {
        Classified::Unavailable(self.into_cause())
    }
}

impl StorageCause for rusqlite::Error {
    fn into_cause(self) -> BackendCause {
        BackendCause::from_error(self)
    }

    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "rusqlite::Error is non_exhaustive; every database failure not named here is unavailable"
    )]
    fn classify(self) -> Classified {
        use rusqlite::{Error, ErrorCode};
        match &self {
            Error::SqliteFailure(failure, _)
                if matches!(
                    failure.code,
                    ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
                ) =>
            {
                Classified::Corrupt(self.into_cause())
            }
            Error::FromSqlConversionFailure(..)
            | Error::IntegralValueOutOfRange(..)
            | Error::InvalidColumnType(..)
            | Error::Utf8Error(..) => Classified::Corrupt(self.into_cause()),
            Error::SqliteFailure(failure, _) if failure.code == ErrorCode::DiskFull => {
                Classified::Io(full_database(self))
            }
            _ => Classified::Unavailable(self.into_cause()),
        }
    }
}

impl StorageCause for io::Error {
    fn into_cause(self) -> BackendCause {
        BackendCause::from_error(self)
    }

    fn classify(self) -> Classified {
        Classified::Io(self.into())
    }
}

/// An exhausted RAM blob budget is I/O, like a full disk, and keeps the budget it exhausted.
impl StorageCause for BlobError {
    fn into_cause(self) -> BackendCause {
        BackendCause::from_error(self)
    }

    fn classify(self) -> Classified {
        Classified::Io(self.into())
    }
}

/// A storage failure keeps its own classification and cause: wrapping an unavailable failure
/// again would repeat its "catalog unavailable" prefix and hide the cause from log allowlists.
impl StorageCause for CatalogError {
    fn into_cause(self) -> BackendCause {
        BackendCause::from_error(self)
    }

    fn classify(self) -> Classified {
        match self {
            Self::Corrupt(cause) => Classified::Corrupt(cause),
            Self::Io(cause) => Classified::Io(cause),
            Self::Unavailable(cause) => Classified::Unavailable(cause),
            error @ (Self::AlreadyExists
            | Self::NotFound
            | Self::NotEmpty
            | Self::RestoreConflict
            | Self::PreconditionFailed
            | Self::NotModified
            | Self::PublicAccessPrevention
            | Self::Denied(_)
            | Self::Forbidden
            | Self::RetentionPolicyLocked(_)
            | Self::RetentionPolicyNotSet
            | Self::InvalidMetadata(_)
            | Self::Metadata(_)
            | Self::UnsupportedMetadata(_)
            | Self::NewerSchema { .. }) => Classified::Unavailable(BackendCause::from_error(error)),
        }
    }
}

/// Classified like [`CatalogError`], so both enums map each other's storage failures alike.
impl StorageCause for ObjectError {
    fn into_cause(self) -> BackendCause {
        BackendCause::from_error(self)
    }

    fn classify(self) -> Classified {
        match self {
            Self::Corrupt(cause) => Classified::Corrupt(cause),
            Self::Io(cause) => Classified::Io(cause),
            Self::Unavailable(cause) => Classified::Unavailable(cause),
            error @ (Self::BucketNotFound
            | Self::ObjectNotFound
            | Self::PreconditionFailed
            | Self::Component(_)
            | Self::PublicAccessPrevention
            | Self::OverwriteForbidden(_)
            | Self::ActiveHold(_)
            | Self::ObjectRetentionNotMet(_)
            | Self::UnlockedRetentionOverrideRequired
            | Self::LockedRetention
            | Self::ObjectNotSoftDeleted
            | Self::SoftDeletePolicyRequired(_)
            | Self::Denied(_)
            | Self::Forbidden
            | Self::NotModified
            | Self::WriteNotModified
            | Self::SourceNotModified
            | Self::ChecksumMismatch
            | Self::HashMismatch(_)
            | Self::TooLarge
            | Self::InvalidFixedMetadata(_)
            | Self::UniformAccessPredefinedAcl
            | Self::UniformAccessAclUpdate
            | Self::Metadata(_)
            | Self::InvalidDerivedArgument(_)
            | Self::ComposeClassMismatch { .. }
            | Self::DerivedTransferNotFound
            | Self::DerivedTransferExpired
            | Self::DerivedTransferBusy
            | Self::DerivedTransferIncomplete
            | Self::UnsupportedMetadata(_)
            | Self::CustomerEncryption(_)
            | Self::BodyAborted(_)) => Classified::Unavailable(BackendCause::from_error(error)),
        }
    }
}

impl StorageCause for &'static str {
    fn into_cause(self) -> BackendCause {
        BackendCause::new(self)
    }
}

impl StorageCause for BackendFailure {
    fn into_cause(self) -> BackendCause {
        self.into()
    }
}

impl StorageCause for String {
    fn into_cause(self) -> BackendCause {
        BackendCause::new(self)
    }
}

impl<T> StorageCause for PoisonError<T> {
    // The guard borrows the poisoned lock, so only the message can outlive it.
    fn into_cause(self) -> BackendCause {
        BackendCause::new(self.to_string())
    }
}

/// Error types with no corrupt or I/O meaning: always unavailable, source kept.
macro_rules! unavailable_cause {
    ($($error:ty),* $(,)?) => {
        $(impl StorageCause for $error {
            fn into_cause(self) -> BackendCause {
                BackendCause::from_error(self)
            }
        })*
    };
}

unavailable_cause!(
    TryFromIntError,
    std::array::TryFromSliceError,
    std::time::SystemTimeError,
    getrandom::Error,
    rusqlite::types::FromSqlError,
    serde_json::Error,
    tokio::sync::AcquireError,
    tokio::sync::TryLockError,
    tokio::sync::oneshot::error::RecvError,
    tokio::task::JoinError,
    crate::task::BlockingFailed,
    crate::fsutil::FsError,
    crate::storage::admin::AdminError,
    crate::storage::opaque_id::InvalidOpaqueId,
);

impl From<Classified> for CatalogError {
    fn from(failure: Classified) -> Self {
        match failure {
            Classified::Corrupt(cause) => Self::Corrupt(cause),
            Classified::Io(cause) => Self::Io(cause),
            Classified::Unavailable(cause) => Self::Unavailable(cause),
        }
    }
}

impl From<Classified> for ObjectError {
    fn from(failure: Classified) -> Self {
        match failure {
            Classified::Corrupt(cause) => Self::Corrupt(cause),
            Classified::Io(cause) => Self::Io(cause),
            Classified::Unavailable(cause) => Self::Unavailable(cause),
        }
    }
}

/// Maps a catalog failure to `CatalogError`, classifying corrupt and I/O causes.
pub(crate) fn unavailable(cause: impl StorageCause) -> CatalogError {
    cause.classify().into()
}

/// Maps backend failures to `ObjectError`; domain and authorization errors use `From`.
pub(crate) fn object_unavailable(cause: impl StorageCause) -> ObjectError {
    cause.classify().into()
}

/// Adds recovery context while preserving the failure's classification and original source.
pub(crate) fn object_context(context: impl fmt::Display, cause: impl StorageCause) -> ObjectError {
    with_context(context, cause).into()
}

/// Adds context to a catalog failure while preserving its classification and original source.
pub(crate) fn catalog_context(
    context: impl fmt::Display,
    cause: impl StorageCause,
) -> CatalogError {
    with_context(context, cause).into()
}

/// Classifies `cause` and puts `context` before its message, keeping the cause as the source.
fn with_context(context: impl fmt::Display, cause: impl StorageCause) -> Classified {
    match cause.classify() {
        Classified::Corrupt(cause) => Classified::Corrupt(BackendCause::with_source(
            format!("{context}: {cause}"),
            cause,
        )),
        Classified::Unavailable(cause) => Classified::Unavailable(BackendCause::with_source(
            format!("{context}: {cause}"),
            cause,
        )),
        Classified::Io(cause) => Classified::Io(cause.with_context(context)),
    }
}

macro_rules! classified_from {
    ($($error:ty),* $(,)?) => {
        $(
            impl From<$error> for CatalogError {
                fn from(error: $error) -> Self {
                    unavailable(error)
                }
            }

            impl From<$error> for ObjectError {
                fn from(error: $error) -> Self {
                    object_unavailable(error)
                }
            }
        )*
    };
}

classified_from!(
    rusqlite::Error,
    io::Error,
    BlobError,
    tokio::task::JoinError,
    crate::task::BlockingFailed
);

#[cfg(test)]
pub(crate) mod public_source_tests;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
