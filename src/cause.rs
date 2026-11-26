//! Failure identities, causes and typed links shared by storage and its callers.

use std::{borrow::Cow, error::Error, fmt, io, sync::Arc};

mod source;
pub(crate) use source::{Layer, Link, Source, TypedSource, chain};

/// Fixed storage failures whose identity must survive contextual error messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackendFailure {
    CatalogClosed,
    UploadWaitTimeout,
    RetainedSessionLimit,
    RewriteSessionLimit,
    FinalizationTimeout,
    CopyDeadline,
    SessionBusy,
    MissingCompletion,
}

impl BackendFailure {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::CatalogClosed => "catalog is closed",
            Self::UploadWaitTimeout => "upload catalog wait timed out",
            Self::RetainedSessionLimit => "retained upload session limit reached",
            Self::RewriteSessionLimit => "rewrite session limit reached",
            Self::FinalizationTimeout => "upload finalization timed out",
            Self::CopyDeadline => "copy request deadline exceeded",
            Self::SessionBusy => "upload session is busy",
            Self::MissingCompletion => "Completed upload session has no stored response",
        }
    }

    /// What an operator can do about the failure, or `None` when there is nothing to advise.
    /// Each adapter puts its own subject ("Object storage is unavailable: ") in front.
    pub(crate) fn remedy(self) -> Option<&'static str> {
        Some(match self {
            Self::CatalogClosed => {
                "catalog is closed; wait for the server to restart before retrying"
            }
            Self::UploadWaitTimeout => {
                "upload catalog wait timed out; retry with generation preconditions or increase catalog_wait_timeout_ms (for example, 300000; 0 disables it); a completed upload may still commit"
            }
            Self::RetainedSessionLimit => {
                "retained upload session limit reached; raise max_resumable_sessions (0 is unlimited), wait for expired sessions to be reclaimed or use single-request media uploads"
            }
            Self::RewriteSessionLimit => {
                "rewrite session limit reached; raise max_rewrite_sessions (0 is unlimited) or wait for unfinished rewrites to expire"
            }
            Self::FinalizationTimeout => {
                "upload finalization timed out; read object state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000; 0 disables it)"
            }
            Self::CopyDeadline => {
                "copy request deadline exceeded; read destination state before retrying with generation preconditions, or increase object_request_timeout_ms (for example, 1800000; 0 disables it)"
            }
            Self::SessionBusy => {
                "upload session is busy; wait for the current request to finish before sending another request for this session, or increase catalog_wait_timeout_ms (for example, 60000)"
            }
            Self::MissingCompletion => return None,
        })
    }
}

impl fmt::Display for BackendFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl Error for BackendFailure {}

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
    pub(crate) budget: MemoryBudget,
    /// SQLite's full database, when the catalog page budget ran out.
    pub(crate) source: Option<rusqlite::Error>,
}

impl MemoryExhausted {
    pub(crate) const fn new(budget: MemoryBudget) -> Self {
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
        self.source.as_ref().map(|error| error as &dyn Error)
    }
}

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
/// The source is the error that produced the failure, or, when storage recovery added a
/// message naming the entry the failure hit, a [`BackendCause`] whose own source is that
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
        Self::with_source(io::ErrorKind::Other, error)
    }

    pub(crate) fn with_source(kind: io::ErrorKind, source: impl Into<Source>) -> Self {
        Self {
            kind,
            source: Some(Arc::new(source.into())),
        }
    }

    pub(crate) fn with_context(self, context: impl fmt::Display) -> Self {
        let message = format!(
            "{context}: {}",
            self.source()
                .map_or_else(|| self.kind.to_string(), ToString::to_string)
        );
        Self::with_source(
            self.kind,
            BackendCause {
                message: message.into(),
                source: self.source,
            },
        )
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
        Self::with_source(io::ErrorKind::StorageFull, exhausted)
    }
}

impl TypedSource for IoCause {
    fn typed_source(&self) -> Option<Link<'_>> {
        self.source.as_deref().map(Source::link)
    }
}

impl TypedSource for MemoryExhausted {
    fn typed_source(&self) -> Option<Link<'_>> {
        self.source.as_ref().map(Link::Sqlite)
    }
}

/// Plain failures retain the kind-only message; contextual failures also identify their entry.
impl fmt::Display for IoCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(Link::Context(context)) = self.typed_source() {
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
        Self::with_source(error.kind(), error)
    }
}
