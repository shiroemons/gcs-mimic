//! JSON API integer query parameters and the wording Cloud Storage refuses each with.

use super::ApiError;
use crate::decimal::Grammar;
use crate::object::generation::Generation;
use std::collections::BTreeMap;

/// A signed 64-bit object parameter, refused with Cloud Storage's `Invalid long value` wording.
pub(super) fn take_long(
    query: &mut BTreeMap<String, String>,
    key: &str,
) -> Result<Option<i64>, ApiError> {
    query
        .remove(key)
        .map(|value| {
            Grammar::SignedNoPlus
                .parse(&value)
                .ok_or_else(|| ApiError::invalid_long(&value).at_parameter(key))
        })
        .transpose()
}

/// A copy or rewrite source selector, which must name a concrete generation when present.
pub(super) fn take_source_generation(
    query: &mut BTreeMap<String, String>,
    nonpositive: &'static str,
) -> Result<Option<Generation>, ApiError> {
    take_long(query, "sourceGeneration")?
        .map(Generation::try_from)
        .transpose()
        .map_err(|_| ApiError::invalid_parameter(nonpositive))
}

/// A signed 64-bit bucket or ACL precondition, refused as "`key` must be an int64".
pub(super) fn take_int64(
    query: &mut BTreeMap<String, String>,
    key: &str,
) -> Result<Option<i64>, ApiError> {
    query
        .remove(key)
        .map(|value| {
            Grammar::SignedNoPlus.parse(&value).ok_or_else(|| {
                ApiError::invalid_parameter(format!("{key} must be an int64")).at_parameter(key)
            })
        })
        .transpose()
}

/// An unsigned 32-bit `maxResults`, refused with Cloud Storage's `Invalid unsigned integer
/// value` wording (objects.list and hmacKeys.list; buckets.list keeps its own wording).
pub(super) fn take_max_results(
    query: &mut BTreeMap<String, String>,
) -> Result<Option<u32>, ApiError> {
    query
        .remove("maxResults")
        .map(|value| {
            Grammar::Digits
                .parse(&value)
                .ok_or_else(|| ApiError::invalid_unsigned(&value).at_parameter("maxResults"))
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_generation_keeps_syntax_and_nonpositive_errors_distinct() {
        let query = |text: &str| BTreeMap::from([("sourceGeneration".into(), text.into())]);
        for message in [
            "sourceGeneration must be positive",
            "Invalid sourceGeneration",
        ] {
            let parsed = take_source_generation(&mut query("09223372036854775807"), message);
            assert_eq!(parsed.unwrap().map(Generation::get), Some(i64::MAX));
            for text in ["0", "-0", "-1"] {
                let error = take_source_generation(&mut query(text), message).unwrap_err();
                assert_eq!(error.reason(), "invalidParameter");
                assert_eq!(error.message(), message);
            }
            for text in ["+1", "9223372036854775808"] {
                let error = take_source_generation(&mut query(text), message).unwrap_err();
                assert_eq!(error.reason(), "invalidParameter");
                assert_eq!(error.message(), format!("Invalid long value: '{text}'."));
            }
        }
    }
}
