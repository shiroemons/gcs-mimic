//! Backend failure causes shared by the bucket and object error types.
//!
//! Every storage module maps SQLite, filesystem and task failures through this module so
//! that the corrupt / I/O / unavailable classification and the client-visible messages stay
//! identical across modules, and so that the original error remains available as `source()`.

use crate::bucket::CatalogError;
use crate::object::ObjectError;
use std::{
    borrow::Cow,
    cell::Cell,
    error::Error,
    fmt, io,
    num::TryFromIntError,
    sync::{Arc, PoisonError},
};

type Source = Arc<dyn Error + Send + Sync + 'static>;

mod kind;
pub(crate) use kind::BackendFailure;

/// The message and optional underlying error of an unavailable or corrupt backend.
///
/// `Display` prints only the message. A cause built from an error uses that error's own
/// `Display` text as the message, so wrapped errors render exactly as the stringified
/// errors did before sources were kept. Equality compares messages, which is what callers
/// and tests observe.
#[derive(Clone)]
pub struct BackendCause {
    message: Cow<'static, str>,
    source: Option<Source>,
}

impl BackendCause {
    /// A cause with a fixed or formatted message and no underlying error.
    pub(crate) fn new(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// A cause whose message is the error's `Display` text.
    pub(crate) fn from_error(error: impl Error + Send + Sync + 'static) -> Self {
        Self {
            message: error.to_string().into(),
            source: Some(Arc::new(error)),
        }
    }

    /// A cause with a contextual message that already describes `error`.
    pub(crate) fn with_source(
        message: impl Into<Cow<'static, str>>,
        error: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            message: message.into(),
            source: Some(Arc::new(error)),
        }
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn failure(&self) -> Option<BackendFailure> {
        crate::error::causes(self).find_map(|error| error.downcast_ref::<BackendFailure>().copied())
    }

    pub(crate) fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|error| error as &(dyn Error + 'static))
    }
}

impl fmt::Display for BackendCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// Lets a contextual message travel inside another error, such as `io::Error::other`, with
/// the underlying error still reachable through `source()`.
impl Error for BackendCause {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        BackendCause::source(self)
    }
}

/// Formats like the bare message so `Debug` output of the error enums is unchanged.
impl fmt::Debug for BackendCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Sync diagnostics expose this text, so preserve the original message format.
        fmt::Debug::fmt(&*self.message, formatter)
    }
}

impl PartialEq for BackendCause {
    fn eq(&self, other: &Self) -> bool {
        self.message == other.message
    }
}

impl Eq for BackendCause {}

impl PartialEq<&str> for BackendCause {
    fn eq(&self, other: &&str) -> bool {
        self.message == *other
    }
}

impl From<&str> for BackendCause {
    fn from(message: &str) -> Self {
        Self::new(message.to_owned())
    }
}

impl From<String> for BackendCause {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<BackendFailure> for BackendCause {
    fn from(failure: BackendFailure) -> Self {
        Self::from_error(failure)
    }
}

/// An I/O failure: the kind callers classify by, and the error that produced it.
#[derive(Clone)]
pub struct IoCause {
    kind: io::ErrorKind,
    source: Option<Source>,
}

impl IoCause {
    pub(crate) fn kind(&self) -> io::ErrorKind {
        self.kind
    }

    pub(crate) fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|error| error as &(dyn Error + 'static))
    }

    /// The `backend=memory` budget this failure exhausted, if it is one. A full disk carries none.
    pub(crate) fn memory_budget(&self) -> Option<MemoryBudget> {
        crate::error::causes(self.source()?).find_map(|cause| {
            cause
                .downcast_ref::<MemoryExhausted>()
                .map(|exhausted| exhausted.budget)
        })
    }
}

/// A `backend=memory` budget, each with its own remedy when it runs out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemoryBudget {
    /// Object bytes, bounded by `memory_max_bytes`.
    Blobs,
    /// Catalog database pages, bounded by `memory_catalog_max_bytes`.
    Catalog,
}

