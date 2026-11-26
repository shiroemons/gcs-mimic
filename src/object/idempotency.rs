//! Retry identifiers are opaque and must retain their exact text when used for replay.

use crate::http_header::{Repeated, TextError, TextOrder, sole};
use hyper::HeaderMap;

pub(crate) struct IdempotencyToken<'a>(&'a str);

#[derive(Debug, thiserror::Error)]
pub(crate) enum OpaqueTokenError {
    #[error(transparent)]
    Repeated(#[from] Repeated),
    #[error("empty idempotency token")]
    Empty,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TextTokenError {
    #[error(transparent)]
    Header(#[from] TextError),
    #[error("empty idempotency token")]
    Empty,
}

impl<'a> IdempotencyToken<'a> {
    pub(crate) const HEADER: &str = "x-goog-gcs-idempotency-token";

    /// A request that does not consume the token needs only the field's byte-level grammar.
    pub(crate) fn validate_opaque(headers: &HeaderMap) -> Result<(), OpaqueTokenError> {
        if let Some(value) = sole(headers, Self::HEADER).value()?
            && is_blank(value.as_bytes())
        {
            return Err(OpaqueTokenError::Empty);
        }
        Ok(())
    }

    /// Replay and XML writes also require text, without trimming the accepted token.
    pub(crate) fn from_text(
        headers: &'a HeaderMap,
        order: TextOrder,
    ) -> Result<Option<Self>, TextTokenError> {
        let Some(value) = sole(headers, Self::HEADER).text(order)? else {
            return Ok(None);
        };
        if is_blank(value.as_bytes()) {
            return Err(TextTokenError::Empty);
        }
        Ok(Some(Self(value)))
    }

    pub(crate) fn as_str(&self) -> &'a str {
        self.0
    }
}

fn is_blank(bytes: &[u8]) -> bool {
    bytes.trim_ascii().is_empty()
}

#[cfg(test)]
mod tests;
