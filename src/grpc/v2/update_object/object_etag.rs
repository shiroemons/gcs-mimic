//! UpdateObject with an etag in the request's object. The pinned v2 proto says of
//! `Object.etag`: "If included in the metadata of an update or delete request message, the
//! operation is only performed if the etag matches that of the live object."

use super::super::test_support::{RESOURCE, setup, stored_object, upload};
use super::*;
use crate::grpc::test_support::{self as fixtures, assert_code, key_params, put_keyed};
use tonic::Code;

const BUCKET: &str = "update-copy-bucket";

/// The message of a failed generation or metageneration condition.
const NOT_HELD: &str = "At least one of the pre-conditions you specified did not hold.";

fn relabel(etag: &str, generation: i64) -> pb::UpdateObjectRequest {
    pb::UpdateObjectRequest {
        object: Some(pb::Object {
            bucket: RESOURCE.into(),
            name: "object".into(),
            generation,
            etag: etag.into(),
            content_language: "fr".into(),
            ..Default::default()
        }),
        update_mask: fixtures::mask(&["content_language"]),
        ..Default::default()
    }
}

async fn update(storage: &Storage, request: pb::UpdateObjectRequest) -> Result<pb::Object, Status> {
    storage
        .update_object(Request::new(request), None)
        .await
        .map(Response::into_inner)
}

/// The etag a client reads with GetObject for one generation of the object.
async fn read_etag(storage: &Storage, generation: i64) -> String {
    storage
        .get_object(
            Request::new(pb::GetObjectRequest {
                bucket: RESOURCE.into(),
                object: "object".into(),
                generation,
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap()
        .into_inner()
        .etag
}

/// The proto names no status for a mismatched etag; a refused update answers as a failed
/// generation or metageneration condition does.
#[track_caller]
fn assert_not_held<T>(result: Result<T, Status>) {
    let status = assert_code(result, Code::FailedPrecondition);
    assert_eq!(status.message(), NOT_HELD);
}

#[tokio::test]
async fn update_object_applies_only_when_the_request_etag_matches() {
    let (storage, _dir) = setup().await;
    let original = upload(&storage, "object", 4).await;
    let current = read_etag(&storage, 0).await;

    // The comparison is exact: a quoted HTTP entity tag is another value.
    for stale in ["CAE=", "not an etag", &format!("\"{current}\"")] {
        assert_not_held(update(&storage, relabel(stale, 0)).await);
    }
    let unchanged = stored_object(&storage, "object").await.unwrap();
    assert_eq!(unchanged.metageneration, original.metageneration);
    assert_eq!(unchanged.content_language, None);

    let updated = update(&storage, relabel(&current, 0)).await.unwrap();
    assert_eq!(updated.content_language, "fr");
    assert_eq!(updated.metageneration, original.metageneration + 1);
    assert_ne!(updated.etag, current);
    // The update advanced the metageneration, so the etag the client read first is now stale.
    assert_not_held(update(&storage, relabel(&current, 0)).await);
    update(&storage, relabel(&updated.etag, 0)).await.unwrap();
    // An empty etag sets no condition.
    update(&storage, relabel("", 0)).await.unwrap();
    storage.state.catalog.close().await.unwrap();
}

/// The proto names the live object without saying what a request that selects another
/// generation compares; like the generation and metageneration conditions, the etag is compared
/// with the generation the request selects.
#[tokio::test]
async fn a_selected_generation_is_compared_with_its_own_etag() {
    let (storage, _dir) = setup().await;
    storage
        .update_bucket(
            Request::new(pb::UpdateBucketRequest {
                bucket: Some(pb::Bucket {
                    name: RESOURCE.into(),
                    versioning: Some(pb::bucket::Versioning { enabled: true }),
                    ..Default::default()
                }),
                update_mask: fixtures::mask(&["versioning"]),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap();
    let noncurrent = upload(&storage, "object", 4).await;
    let live = upload(&storage, "object", 5).await;
    let live_etag = read_etag(&storage, live.generation).await;
    let noncurrent_etag = read_etag(&storage, noncurrent.generation).await;

    assert_not_held(update(&storage, relabel(&live_etag, noncurrent.generation)).await);
    let updated = update(&storage, relabel(&noncurrent_etag, noncurrent.generation))
        .await
        .unwrap();
    assert_eq!(updated.generation, noncurrent.generation);
    assert_eq!(updated.content_language, "fr");
    let unchanged = stored_object(&storage, "object").await.unwrap();
    assert_eq!(
        (unchanged.generation, unchanged.metageneration),
        (live.generation, live.metageneration)
    );
    storage.state.catalog.close().await.unwrap();
}

/// The etag is checked with the request's other conditions: after the object is found and
/// before its customer-supplied key.
#[tokio::test]
async fn the_etag_is_checked_after_the_lookup_and_before_the_customer_key() {
    let (storage, _dir) = setup().await;
    let missing = update(&storage, relabel("CAE=", 0)).await.unwrap_err();
    assert_eq!(missing.code(), Code::NotFound);
    put_keyed(
        &storage.state.catalog,
        BUCKET,
        "object",
        b"abc",
        Some(&[7; 32]),
    )
    .await;
    let mut request = relabel("CAE=", 0);
    request.common_object_request_params = key_params(&[8; 32]);
    assert_not_held(update(&storage, request).await);
    storage.state.catalog.close().await.unwrap();
}
