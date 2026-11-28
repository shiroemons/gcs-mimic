//! RewriteObject with customer-supplied keys: the source key in `copy_source_encryption_*`, the
//! destination key in `common_object_request_params`. The rules for a single call follow Cloud
//! Storage's recorded answers (2026-11-15); continuations follow the JSON API.

use super::super::test_support::{RESOURCE, setup, stored_object, undispatched};
use super::*;
use crate::auth::Auth;
use crate::csek::{CsekError, key_sha256};
use crate::grpc::test_support::{assert_customer_encryption, key_params, put_keyed};
use crate::object::ObjectError;
use crate::storage::Catalog;
use crate::storage::test_fixtures::{PERMISSIVE, live};
use std::sync::Arc;

const OLD: [u8; 32] = [1; 32];
const NEW: [u8; 32] = [2; 32];
const OTHER_BUCKET: &str = "projects/_/buckets/update-copy-other";

fn rewrite(source: &str, destination: &str) -> pb::RewriteObjectRequest {
    pb::RewriteObjectRequest {
        source_bucket: RESOURCE.into(),
        source_object: source.into(),
        destination_bucket: RESOURCE.into(),
        destination_name: destination.into(),
        ..Default::default()
    }
}

/// The request with `source` in the rewrite's own source key fields.
fn with_source_key(
    request: pb::RewriteObjectRequest,
    source: &[u8; 32],
) -> pb::RewriteObjectRequest {
    let params = key_params(source).unwrap_or_default();
    pb::RewriteObjectRequest {
        copy_source_encryption_algorithm: params.encryption_algorithm,
        copy_source_encryption_key_bytes: params.encryption_key_bytes,
        copy_source_encryption_key_sha256_bytes: params.encryption_key_sha256_bytes,
        ..request
    }
}

fn with_destination_key(
    request: pb::RewriteObjectRequest,
    destination: &[u8; 32],
) -> pb::RewriteObjectRequest {
    pb::RewriteObjectRequest {
        common_object_request_params: key_params(destination),
        ..request
    }
}

async fn call(
    storage: &Storage,
    request: pb::RewriteObjectRequest,
) -> Result<pb::RewriteResponse, Status> {
    storage
        .rewrite_object(undispatched(request), None)
        .await
        .map(Response::into_inner)
}

/// Opens the stored object's data with `key`, as every API's read does.
async fn read_with(
    catalog: &Catalog,
    bucket: &str,
    name: &str,
    key: Option<&[u8; 32]>,
) -> Result<crate::object::ObjectRecord, crate::object::ObjectError> {
    catalog
        .open_object_keyed(
            live(bucket, name),
            Default::default(),
            PERMISSIVE,
            key.map(|key| CustomerKey::from_sha256(key_sha256(key))),
            crate::object::ReadAhead::None,
        )
        .await
        .map(|(record, _)| record)
}

fn customer_encryption(key: &[u8; 32]) -> Option<pb::CustomerEncryption> {
    Some(pb::CustomerEncryption {
        encryption_algorithm: crate::csek::ALGORITHM.into(),
        key_sha256_bytes: key_sha256(key).to_vec(),
    })
}

