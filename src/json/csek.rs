//! JSON adapter rules for customer-supplied encryption keys: header parsing, error mapping and
//! which representation fields a caller may see.

use super::JsonError;
use crate::{
    csek::{CsekError, CustomerKey, DESTINATION_PREFIX, SOURCE_PREFIX},
    object::ObjectRecord,
};
use hyper::{HeaderMap, StatusCode, header::HeaderValue};
use serde_json::{Value, json};

impl From<CsekError> for JsonError {
    fn from(error: CsekError) -> Self {
        Self::new(StatusCode::BAD_REQUEST, error.reason(), error.message())
    }
}

/// The key for the addressed (or destination) object: `x-goog-encryption-*`.
pub(crate) fn request_key(headers: &HeaderMap) -> Result<Option<CustomerKey>, JsonError> {
    CustomerKey::from_headers(headers, DESTINATION_PREFIX).map_err(JsonError::from)
}

/// The copy/rewrite source key: `x-goog-copy-source-encryption-*`.
pub(crate) fn source_key(headers: &HeaderMap) -> Result<Option<CustomerKey>, JsonError> {
    CustomerKey::from_headers(headers, SOURCE_PREFIX).map_err(JsonError::from)
}

/// Encryption output fields for a resource. Hashes describe the stored plaintext, so Cloud
/// Storage returns them for an encrypted object only to callers that supplied its key.
pub(super) fn render(object: &ObjectRecord, value: &mut Value) {
    let Some(sha256) = object.customer_key_sha256 else {
        return;
    };
    value["customerEncryption"] = json!({
        "encryptionAlgorithm": crate::csek::ALGORITHM,
        "keySha256": super::base64(&sha256),
    });
    if let Some(map) = value.as_object_mut() {
        map.remove("md5Hash");
        map.remove("crc32c");
    }
}

/// Render a resource for a caller whose `key` has already been verified against `object`.
pub(crate) fn object_resource_keyed(
    object: &ObjectRecord,
    base_url: &str,
    projection: super::Projection,
    key: Option<CustomerKey>,
) -> Result<Value, JsonError> {
    let mut value = super::object_resource(object, base_url, projection)?;
    if key.is_some_and(|key| object.customer_key_sha256 == Some(key.sha256())) {
        value["crc32c"] = json!(super::base64(&object.crc32c.to_be_bytes()));
        if object.component_count.is_none() {
            value["md5Hash"] = json!(super::base64(&object.md5));
        }
    }
    Ok(value)
}

/// Response headers identifying the key of an encrypted object's data.
pub(crate) fn insert_response_headers(object: &ObjectRecord, headers: &mut HeaderMap) {
    if let Some(sha256) = object.customer_key_sha256 {
        headers.insert(
            "x-goog-encryption-algorithm",
            HeaderValue::from_static(crate::csek::ALGORITHM),
        );
        // Base64 always forms a valid header value; omitting the header is safer than
        // panicking if that ever changes.
        if let Ok(value) = HeaderValue::from_str(&super::base64(&sha256)) {
            headers.insert("x-goog-encryption-key-sha256", value);
        }
    }
}
