//! BidiReadObject with customer-supplied keys. Real Cloud Storage refused BidiReadObject for the
//! test project, so these expectations are the local rules that mirror ReadObject.

use super::*;
use crate::csek::CsekError;
use crate::grpc::test_support::{assert_customer_encryption, key_params, put_keyed, rpc_status};
use crate::{auth::Auth, storage::Catalog, test_support::TestDir};
use std::sync::Arc;

const KEY: [u8; 32] = [3; 32];
const OTHER: [u8; 32] = [4; 32];

async fn setup() -> (TestDir, Arc<crate::server::State>) {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(crate::storage::test_fixtures::bucket("bidi-csek", "p"))
        .await
        .unwrap();
    put_keyed(&catalog, "bidi-csek", "secret", b"abc", Some(&KEY)).await;
    put_keyed(&catalog, "bidi-csek", "plain", b"abc", None).await;
    let state = Arc::new(crate::server::State::for_test(catalog, Auth::permissive()));
    (dir, state)
}

fn spec(object: &str, key: Option<&[u8; 32]>) -> pb::BidiReadObjectSpec {
    pb::BidiReadObjectSpec {
        bucket: "projects/_/buckets/bidi-csek".into(),
        object: object.into(),
        common_object_request_params: key.and_then(key_params),
        ..Default::default()
    }
}

/// Runs one BidiReadObject stream over the wire and returns its final status and responses.
async fn open(
    state: &Arc<crate::server::State>,
    spec: pb::BidiReadObjectSpec,
) -> (Status, Vec<pb::BidiReadObjectResponse>) {
    rpc_status(
        "/google.storage.v2.Storage/BidiReadObject",
        None,
        &[pb::BidiReadObjectRequest {
            read_object_spec: Some(spec),
            read_ranges: vec![pb::ReadRange {
                read_id: 1,
                read_offset: 0,
                read_length: 0,
            }],
        }],
        async |request| {
            crate::grpc::v2::dispatch(
                crate::grpc::v2::service(
                    state.clone(),
                    1024 * 1024,
                    crate::config::CompatProfile::Native,
                ),
                request,
            )
            .await
            .unwrap()
        },
    )
    .await
}

fn data(responses: &[pb::BidiReadObjectResponse]) -> Vec<u8> {
    responses
        .iter()
        .flat_map(|response| &response.object_data_ranges)
        .filter_map(|range| range.checksummed_data.as_ref())
        .flat_map(|data| data.content.iter().copied())
        .collect()
}

#[tokio::test]
async fn opening_with_the_right_key_streams_data_and_shows_checksums() {
    let (_dir, state) = setup().await;
    let (status, responses) = open(&state, spec("secret", Some(&KEY))).await;
    assert_eq!(status.code(), tonic::Code::Ok, "{status:?}");
    let metadata = responses.first().unwrap().metadata.as_ref().unwrap();
    assert!(metadata.checksums.is_some());
    assert!(metadata.customer_encryption.is_some());
    assert_eq!(data(&responses), b"abc");
    state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn opening_without_or_with_the_wrong_key_is_refused() {
    let (_dir, state) = setup().await;
    for (object, key, expected) in [
        ("secret", None, CsekError::ResourceIsEncrypted),
        ("secret", Some(&OTHER), CsekError::KeyIncorrect),
        ("plain", Some(&KEY), CsekError::ResourceNotEncrypted),
    ] {
        let (status, responses) = open(&state, spec(object, key)).await;
        assert!(responses.is_empty(), "{object}: no data before the check");
        assert_customer_encryption(Err::<(), _>(status), expected);
    }
    let (status, responses) = open(&state, spec("plain", None)).await;
    assert_eq!(status.code(), tonic::Code::Ok);
    assert_eq!(data(&responses), b"abc");
    state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn a_malformed_key_fails_with_its_parse_error() {
    let (_dir, state) = setup().await;
    let mut request = spec("missing", None);
    request.common_object_request_params = Some(pb::CommonObjectRequestParams {
        encryption_algorithm: "AES128".into(),
        encryption_key_bytes: KEY.to_vec(),
        encryption_key_sha256_bytes: crate::csek::key_sha256(&KEY).to_vec(),
    });
    let (status, _) = open(&state, request).await;
    assert_customer_encryption(Err::<(), _>(status), CsekError::AlgorithmInvalid);
    state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn routing_token_and_read_mask_stay_unimplemented() {
    let (_dir, state) = setup().await;
    let mut request = spec("secret", Some(&KEY));
    request.routing_token = Some("token".into());
    let (status, _) = open(&state, request).await;
    assert_eq!(status.code(), tonic::Code::Unimplemented);
    state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn resuming_needs_the_key_again_and_a_failure_keeps_the_handle() {
    let (_dir, state) = setup().await;
    let (status, responses) = open(&state, spec("secret", Some(&KEY))).await;
    assert_eq!(status.code(), tonic::Code::Ok, "{status:?}");
    let first = responses.first().unwrap();
    let resume = |key| pb::BidiReadObjectSpec {
        generation: first.metadata.as_ref().unwrap().generation,
        read_handle: first.read_handle.clone(),
        ..spec("secret", key)
    };

    // Handles never store a key, so a resume without one fails ...
    let (status, _) = open(&state, resume(None)).await;
    assert_customer_encryption(Err::<(), _>(status), CsekError::ResourceIsEncrypted);
    let (status, _) = open(&state, resume(Some(&OTHER))).await;
    assert_customer_encryption(Err::<(), _>(status), CsekError::KeyIncorrect);

    // ... without invalidating the handle, which still resumes with the right key.
    let (status, responses) = open(&state, resume(Some(&KEY))).await;
    assert_eq!(status.code(), tonic::Code::Ok, "{status:?}");
    assert_eq!(data(&responses), b"abc");
    // The resumed stream carries no object metadata, so no hashes are shown again.
    assert!(responses.first().unwrap().metadata.is_none());
    state.catalog.close().await.unwrap();
}