#[tokio::test]
async fn grpc_rewrite_rotates_adds_and_removes_customer_keys() {
    let (s, _dir) = setup().await;
    let catalog = &s.state.catalog;
    let secret = put_keyed(
        catalog,
        "update-copy-bucket",
        "secret",
        b"secret",
        Some(&OLD),
    )
    .await;
    put_keyed(catalog, "update-copy-bucket", "plain", b"plain", None).await;

    // Rewriting an object onto itself with a new destination key rotates it.
    let rotated = call(
        &s,
        with_destination_key(with_source_key(rewrite("secret", "secret"), &OLD), &NEW),
    )
    .await
    .unwrap();
    assert!(rotated.done);
    let resource = rotated.resource.unwrap();
    assert_eq!(resource.customer_encryption, customer_encryption(&NEW));
    // The caller supplied the key, so the response shows the plaintext's hashes.
    assert_eq!(
        resource.checksums.and_then(|checksums| checksums.crc32c),
        Some(secret.crc32c)
    );
    let bucket = "update-copy-bucket";
    assert!(
        read_with(catalog, bucket, "secret", Some(&NEW))
            .await
            .is_ok()
    );
    assert_eq!(
        read_with(catalog, bucket, "secret", Some(&OLD)).await.err(),
        Some(ObjectError::CustomerEncryption(CsekError::KeyIncorrect))
    );

    // A source key alone decrypts; a destination key alone encrypts.
    let decrypted = call(&s, with_source_key(rewrite("secret", "decrypted"), &NEW))
        .await
        .unwrap();
    assert_eq!(decrypted.resource.unwrap().customer_encryption, None);
    assert!(read_with(catalog, bucket, "decrypted", None).await.is_ok());
    let encrypted = call(
        &s,
        with_destination_key(rewrite("plain", "encrypted"), &OLD),
    )
    .await
    .unwrap();
    let resource = encrypted.resource.unwrap();
    assert_eq!(resource.customer_encryption, customer_encryption(&OLD));
    assert!(resource.checksums.is_some());
    assert_eq!(
        read_with(catalog, bucket, "encrypted", None).await.err(),
        Some(ObjectError::CustomerEncryption(
            CsekError::ResourceIsEncrypted
        ))
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_rewrite_key_failures_create_no_destination() {
    let (s, _dir) = setup().await;
    let catalog = &s.state.catalog;
    put_keyed(
        catalog,
        "update-copy-bucket",
        "secret",
        b"secret",
        Some(&OLD),
    )
    .await;
    put_keyed(catalog, "update-copy-bucket", "plain", b"plain", None).await;
    let mut algorithm_only = rewrite("secret", "target");
    algorithm_only.copy_source_encryption_algorithm = crate::csek::ALGORITHM.into();
    // Recorded against Cloud Storage over gRPC on 2026-11-15.
    for (label, request, error) in [
        (
            "no keys",
            rewrite("secret", "target"),
            CsekError::ResourceIsEncrypted,
        ),
        (
            "wrong source key",
            with_source_key(rewrite("secret", "target"), &NEW),
            CsekError::KeyIncorrect,
        ),
        (
            "source key for an unencrypted source",
            with_source_key(rewrite("plain", "target"), &OLD),
            CsekError::ResourceNotEncrypted,
        ),
        (
            "destination key only",
            with_destination_key(rewrite("secret", "target"), &OLD),
            CsekError::ResourceIsEncrypted,
        ),
        (
            "malformed source key",
            algorithm_only,
            CsekError::KeyFormatInvalid,
        ),
    ] {
        assert_customer_encryption(call(&s, request).await, error);
        assert!(stored_object(&s, "target").await.is_none(), "{label}");
    }
    catalog.close().await.unwrap();
}

/// A rewrite to another location copies 1 MiB per call, so it needs continuations.
fn multi_call(source: &str) -> pb::RewriteObjectRequest {
    pb::RewriteObjectRequest {
        destination_bucket: OTHER_BUCKET.into(),
        max_bytes_rewritten_per_call: 1024 * 1024,
        ..with_destination_key(with_source_key(rewrite(source, "target"), &OLD), &NEW)
    }
}

fn continuation(token: &str) -> pb::RewriteObjectRequest {
    pb::RewriteObjectRequest {
        rewrite_token: token.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn grpc_rewrite_continuations_repeat_both_keys_across_a_restart() {
    let (s, dir) = setup().await;
    let data = vec![42; 3 * 1024 * 1024];
    let source = put_keyed(
        &s.state.catalog,
        "update-copy-bucket",
        "large",
        &data,
        Some(&OLD),
    )
    .await;
    let first = call(&s, multi_call("large")).await.unwrap();
    assert!(!first.done);

    // The session survives a restart with both fingerprints.
    s.state.catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let s = Storage {
        state: Arc::new(crate::service::state::State::for_test(
            catalog,
            Auth::permissive(),
        )),
        ..s
    };
    let keyed = |token: &str, source: Option<&[u8; 32]>, destination: Option<&[u8; 32]>| {
        let request = continuation(token);
        let request = match source {
            Some(key) => with_source_key(request, key),
            None => request,
        };
        match destination {
            Some(key) => with_destination_key(request, key),
            None => request,
        }
    };
    let token = first.rewrite_token.as_str();
    for (request, error) in [
        (keyed(token, None, None), CsekError::ResourceIsEncrypted),
        (
            keyed(token, Some(&OLD), None),
            CsekError::ResourceIsEncrypted,
        ),
        (
            keyed(token, Some(&NEW), Some(&NEW)),
            CsekError::KeyIncorrect,
        ),
        (
            keyed(token, Some(&OLD), Some(&OLD)),
            CsekError::KeyIncorrect,
        ),
    ] {
        // A refused continuation leaves the session usable.
        assert_customer_encryption(call(&s, request).await, error);
    }
    let second = call(&s, keyed(token, Some(&OLD), Some(&NEW)))
        .await
        .unwrap();
    assert_eq!(second.total_bytes_rewritten, 2 * 1024 * 1024);
    let last_request = keyed(&second.rewrite_token, Some(&OLD), Some(&NEW));
    let last = call(&s, last_request.clone()).await.unwrap();
    assert!(last.done);
    let resource = last.resource.clone().unwrap();
    assert_eq!(resource.customer_encryption, customer_encryption(&NEW));
    assert_eq!(
        resource.checksums.and_then(|checksums| checksums.crc32c),
        Some(source.crc32c)
    );
    // Replaying the last token answers with the stored response, still behind the keys.
    assert_eq!(call(&s, last_request).await.unwrap(), last);
    assert_customer_encryption(
        call(&s, continuation(&second.rewrite_token)).await,
        CsekError::ResourceIsEncrypted,
    );
    assert!(
        read_with(&s.state.catalog, "update-copy-other", "target", Some(&NEW))
            .await
            .is_ok()
    );
    s.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_rewrite_continuations_of_a_keyless_session_refuse_a_key() {
    let (s, _dir) = setup().await;
    put_keyed(
        &s.state.catalog,
        "update-copy-bucket",
        "large",
        &vec![7; 2 * 1024 * 1024],
        None,
    )
    .await;
    let request = pb::RewriteObjectRequest {
        destination_bucket: OTHER_BUCKET.into(),
        max_bytes_rewritten_per_call: 1024 * 1024,
        ..rewrite("large", "target")
    };
    let first = call(&s, request).await.unwrap();
    assert!(!first.done);
    assert_customer_encryption(
        call(
            &s,
            with_destination_key(continuation(&first.rewrite_token), &NEW),
        )
        .await,
        CsekError::ResourceNotEncrypted,
    );
    let last = call(&s, continuation(&first.rewrite_token)).await.unwrap();
    assert!(last.done);
    assert_eq!(last.resource.unwrap().customer_encryption, None);
    s.state.catalog.close().await.unwrap();
}
