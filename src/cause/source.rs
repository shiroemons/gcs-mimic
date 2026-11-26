//! Owned and borrowed links for backend classification and diagnostics.

use super::{BackendCause, BackendFailure, MemoryBudget, MemoryExhausted};
use crate::fsutil::Unopenable;
use std::{error::Error, fmt, io};

/// Owning a layer preserves the concrete error returned by `source()`.
pub(crate) struct Source(Box<dyn Layer>);

pub(crate) trait Layer: Error + Send + Sync + 'static {
    fn link(&self) -> Link<'_>;

    /// A cause included in this layer's historical public diagnostic.
    fn embedded_cause(&self) -> Option<Link<'_>> {
        None
    }

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

impl fmt::Display for Link<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_error(), f)?;
        if let Self::Typed(layer) = self
            && let Some(cause) = layer.embedded_cause()
        {
            write!(f, ": {cause}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Source {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.link(), formatter)
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
    Unopenable(Unopenable),
);
