//! UpdateObject and ComposeObject with a customer-supplied key in
//! `common_object_request_params`. The rules follow Cloud Storage's recorded answers over gRPC
//! (2026-11-15); the one key applies to every compose source and to the result.

use super::super::test_support::{RESOURCE, setup, stored_object};
use super::*;
use crate::csek::{CsekError, key_sha256};
use crate::grpc::test_support::{
    self as fixtures, assert_customer_encryption, key_params, put_keyed,
};

const BUCKET: &str = "update-copy-bucket";
const KEY: [u8; 32] = [1; 32];
const OTHER: [u8; 32] = [2; 32];

fn customer_encryption(key: &[u8; 32]) -> Option<pb::CustomerEncryption> {
    Some(pb::CustomerEncryption {
        encryption_algorithm: crate::csek::ALGORITHM.into(),
        key_sha256_bytes: key_sha256(key).to_vec(),
    })
}

fn relabel(name: &str, key: Option<&[u8; 32]>) -> pb::UpdateObjectRequest {
    pb::UpdateObjectRequest {
        object: Some(pb::Object {
            bucket: RESOURCE.into(),
            name: name.into(),
            content_type: "text/plain".into(),
            ..Default::default()
        }),
        update_mask: fixtures::mask(&["content_type"]),
        common_object_request_params: key.and_then(key_params),
        ..Default::default()
    }
}

async fn update(storage: &Storage, request: pb::UpdateObjectRequest) -> Result<pb::Object, Status> {
    storage
        .update_object(Request::new(request), None)
        .await
        .map(Response::into_inner)
}

fn compose(sources: &[&str], key: Option<&[u8; 32]>) -> pb::ComposeObjectRequest {
    pb::ComposeObjectRequest {
        destination: Some(pb::Object {
            bucket: RESOURCE.into(),
            name: "composed".into(),
            ..Default::default()
        }),
        source_objects: sources
            .iter()
            .map(|name| pb::compose_object_request::SourceObject {
                name: (*name).into(),
                ..Default::default()
            })
            .collect(),
        common_object_request_params: key.and_then(key_params),
        ..Default::default()
    }
}

async fn compose_with(
    storage: &Storage,
    request: pb::ComposeObjectRequest,
) -> Result<pb::Object, Status> {
    storage
        .compose_object(Request::new(request), None)
        .await
        .map(Response::into_inner)
}

#[tokio::test]
async fn grpc_update_object_reveals_hashes_only_to_the_key_holder() {
    let (s, _dir) = setup().await;
    put_keyed(&s.state.catalog, BUCKET, "secret", b"secret", Some(&KEY)).await;

    // Metadata needs no key, but the response withholds the plaintext's hashes.
    let keyless = update(&s, relabel("secret", None)).await.unwrap();
    assert_eq!(keyless.customer_encryption, customer_encryption(&KEY));
    assert_eq!(keyless.checksums, None);
    assert_eq!(keyless.content_type, "text/plain");

    let keyed = update(&s, relabel("secret", Some(&KEY))).await.unwrap();
    assert_eq!(keyed.customer_encryption, customer_encryption(&KEY));
    assert!(keyed.checksums.is_some());
    s.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_update_object_key_failures_commit_nothing() {
    let (s, _dir) = setup().await;
    put_keyed(&s.state.catalog, BUCKET, "secret", b"secret", Some(&KEY)).await;
    put_keyed(&s.state.catalog, BUCKET, "plain", b"plain", None).await;
    let secret = stored_object(&s, "secret").await.unwrap();
    let plain = stored_object(&s, "plain").await.unwrap();

    assert_customer_encryption(
        update(&s, relabel("secret", Some(&OTHER))).await,
        CsekError::KeyIncorrect,
    );
    assert_customer_encryption(
        update(&s, relabel("plain", Some(&KEY))).await,
        CsekError::ResourceNotEncrypted,
    );
    for (before, name) in [(secret, "secret"), (plain, "plain")] {
        let after = stored_object(&s, name).await.unwrap();
        assert_eq!(after.metageneration, before.metageneration, "{name}");
        assert_eq!(after.content_type, before.content_type, "{name}");
    }
    s.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_compose_applies_one_key_to_every_source_and_the_result() {
    let (s, _dir) = setup().await;
    let catalog = &s.state.catalog;
    put_keyed(catalog, BUCKET, "a", b"hello ", Some(&KEY)).await;
    put_keyed(catalog, BUCKET, "b", b"world", Some(&KEY)).await;

    let composed = compose_with(&s, compose(&["a", "b"], Some(&KEY)))
        .await
        .unwrap();
    assert_eq!(composed.customer_encryption, customer_encryption(&KEY));
    // A composite carries crc32c only.
    let checksums = composed.checksums.unwrap();
    assert!(checksums.crc32c.is_some());
    assert!(checksums.md5_hash.is_empty());
    assert_eq!(composed.size, 11);

    // The result is stored under the same key: it can be updated with it, not without.
    let stored = stored_object(&s, "composed").await.unwrap();
    assert_eq!(stored.customer_key_sha256, Some(key_sha256(&KEY)));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_compose_key_failures_create_no_destination() {
    let (s, _dir) = setup().await;
    let catalog = &s.state.catalog;
    put_keyed(catalog, BUCKET, "a", b"hello ", Some(&KEY)).await;
    put_keyed(catalog, BUCKET, "b", b"world", Some(&KEY)).await;
    put_keyed(catalog, BUCKET, "plain", b"plain", None).await;
    put_keyed(catalog, BUCKET, "plain2", b"plain", None).await;

    // Recorded against Cloud Storage over gRPC on 2026-11-15.
    for (label, request, error) in [
        (
            "encrypted sources, no key",
            compose(&["a", "b"], None),
            CsekError::ResourceIsEncrypted,
        ),
        (
            "wrong key",
            compose(&["a", "b"], Some(&OTHER)),
            CsekError::KeyIncorrect,
        ),
        (
            "one unencrypted source",
            compose(&["a", "plain"], Some(&KEY)),
            CsekError::ResourceNotEncrypted,
        ),
        (
            "unencrypted sources, a key",
            compose(&["plain", "plain2"], Some(&KEY)),
            CsekError::ResourceNotEncrypted,
        ),
    ] {
        assert_customer_encryption(compose_with(&s, request).await, error);
        assert!(stored_object(&s, "composed").await.is_none(), "{label}");
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn grpc_compose_deletes_encrypted_sources_with_the_right_key() {
    let (s, _dir) = setup().await;
    let catalog = &s.state.catalog;
    put_keyed(catalog, BUCKET, "a", b"hello ", Some(&KEY)).await;
    put_keyed(catalog, BUCKET, "b", b"world", Some(&KEY)).await;

    // A refused key leaves the sources alone.
    let wrong = pb::ComposeObjectRequest {
        delete_source_objects: Some(true),
        ..compose(&["a", "b"], Some(&OTHER))
    };
    assert_customer_encryption(compose_with(&s, wrong).await, CsekError::KeyIncorrect);
    assert!(stored_object(&s, "a").await.is_some());

    let request = pb::ComposeObjectRequest {
        delete_source_objects: Some(true),
        ..compose(&["a", "b"], Some(&KEY))
    };
    compose_with(&s, request).await.unwrap();
    assert!(stored_object(&s, "composed").await.is_some());
    assert!(stored_object(&s, "a").await.is_none());
    assert!(stored_object(&s, "b").await.is_none());
    catalog.close().await.unwrap();
}
