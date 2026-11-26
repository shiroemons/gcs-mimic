//! Percent-decoding of URI components and form-encoded query strings. The JSON, XML and gRPC
//! adapters decode paths, queries and routing parameters alike; each answers a refusal in its
//! own protocol.

use std::{
    collections::{BTreeMap, btree_map::Entry},
    fmt,
    sync::OnceLock,
};

/// A request target whose form-decoded parameters are shared from their first use.
#[derive(Clone, Default)]
pub(crate) struct RequestUri {
    uri: hyper::Uri,
    parameters: OnceLock<Result<BTreeMap<String, String>, QueryError>>,
}

impl From<hyper::Uri> for RequestUri {
    fn from(uri: hyper::Uri) -> Self {
        Self {
            uri,
            parameters: OnceLock::new(),
        }
    }
}

impl std::ops::Deref for RequestUri {
    type Target = hyper::Uri;
    fn deref(&self) -> &Self::Target {
        &self.uri
    }
}

impl RequestUri {
    pub(crate) fn parameters(&self) -> Result<&BTreeMap<String, String>, QueryError> {
        self.parameters
            .get_or_init(|| parse_query(self.query().unwrap_or_default()))
            .as_ref()
            .map_err(Clone::clone)
    }

    /// The Discovery upload alias changes the path and keeps the query byte-for-byte.
    pub(crate) fn rewrite_path(&mut self, uri: hyper::Uri) {
        debug_assert_eq!(self.uri.query(), uri.query());
        self.uri = uri;
    }
}

#[cfg(test)]
pub(crate) static EMPTY_REQUEST_URI: std::sync::LazyLock<RequestUri> =
    std::sync::LazyLock::new(RequestUri::default);

#[cfg(test)]
impl fmt::Display for RequestUri {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.uri.fmt(formatter)
    }
}

#[cfg(test)]
impl std::str::FromStr for RequestUri {
    type Err = hyper::http::uri::InvalidUri;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse::<hyper::Uri>().map(Self::from)
    }
}

/// The longest raw query string a request may carry. Cloud Storage documents no limit; this
/// bounds the work and memory one query can cause. The gRPC v1 listing adapter also applies it
/// to the encoded query it would build, since it does not go through the HTTP/1 head limit.
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
    let bytes = decode_bytes(raw, decoding).map_err(|_| MalformedEncoding)?;
    String::from_utf8(bytes).map_err(|_| MalformedEncoding)
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("Malformed percent escape")]
pub(crate) struct MalformedEscape;

/// Expands escapes without interpreting the resulting bytes as UTF-8.
pub(crate) fn decode_bytes(raw: &str, decoding: Decoding) -> Result<Vec<u8>, MalformedEscape> {
    let mut bytes = Vec::with_capacity(raw.len());
    let mut input = raw.bytes();
    while let Some(byte) = input.next() {
        bytes.push(match byte {
            b'%' => crate::hex::byte([
                input.next().ok_or(MalformedEscape)?,
                input.next().ok_or(MalformedEscape)?,
            ])
            .ok_or(MalformedEscape)?,
            b'+' if decoding == Decoding::Form => b' ',
            byte => byte,
        });
    }
    Ok(bytes)
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
            Self::Malformed(error) => error.source(),
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
        match query.entry(key) {
            Entry::Vacant(entry) => {
                entry.insert(value);
            }
            Entry::Occupied(entry) => return Err(QueryError::Duplicate(entry.key().clone())),
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
    use crate::url::{encode_listing_name, encode_object_name, encode_unreserved};
    use proptest::prelude::*;

    #[test]
    fn rewriting_an_upload_alias_preserves_the_decoded_query() {
        let mut uri: RequestUri = "/storage/v1/b/bucket/o?name=a%2Fb".parse().unwrap();
        assert!(uri.parameters.get().is_none());
        let address = std::ptr::from_ref(&uri.parameters().unwrap()["name"]);
        uri.rewrite_path("/upload/storage/v1/b/bucket/o?name=a%2Fb".parse().unwrap());
        let name = &uri.parameters().unwrap()["name"];
        assert_eq!(name, "a/b");
        assert_eq!(address, std::ptr::from_ref(name));
    }

    proptest! {
        #[test]
        fn encoded_paths_and_form_components_round_trip(value in any::<String>()) {
            prop_assert_eq!(&decode(&encode_object_name(&value), Decoding::Path).unwrap(), &value);
            prop_assert_eq!(&decode(&encode_listing_name(&value), Decoding::Form).unwrap(), &value);
        }

        #[test]
        fn arbitrary_uri_text_never_panics(raw in any::<String>()) {
            for decoding in [Decoding::Path, Decoding::Form] {
                let _ = decode(&raw, decoding);
                let _ = parse_query_with(&raw, decoding);
            }
        }

        #[test]
        fn encoded_query_separators_stay_inside_their_components(key in any::<String>(), value in any::<String>()) {
            let query = format!("{}={}", encode_unreserved(&key), encode_unreserved(&value));
            let parsed = parse_query(&query).unwrap();
            prop_assert_eq!(parsed, BTreeMap::from([(key, value)]));
        }
    }

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
    fn byte_decoding_distinguishes_bad_escapes_from_non_utf8() {
        assert_eq!(decode_bytes("%ff+%2B", Decoding::Path).unwrap(), b"\xff++");
        assert_eq!(decode_bytes("%ff+%2B", Decoding::Form).unwrap(), b"\xff +");
        for raw in ["%", "%2", "%g1"] {
            assert!(decode_bytes(raw, Decoding::Path).is_err(), "{raw}");
        }
    }

    #[test]
    fn query_limits_and_duplicate_keys_use_their_own_decoding_stage() {
        let count = (MAX_QUERY_BYTES - 2) / 3;
        let tail = (MAX_QUERY_BYTES - 2) % 3;
        let encoded = format!("x={}{}", "%41".repeat(count), "A".repeat(tail));
        assert_eq!(encoded.len(), MAX_QUERY_BYTES);
        assert_eq!(
            parse_query(&encoded).unwrap()["x"],
            "A".repeat(count + tail)
        );
        assert_eq!(parse_query(&(encoded + "A")), Err(QueryError::TooLong));
        assert_eq!(
            parse_query("a=1&%61=2"),
            Err(QueryError::Duplicate("a".into()))
        );
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
