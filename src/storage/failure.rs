//! Backend failure causes shared by the bucket and object error types.
//!
//! Every storage module maps SQLite, filesystem and task failures through this module so
//! that the corrupt / I/O / unavailable classification and the client-visible messages stay
//! identical across modules, and so that the original error remains available as `source()`.

use crate::bucket::CatalogError;
use crate::object::ObjectError;
use crate::storage::memory::BlobError;
use std::{
    borrow::Cow,
    cell::Cell,
    error::Error,
    fmt, io,
    num::TryFromIntError,
    sync::{Arc, PoisonError},
};

mod kind;
pub(crate) use kind::BackendFailure;
mod source;
pub(crate) use source::{Link, Source, TypedSource, chain};
pub(crate) mod wording;

/// The message and optional underlying error of an unavailable or corrupt backend.
///
/// `Display` prints only the message. A cause built from an error uses that error's own
/// `Display` text as the message, so wrapped errors render exactly as the stringified
/// errors did before sources were kept. Equality compares messages, which is what callers
/// and tests observe.
#[derive(Clone)]
pub struct BackendCause {
    message: Cow<'static, str>,
    source: Option<Arc<Source>>,
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
    pub(crate) fn from_error(error: impl Into<Source>) -> Self {
        let source = error.into();
        Self {
            message: source.to_string().into(),
            source: Some(Arc::new(source)),
        }
    }

    /// A cause with a contextual message that already describes `error`.
    pub(crate) fn with_source(
        message: impl Into<Cow<'static, str>>,
        error: impl Into<Source>,
    ) -> Self {
        Self {
            message: message.into(),
            source: Some(Arc::new(error.into())),
        }
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    /// The fixed failure this cause reports, however many typed layers describe it.
    pub(crate) fn failure(&self) -> Option<BackendFailure> {
        chain(Link::Context(self)).find_map(Link::failure)
    }

    /// SQLite's extended result code, when an SQLite failure caused this one.
    pub(crate) fn sqlite_extended_code(&self) -> Option<std::ffi::c_int> {
        chain(Link::Context(self)).find_map(Link::sqlite_extended_code)
    }

    pub(crate) fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.typed_source().map(Link::as_error)
    }
}

impl TypedSource for BackendCause {
    fn typed_source(&self) -> Option<Link<'_>> {
        self.source.as_deref().map(Source::link)
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
///
/// The source is the error that produced the failure, or, when [`object_context`] added a
/// message naming the entry the failure hit, a [`Source::Context`] whose own source is that
/// error.
#[derive(Clone)]
pub struct IoCause {
    kind: io::ErrorKind,
    source: Option<Arc<Source>>,
}

impl IoCause {
    /// A failure of kind `Other` that keeps `error` as its typed source, as `io::Error::other`
    /// would without hiding the error's type from the decisions that read it.
    pub(crate) fn other(error: impl Into<Source>) -> Self {
        Self {
            kind: io::ErrorKind::Other,
            source: Some(Arc::new(error.into())),
        }
    }

    pub(crate) fn kind(&self) -> io::ErrorKind {
        self.kind
    }

    pub(crate) fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.typed_source().map(Link::as_error)
    }

    /// The `backend=memory` budget this failure exhausted, if it is one. A full disk carries none.
    pub fn memory_budget(&self) -> Option<MemoryBudget> {
        chain(self.typed_source()?).find_map(Link::memory_budget)
    }
}

/// An exhausted `backend=memory` budget is a full-storage failure that names that budget.
impl From<MemoryExhausted> for IoCause {
    fn from(exhausted: MemoryExhausted) -> Self {
        Self {
            kind: io::ErrorKind::StorageFull,
            source: Some(Arc::new(Source::Exhausted(exhausted))),
        }
    }
}

impl From<BlobError> for IoCause {
    fn from(error: BlobError) -> Self {
        Self {
            kind: error.kind(),
            source: Some(Arc::new(error.into())),
        }
    }
}

impl TypedSource for IoCause {
    fn typed_source(&self) -> Option<Link<'_>> {
        self.source.as_deref().map(Source::link)
    }
}

/// A `backend=memory` budget, each with its own remedy when it runs out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryBudget {
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
    /// SQLite's full database, when the catalog page budget ran out.
    source: Option<rusqlite::Error>,
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
        self.typed_source().map(Link::as_error)
    }
}

impl TypedSource for MemoryExhausted {
    fn typed_source(&self) -> Option<Link<'_>> {
        self.source.as_ref().map(Link::Sqlite)
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
        CatalogPages::Disk => Source::Sqlite(error),
        CatalogPages::Memory => Source::Exhausted(MemoryExhausted {
            budget: MemoryBudget::Catalog,
            source: Some(error),
        }),
    };
    IoCause {
        kind: io::ErrorKind::StorageFull,
        source: Some(Arc::new(source)),
    }
}

/// Plain failures retain the kind-only message; contextual failures also identify their entry.
impl fmt::Display for IoCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(Source::Context(context)) = self.source.as_deref() {
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

/// Lets the cause travel inside an `io::Error` of its kind, with its source still reachable.
impl Error for IoCause {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        IoCause::source(self)
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
            source: Some(Arc::new(Source::Io(error))),
        }
    }
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
        Classified::Io(cause) => {
            let message = format!(
                "{context}: {}",
                cause
                    .source()
                    .map_or_else(|| cause.kind().to_string(), ToString::to_string)
            );
            Classified::Io(IoCause {
                kind: cause.kind(),
                source: Some(Arc::new(Source::Context(BackendCause {
                    message: message.into(),
                    source: cause.source,
                }))),
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
    BlobError,
    tokio::task::JoinError,
    crate::task::BlockingFailed
);

#[cfg(test)]
pub(crate) mod repeated_source_tests;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
