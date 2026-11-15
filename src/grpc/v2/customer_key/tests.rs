use super::*;
use crate::csek::CsekError;
use crate::grpc::test_support::{assert_customer_encryption, assert_unsupported, key_params};

#[test]
fn absent_and_default_parameters_carry_no_key() {
    assert_eq!(key(None).unwrap(), None);
    assert_eq!(
        key(Some(&pb::CommonObjectRequestParams::default())).unwrap(),
        None
    );
}

#[test]
fn raw_keys_parse_to_their_fingerprint_and_failures_use_the_grpc_contract() {
    let parsed = key(key_params(&[7; 32]).as_ref()).unwrap().unwrap();
    assert_eq!(parsed.sha256(), crate::csek::key_sha256(&[7; 32]));
    let mut fingerprint_only = key_params(&[7; 32]).unwrap();
    fingerprint_only.encryption_algorithm.clear();
    fingerprint_only.encryption_key_bytes.clear();
    assert_customer_encryption(key(Some(&fingerprint_only)), CsekError::KeyFormatInvalid);
}

#[test]
fn rewrite_keys_come_from_their_own_fields_destination_first() {
    let source = key_params(&[1; 32]).unwrap();
    let request = pb::RewriteObjectRequest {
        common_object_request_params: key_params(&[2; 32]),
        copy_source_encryption_algorithm: source.encryption_algorithm.clone(),
        copy_source_encryption_key_bytes: source.encryption_key_bytes.clone(),
        copy_source_encryption_key_sha256_bytes: source.encryption_key_sha256_bytes.clone(),
        ..Default::default()
    };
    let keys = copy_keys(&request).unwrap();
    assert_eq!(
        keys.destination.map(CustomerKey::sha256),
        Some(crate::csek::key_sha256(&[2; 32]))
    );
    assert_eq!(
        keys.source.map(CustomerKey::sha256),
        Some(crate::csek::key_sha256(&[1; 32]))
    );
    assert_eq!(
        copy_keys(&pb::RewriteObjectRequest::default()).unwrap(),
        CopyKeys::default()
    );
    // Both keys malformed: the destination's failure is reported.
    let both_bad = pb::RewriteObjectRequest {
        common_object_request_params: Some(pb::CommonObjectRequestParams {
            encryption_algorithm: "AES256".into(),
            ..Default::default()
        }),
        copy_source_encryption_algorithm: "AES128".into(),
        copy_source_encryption_key_bytes: source.encryption_key_bytes,
        copy_source_encryption_key_sha256_bytes: source.encryption_key_sha256_bytes,
        ..Default::default()
    };
    assert_customer_encryption(copy_keys(&both_bad), CsekError::KeyFormatInvalid);
}

#[test]
fn unmodelled_rpcs_reject_a_key_without_sending_clients_to_the_json_api() {
    assert!(reject(None, "RestoreObject").is_ok());
    assert!(
        reject(
            Some(&pb::CommonObjectRequestParams::default()),
            "RestoreObject"
        )
        .is_ok()
    );
    let result = reject(key_params(&[7; 32]).as_ref(), "RestoreObject");
    let status = result.as_ref().unwrap_err().clone();
    assert_unsupported(result, "customer-supplied encryption on RestoreObject");
    assert!(!status.message().contains("JSON API"));
}
