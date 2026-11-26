//! Retry identifiers are opaque and must retain their exact text when used for replay.

use crate::http_header::{Repeated, TextError, TextOrder, sole};
use hyper::HeaderMap;

pub(crate) struct IdempotencyToken<'a>(&'a str);

#[derive(Debug, thiserror::Error)]
pub(crate) enum TokenError {
    #[error(transparent)]
    Repeated(#[from] Repeated),
    #[error(transparent)]
    InvalidText(hyper::header::ToStrError),
    #[error("empty idempotency token")]
    Empty,
}

impl From<TextError> for TokenError {
    fn from(error: TextError) -> Self {
        match error {
            TextError::Repeated(error) => Self::Repeated(error),
            TextError::Invalid(error) => Self::InvalidText(error),
        }
    }
}

impl<'a> IdempotencyToken<'a> {
    pub(crate) const HEADER: &str = "x-goog-gcs-idempotency-token";

    /// A request that does not consume the token needs only the field's byte-level grammar.
    pub(crate) fn validate_opaque(headers: &HeaderMap) -> Result<(), TokenError> {
        if let Some(value) = sole(headers, Self::HEADER).value()? {
            nonempty(value.as_bytes())?;
        }
        Ok(())
    }

    /// Replay and XML writes also require text, without trimming the accepted token.
    pub(crate) fn from_text(
        headers: &'a HeaderMap,
        order: TextOrder,
    ) -> Result<Option<Self>, TokenError> {
        let Some(value) = sole(headers, Self::HEADER).text(order)? else {
            return Ok(None);
        };
        nonempty(value.as_bytes())?;
        Ok(Some(Self(value)))
    }

    pub(crate) fn as_str(&self) -> &'a str {
        self.0
    }
}

fn nonempty(bytes: &[u8]) -> Result<(), TokenError> {
    if bytes.trim_ascii().is_empty() {
        return Err(TokenError::Empty);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
