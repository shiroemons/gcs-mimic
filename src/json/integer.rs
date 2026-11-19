//! JSON API integer query parameters and the wording Cloud Storage refuses each with.

use super::ApiError;
use crate::decimal::Grammar;
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
