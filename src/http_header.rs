//! Shared cardinality checks for HTTP fields whose callers accept at most one value.

use hyper::header::{AsHeaderName, HeaderMap, HeaderValue, ToStrError};

pub(crate) enum Sole<'a> {
    Absent,
    Value(&'a HeaderValue),
    Repeated { first: &'a HeaderValue },
}

pub(crate) fn sole(headers: &HeaderMap, name: impl AsHeaderName) -> Sole<'_> {
    let mut values = headers.get_all(name).iter();
    match (values.next(), values.next()) {
        (None, _) => Sole::Absent,
        (Some(value), None) => Sole::Value(value),
        (Some(first), Some(_)) => Sole::Repeated { first },
    }
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("repeated HTTP field")]
pub(crate) struct Repeated;

/// Which refusal takes priority when a repeated field's first value is not visible text.
#[derive(Clone, Copy)]
pub(crate) enum TextOrder {
    RepetitionFirst,
    ValueFirst,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TextError {
    #[error(transparent)]
    Repeated(#[from] Repeated),
    #[error(transparent)]
    Invalid(#[from] ToStrError),
}

impl<'a> Sole<'a> {
    pub(crate) fn value(self) -> Result<Option<&'a HeaderValue>, Repeated> {
        match self {
            Self::Absent => Ok(None),
            Self::Value(value) => Ok(Some(value)),
            Self::Repeated { .. } => Err(Repeated),
        }
    }

    pub(crate) fn text(self, order: TextOrder) -> Result<Option<&'a str>, TextError> {
        match self {
            Self::Absent => Ok(None),
            Self::Value(value) => Ok(Some(value.to_str()?)),
            Self::Repeated { first } => {
                if matches!(order, TextOrder::ValueFirst) {
                    first.to_str()?;
                }
                Err(Repeated.into())
            }
        }
    }
}

#[cfg(test)]
mod tests;
