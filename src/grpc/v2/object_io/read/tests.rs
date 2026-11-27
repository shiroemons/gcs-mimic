//! GetObject and ReadObject with customer-supplied keys. The expectations follow Cloud Storage's
//! answers recorded over gRPC on 2026-11-15.

use super::super::tests::setup;
use super::test_support::{body, read};
use super::*;
use crate::csek::CsekError;
use crate::grpc::test_support::{assert_customer_encryption, key_params, mask, put_keyed};
use crate::object::ObjectKey;
use crate::storage::test_fixtures::live;
use crate::{bucket::BucketAccess, storage::ResumableOptions};

const BUCKET: &str = "projects/_/buckets/grpc-io";
const KEY: [u8; 32] = [3; 32];
const OTHER: [u8; 32] = [4; 32];

#[tokio::test]
async fn read_messages_stay_within_256_kib_and_only_the_first_has_metadata() {
    let (_dir, storage) = setup(false).await;
    for size in [0, 256 * 1024, 1024 * 1024 + 1] {
        let bytes = vec![42; size];
        put_keyed(&storage.state.catalog, "grpc-io", "frames", &bytes, None).await;
        let messages = read(&storage, read_request("frames", None)).await.unwrap();
        let (first, rest) = messages.split_first().unwrap();
        assert_eq!(first.metadata.as_ref().unwrap().size, size as i64);
        assert_eq!(
            first.checksummed_data.as_ref().unwrap().content.is_empty(),
            size == 0
        );
        assert_eq!(rest.is_empty(), size <= 256 * 1024);
        let (last, full) = messages.split_last().unwrap();
        assert!(full.iter().all(|message| {
            message.checksummed_data.as_ref().unwrap().content.len() == 256 * 1024
        }));
        let tail = last.checksummed_data.as_ref().unwrap().content.len();
        assert!(tail <= 256 * 1024);
        assert_eq!(tail == 0, size == 0);
        assert!(
            rest.iter()
                .all(|message| message.metadata.is_none() && message.object_checksums.is_none())
        );
        assert_eq!(body(&messages), bytes);
    }
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn dropping_a_partial_read_releases_the_generation_immediately() {
    let dir = crate::test_support::TestDir::new();
    let path = dir.0.join("blob");
    let size = 2 * 1024 * 1024;
    std::fs::write(&path, vec![7; size]).unwrap();
    let file = std::sync::Arc::new(std::fs::File::open(path).unwrap());
    let descriptor = std::sync::Arc::downgrade(&file);
    let reader = crate::storage::ObjectReader::bounded_for_test(file, size as u64);
    let mut stream = ReadObjectStream {
        frames: reader.into(),
        first: Some(Default::default()),
        checksum: DataChecksum::Omitted,
    };
    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(first.checksummed_data.unwrap().content.len(), 256 * 1024);
    drop(stream);
    assert!(descriptor.upgrade().is_none());
}

#[tokio::test]
async fn a_panicking_read_resets_its_http2_stream() {
    use tonic::codec::{Codec, EncodeBody};
    const PANIC: &str = "injected ReadObject poll panic";
    let stream = ReadObjectStream {
        frames: ObjectFrames::panicking_read(PANIC),
        first: Some(Default::default()),
        checksum: DataChecksum::Sent,
    };
    let encoder =
        tonic_prost::ProstCodec::<pb::ReadObjectResponse, pb::ReadObjectRequest>::new().encoder();
    let body = EncodeBody::new_server(encoder, stream, None, Default::default(), None);
    crate::grpc::tests::assert_body_panic_resets_stream(tonic::body::Body::new(body), PANIC).await;
}

async fn get(
    storage: &Storage,
    object: &str,
    key: Option<&[u8; 32]>,
    paths: &[&str],
) -> Result<pb::Object, Status> {
    storage
        .get_object(
            Request::new(pb::GetObjectRequest {
                bucket: BUCKET.into(),
                object: object.into(),
                common_object_request_params: key.and_then(key_params),
                read_mask: (!paths.is_empty()).then(|| mask(paths)).flatten(),
                ..Default::default()
            }),
            None,
        )
        .await
        .map(Response::into_inner)
}

fn read_request(object: &str, key: Option<&[u8; 32]>) -> pb::ReadObjectRequest {
    pb::ReadObjectRequest {
        bucket: BUCKET.into(),
        object: object.into(),
        common_object_request_params: key.and_then(key_params),
        ..Default::default()
    }
}

fn fingerprint(key: &[u8; 32]) -> Option<pb::CustomerEncryption> {
    Some(pb::CustomerEncryption {
        encryption_algorithm: crate::csek::ALGORITHM.into(),
        key_sha256_bytes: crate::csek::key_sha256(key).to_vec(),
    })
}

#[tokio::test]
async fn get_object_without_a_key_shows_the_fingerprint_but_no_checksums() {
    let (_dir, storage) = setup(false).await;
    put_keyed(
        &storage.state.catalog,
        "grpc-io",
        "secret",
        b"abc",
        Some(&KEY),
    )
    .await;

    for paths in [&[][..], &["checksums", "customer_encryption"][..]] {
        let object = get(&storage, "secret", None, paths).await.unwrap();
        assert_eq!(object.customer_encryption, fingerprint(&KEY));
        assert!(object.checksums.is_none(), "{paths:?}");
    }
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn get_object_with_the_right_key_shows_checksums() {
    let (_dir, storage) = setup(false).await;
    let record = put_keyed(
        &storage.state.catalog,
        "grpc-io",
        "secret",
        b"abc",
        Some(&KEY),
    )
    .await;

    for paths in [&[][..], &["checksums"][..]] {
        let object = get(&storage, "secret", Some(&KEY), paths).await.unwrap();
        let checksums = object.checksums.unwrap();
        assert_eq!(checksums.crc32c, Some(record.crc32c), "{paths:?}");
        assert!(!checksums.md5_hash.is_empty(), "{paths:?}");
    }
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn get_object_refuses_a_wrong_key_and_a_key_on_an_unencrypted_object() {
    let (_dir, storage) = setup(false).await;
    let catalog = &storage.state.catalog;
    put_keyed(catalog, "grpc-io", "secret", b"abc", Some(&KEY)).await;
    put_keyed(catalog, "grpc-io", "plain", b"abc", None).await;

    assert_customer_encryption(
        get(&storage, "secret", Some(&OTHER), &[]).await,
        CsekError::KeyIncorrect,
    );
    assert_customer_encryption(
        get(&storage, "plain", Some(&KEY), &[]).await,
        CsekError::ResourceNotEncrypted,
    );
    assert!(get(&storage, "plain", None, &[]).await.is_ok());
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_malformed_key_fails_before_the_object_is_looked_up() {
    let (_dir, storage) = setup(false).await;
    let params = Some(pb::CommonObjectRequestParams {
        encryption_algorithm: crate::csek::ALGORITHM.into(),
        encryption_key_bytes: vec![1; 5],
        ..Default::default()
    });
    let result = storage
        .get_object(
            Request::new(pb::GetObjectRequest {
                bucket: BUCKET.into(),
                object: "missing".into(),
                common_object_request_params: params.clone(),
                ..Default::default()
            }),
            None,
        )
        .await;
    assert_customer_encryption(result, CsekError::KeyFormatInvalid);
    let mut request = read_request("missing", None);
    request.common_object_request_params = params;
    assert_customer_encryption(read(&storage, request).await, CsekError::KeyFormatInvalid);
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn read_object_returns_data_and_checksums_with_the_right_key() {
    let (_dir, storage) = setup(false).await;
    let record = put_keyed(
        &storage.state.catalog,
        "grpc-io",
        "secret",
        b"abcdef",
        Some(&KEY),
    )
    .await;

    let messages = read(&storage, read_request("secret", Some(&KEY)))
        .await
        .unwrap();
    assert_eq!(body(&messages), b"abcdef");
    let first = messages.first().unwrap();
    let from_object = first.object_checksums.as_ref().unwrap();
    let from_metadata = first
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.checksums.as_ref())
        .unwrap();
    assert_eq!(from_object.crc32c, Some(record.crc32c));
    assert!(!from_object.md5_hash.is_empty());
    assert_eq!(from_object, from_metadata);

    let mut ranged = read_request("secret", Some(&KEY));
    ranged.read_offset = 2;
    ranged.read_limit = 3;
    let messages = read(&storage, ranged).await.unwrap();
    assert_eq!(body(&messages), b"cde");
    let first = messages.first().unwrap();
    assert!(first.object_checksums.is_some());
    assert!(first.metadata.as_ref().unwrap().checksums.is_some());
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn read_object_refuses_missing_and_wrong_keys_before_sending_data() {
    let (_dir, storage) = setup(false).await;
    let catalog = &storage.state.catalog;
    put_keyed(catalog, "grpc-io", "secret", b"abc", Some(&KEY)).await;
    put_keyed(catalog, "grpc-io", "plain", b"abc", None).await;

    assert_customer_encryption(
        read(&storage, read_request("secret", None)).await,
        CsekError::ResourceIsEncrypted,
    );
    assert_customer_encryption(
        read(&storage, read_request("secret", Some(&OTHER))).await,
        CsekError::KeyIncorrect,
    );
    assert_customer_encryption(
        read(&storage, read_request("plain", Some(&KEY))).await,
        CsekError::ResourceNotEncrypted,
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn an_older_generation_is_read_with_its_own_key() {
    let (_dir, storage) = setup(false).await;
    let catalog = &storage.state.catalog;
    let old = put_keyed(catalog, "grpc-io", "rotated", b"old", Some(&KEY)).await;
    put_keyed(catalog, "grpc-io", "rotated", b"new", Some(&OTHER)).await;

    let mut request = read_request("rotated", Some(&KEY));
    request.generation = old.generation.get();
    let messages = read(&storage, request).await.unwrap();
    assert_eq!(body(&messages), b"old");

    // The live generation answers to the new key only.
    assert_customer_encryption(
        read(&storage, read_request("rotated", Some(&KEY))).await,
        CsekError::KeyIncorrect,
    );
    assert_eq!(
        body(
            &read(&storage, read_request("rotated", Some(&OTHER)))
                .await
                .unwrap()
        ),
        b"new"
    );
    let mut request = read_request("rotated", Some(&OTHER));
    request.generation = old.generation.get();
    assert_customer_encryption(read(&storage, request).await, CsekError::KeyIncorrect);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_key_on_an_unfinalized_appendable_object_is_refused() {
    let (_dir, storage) = setup(false).await;
    let catalog = &storage.state.catalog;
    let mut writer = catalog
        .create_appendable(
            ObjectKey::from_parts_unchecked("grpc-io", "growing"),
            ResumableOptions::new(BucketAccess::Permissive),
        )
        .await
        .unwrap();
    writer
        .append(0, bytes::Bytes::from_static(b"abc"), 1024)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    assert!(
        catalog
            .get_readable(
                live("grpc-io", "growing"),
                Default::default(),
                crate::storage::test_fixtures::PERMISSIVE,
                None
            )
            .await
            .is_ok()
    );

    assert_eq!(
        body(&read(&storage, read_request("growing", None)).await.unwrap()),
        b"abc"
    );
    assert_customer_encryption(
        read(&storage, read_request("growing", Some(&KEY))).await,
        CsekError::ResourceNotEncrypted,
    );
    assert_customer_encryption(
        get(&storage, "growing", Some(&KEY), &[]).await,
        CsekError::ResourceNotEncrypted,
    );
    drop(writer);
    catalog.close().await.unwrap();
}