/// A `backend=memory` budget that ran out. It travels inside the `StorageFull` failure, so a reply
/// can name the setting to raise from the failure's type instead of its text.
#[derive(Debug)]
pub(crate) struct MemoryExhausted {
    budget: MemoryBudget,
    /// The failure that reported it, such as SQLite's full database.
    source: Option<Source>,
}

impl MemoryExhausted {
    pub(crate) fn new(budget: MemoryBudget) -> Self {
        Self {
            budget,
            source: None,
        }
    }
}

impl fmt::Display for MemoryExhausted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.budget {
            MemoryBudget::Blobs => {
                "RAM blob byte budget exhausted; delete objects or increase memory_max_bytes"
            }
            MemoryBudget::Catalog => {
                "RAM catalog page budget exhausted; increase memory_catalog_max_bytes"
            }
        })
    }
}

impl Error for MemoryExhausted {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|error| error as &(dyn Error + 'static))
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
    let error: Source = Arc::new(error);
    let source: Source = match CATALOG_PAGES.get() {
        CatalogPages::Disk => error,
        CatalogPages::Memory => Arc::new(MemoryExhausted {
            budget: MemoryBudget::Catalog,
            source: Some(error),
        }),
    };
    IoCause {
        kind: io::ErrorKind::StorageFull,
        source: Some(source),
    }
}

/// Plain failures retain the kind-only message; contextual failures also identify their entry.
impl fmt::Display for IoCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(context) = self
            .source
            .as_deref()
            .and_then(|source| source.downcast_ref::<BackendCause>())
        {
            return context.fmt(formatter);
        }
        fmt::Display::fmt(&self.kind, formatter)
    }
}

impl fmt::Debug for IoCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Sync diagnostics expose this text, so preserve the original kind format.
        fmt::Debug::fmt(&self.kind, formatter)
    }
}

impl PartialEq for IoCause {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl Eq for IoCause {}

impl From<io::ErrorKind> for IoCause {
    fn from(kind: io::ErrorKind) -> Self {
        Self { kind, source: None }
    }
}

impl From<io::Error> for IoCause {
    fn from(error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            source: Some(Arc::new(error)),
        }
    }
}

/// A backend failure sorted into the variants both error enums share.
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

impl StorageCause for CatalogError {
    fn into_cause(self) -> BackendCause {
        BackendCause::from_error(self)
    }

    fn classify(self) -> Classified {
        match self {
            Self::Corrupt(cause) => Classified::Corrupt(cause),
            Self::Io(cause) => Classified::Io(cause),
            error => Classified::Unavailable(BackendCause::from_error(error)),
        }
    }
}

impl StorageCause for ObjectError {
    fn into_cause(self) -> BackendCause {
        BackendCause::from_error(self)
    }

    fn classify(self) -> Classified {
        match self {
            Self::Corrupt(cause) => Classified::Corrupt(cause),
            Self::Io(cause) => Classified::Io(cause),
            error => Classified::Unavailable(BackendCause::from_error(error)),
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
    crate::storage::blob_id::InvalidBlobId,
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
    match cause.classify() {
        Classified::Corrupt(cause) => ObjectError::Corrupt(BackendCause::with_source(
            format!("{context}: {cause}"),
            cause,
        )),
        Classified::Unavailable(cause) => ObjectError::Unavailable(BackendCause::with_source(
            format!("{context}: {cause}"),
            cause,
        )),
        Classified::Io(cause) => {
            let message = format!(
                "{context}: {}",
                cause
                    .source()
                    .map_or_else(|| cause.kind().to_string(), ToString::to_string)
            );
            ObjectError::Io(IoCause {
                kind: cause.kind(),
                source: Some(Arc::new(BackendCause {
                    message: message.into(),
                    source: cause.source,
                })),
            })
        }
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
    tokio::task::JoinError,
    crate::task::BlockingFailed
);

#[cfg(test)]
mod tests;
