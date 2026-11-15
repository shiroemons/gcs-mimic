//! Customer-supplied encryption keys on Storage v2 requests.
//!
//! The key travels in `CommonObjectRequestParams` as raw bytes; RewriteObject adds the source
//! key in its own `copy_source_encryption_*` fields. The pinned proto does not say which RPCs
//! use it, so each rule here follows Cloud Storage's answers recorded on 2026-11-15:
//! DeleteObject ignores a key, later messages of a write stream do not change the key the first
//! message chose, and RPCs whose semantics the emulator does not model reject it.

use super::{pb, status};
use crate::csek::{CopyKeys, CustomerKey};
use tonic::Status;

/// proto3 has no field presence for scalars: an empty value is an absent one.
fn present(bytes: &[u8]) -> Option<&[u8]> {
    (!bytes.is_empty()).then_some(bytes)
}

/// The key a request supplies for the object it addresses (or creates).
pub(super) fn key(
    params: Option<&pb::CommonObjectRequestParams>,
) -> Result<Option<CustomerKey>, Status> {
    let Some(params) = params else {
        return Ok(None);
    };
    CustomerKey::from_raw(
        present(params.encryption_algorithm.as_bytes()),
        present(&params.encryption_key_bytes),
        present(&params.encryption_key_sha256_bytes),
    )
    .map_err(status::customer_encryption)
}

/// The destination key from `common_object_request_params` and the source key from the
/// `copy_source_encryption_*` fields. The destination is parsed first, as the JSON API does.
pub(super) fn copy_keys(request: &pb::RewriteObjectRequest) -> Result<CopyKeys, Status> {
    let destination = key(request.common_object_request_params.as_ref())?;
    let source = CustomerKey::from_raw(
        present(request.copy_source_encryption_algorithm.as_bytes()),
        present(&request.copy_source_encryption_key_bytes),
        present(&request.copy_source_encryption_key_sha256_bytes),
    )
    .map_err(status::customer_encryption)?;
    Ok(CopyKeys {
        destination,
        source,
    })
}

/// Refuses a key on an RPC whose key semantics the emulator does not model. The JSON API
/// rejects these keys too, so the remedy is to omit them rather than to switch APIs.
pub(super) fn reject(
    params: Option<&pb::CommonObjectRequestParams>,
    rpc: &str,
) -> Result<(), Status> {
    if params.is_some_and(|params| params != &pb::CommonObjectRequestParams::default()) {
        return Err(status::unsupported_with_remedy(
            format!(
                "gcs-mimic does not support customer-supplied encryption on {rpc}; omit common_object_request_params"
            ),
            "Omit common_object_request_params; this RPC needs no customer-supplied key",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
