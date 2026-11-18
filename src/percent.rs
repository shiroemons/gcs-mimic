//! Percent-decoding of URI components and form-encoded query strings. The JSON, XML and gRPC
//! adapters decode paths, queries and routing parameters alike; each answers a refusal in its
//! own protocol.

use std::{collections::BTreeMap, fmt};

/// The longest raw query string a request may carry. Cloud Storage documents no limit; this
/// bounds the work and memory one query can cause.
pub(crate) const MAX_QUERY_BYTES: usize = 16 * 1024;

/// How `decode` treats `+`: form-encoded query components use it for a space, while path
/// segments keep it literally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decoding {
    Path,
    Form,
}

/// A component whose `%` escapes are incomplete or not hexadecimal, or whose bytes are not
/// UTF-8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MalformedEncoding;

impl fmt::Display for MalformedEncoding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Malformed URI encoding")
    }
}

impl std::error::Error for MalformedEncoding {}

pub(crate) fn decode(raw: &str, decoding: Decoding) -> Result<String, MalformedEncoding> {
    let mut bytes = Vec::with_capacity(raw.len());
    let mut input = raw.bytes();
    while let Some(byte) = input.next() {
        bytes.push(match byte {
            b'%' => crate::hex::byte(&[
                input.next().ok_or(MalformedEncoding)?,
                input.next().ok_or(MalformedEncoding)?,
            ])
            .ok_or(MalformedEncoding)?,
            b'+' if decoding == Decoding::Form => b' ',
            byte => byte,
        });
    }
    String::from_utf8(bytes).map_err(|_| MalformedEncoding)
}

/// Why a query string was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum QueryError {
    /// The query is longer than [`MAX_QUERY_BYTES`].
    TooLong,
    /// The query names this parameter more than once.
    Duplicate(String),
    Malformed(MalformedEncoding),
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong => formatter.write_str("Query exceeds 16 KiB"),
            Self::Duplicate(key) => write!(formatter, "Duplicate query parameter: {key}"),
            Self::Malformed(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for QueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Malformed(error) => Some(error),
            Self::TooLong | Self::Duplicate(_) => None,
        }
    }
}

impl From<MalformedEncoding> for QueryError {
    fn from(error: MalformedEncoding) -> Self {
        Self::Malformed(error)
    }
}

/// The parameters of a query string, each key and value decoded with `decoding`. A parameter
/// given twice is refused rather than letting one value silently win.
pub(crate) fn parse_query_with(
    raw: &str,
    decoding: Decoding,
) -> Result<BTreeMap<String, String>, QueryError> {
    if raw.len() > MAX_QUERY_BYTES {
        return Err(QueryError::TooLong);
    }
    let mut query = BTreeMap::new();
    for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = decode(key, decoding)?;
        let value = decode(value, decoding)?;
        if query.insert(key.clone(), value).is_some() {
            return Err(QueryError::Duplicate(key));
        }
    }
    Ok(query)
}

/// The parameters of a form-encoded query string.
pub(crate) fn parse_query(raw: &str) -> Result<BTreeMap<String, String>, QueryError> {
    parse_query_with(raw, Decoding::Form)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_decoding_turns_plus_into_a_space_and_path_decoding_keeps_it() {
        assert_eq!(decode("a+b%2B", Decoding::Form).unwrap(), "a b+");
        assert_eq!(decode("a+b%2B", Decoding::Path).unwrap(), "a+b+");
    }

    #[test]
    fn incomplete_escapes_and_invalid_utf8_are_malformed() {
        for raw in ["%", "%4", "%GG", "%FF"] {
            assert_eq!(decode(raw, Decoding::Path), Err(MalformedEncoding), "{raw}");
        }
    }

    #[test]
    fn query_refusals_name_their_cause() {
        let long = "a".repeat(MAX_QUERY_BYTES + 1);
        assert_eq!(
            parse_query(&long).unwrap_err().to_string(),
            "Query exceeds 16 KiB"
        );
        assert_eq!(
            parse_query("a=1&a=2").unwrap_err().to_string(),
            "Duplicate query parameter: a"
        );
        assert_eq!(
            parse_query("a=%G").unwrap_err().to_string(),
            "Malformed URI encoding"
        );
    }
}
