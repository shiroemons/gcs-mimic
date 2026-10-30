//! Backend failure causes shared by the bucket and object error types.
//!
//! Every storage module maps SQLite, filesystem and task failures through this module so
//! that the corrupt / I/O / unavailable classification and the client-visible messages stay
//! identical across modules, and so that the original error remains available as `source()`.

use crate::bucket::CatalogError;
use crate::object::ObjectError;
use std::{
    borrow::Cow,
    error::Error,
    fmt, io,
    num::TryFromIntError,
    sync::{Arc, PoisonError},
};

type Source = Arc<dyn Error + Send + Sync + 'static>;

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

/// Formats like the bare message so `Debug` output of the error enums is unchanged.
impl fmt::Debug for BackendCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
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
}

/// Formats like the kind alone so messages such as `catalog I/O: {kind}` are unchanged.
impl fmt::Display for IoCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.kind, formatter)
    }
}

impl fmt::Debug for IoCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
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
                Classified::Io(IoCause {
                    kind: io::ErrorKind::StorageFull,
                    source: Some(Arc::new(self)),
                })
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
    tokio::sync::oneshot::error::RecvError,
    tokio::task::JoinError,
    crate::fsutil::FsError,
    crate::storage::admin::AdminError,
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

/// Maps an object failure to `ObjectError`, classifying corrupt and I/O causes.
pub(crate) fn object_unavailable(cause: impl StorageCause) -> ObjectError {
    cause.classify().into()
}

/// Maps an object failure to `ObjectError::Unavailable` without classifying it.
///
/// Derived operations (compose, rewrite, derived transfers) and resumable transfers have
/// always reported every backend failure as unavailable; classifying them would change the
/// messages clients receive for corrupt or I/O failures on those paths.
pub(crate) fn object_unclassified(cause: impl StorageCause) -> ObjectError {
    ObjectError::Unavailable(cause.into_cause())
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

classified_from!(rusqlite::Error, io::Error, tokio::task::JoinError);

#[cfg(test)]
mod tests;
