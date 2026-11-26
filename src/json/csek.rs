//! JSON adapter rules for customer-supplied encryption keys: header parsing, error mapping and
//! which representation fields a caller may see.

use super::ApiError;
use crate::{
    csek::{CopyKeys, CustomerKey, DESTINATION_PREFIX, SOURCE_PREFIX, StoredHashes},
    object::ObjectRecord,
};
use hyper::HeaderMap;
use serde_json::{Map, Value, json};

/// The key for the addressed (or destination) object: `x-goog-encryption-*`.
pub(crate) fn request_key(
    headers: &HeaderMap,
) -> Result<Option<CustomerKey>, crate::csek::CsekError> {
    CustomerKey::from_headers(headers, DESTINATION_PREFIX)
}

/// The copy/rewrite source key: `x-goog-copy-source-encryption-*`.
pub(crate) fn source_key(headers: &HeaderMap) -> Result<Option<CustomerKey>, ApiError> {
    CustomerKey::from_headers(headers, SOURCE_PREFIX).map_err(ApiError::from)
}

/// Both keys of a copy or rewrite; the destination is parsed first, so its failure wins.
pub(crate) fn copy_keys(headers: &HeaderMap) -> Result<CopyKeys, ApiError> {
    Ok(CopyKeys {
        destination: request_key(headers)?,
        source: source_key(headers)?,
    })
}

/// Encryption output fields for a resource. Hashes describe the stored plaintext, so Cloud
/// Storage returns them for an encrypted object only to callers that supplied its key.
pub(super) fn render(
    object: &ObjectRecord,
    key: Option<CustomerKey>,
    value: &mut Map<String, Value>,
) {
    let Some(sha256) = object.customer_key_sha256 else {
        return;
    };
    value.insert(
        "customerEncryption".into(),
        json!({
            "encryptionAlgorithm": crate::csek::ALGORITHM,
            "keySha256": super::base64(&sha256),
        }),
    );
    // shift_remove keeps the recorded field order of the remaining fields.
    value.shift_remove("md5Hash");
    value.shift_remove("crc32c");
    if StoredHashes::for_key(Some(sha256), key) == StoredHashes::Revealed {
        value.insert(
            "crc32c".into(),
            super::base64(&object.crc32c.to_be_bytes()).into(),
        );
        if object.has_md5() {
            value.insert("md5Hash".into(), super::base64(&object.md5).into());
        }
    }
}

/// Render a resource with the supplied key; validation belongs to the request handler.
pub(crate) fn object_resource_keyed(
    object: &ObjectRecord,
    base_url: &str,
    projection: super::Projection,
    key: Option<CustomerKey>,
) -> Result<Value, crate::time_format::TimeOutOfRange> {
    super::objects::object_resource_fields(object, base_url, projection, key).map(Value::Object)
}
