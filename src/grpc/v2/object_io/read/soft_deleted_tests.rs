//! GetObject of a soft-deleted generation, which `soft_deleted` and a generation select.

use super::super::super::test_support::{
    OTHER_RESOURCE, RESOURCE, setup, upload, upload_to, without_soft_delete,
};
use super::*;
use crate::grpc::test_support::{assert_bad_request, assert_code};

fn request(bucket: &str, generation: i64, soft_deleted: Option<bool>) -> pb::GetObjectRequest {
    pb::GetObjectRequest {
        bucket: bucket.into(),
        object: "gone".into(),
        generation,
        soft_deleted,
        ..Default::default()
    }
}

async fn get(storage: &Storage, request: pb::GetObjectRequest) -> Result<pb::Object, Status> {
    storage
        .get_object(Request::new(request), None)
        .await
        .map(Response::into_inner)
}

async fn delete(storage: &Storage, bucket: &str) {
    storage
        .delete_object(
            Request::new(pb::DeleteObjectRequest {
                bucket: bucket.into(),
                object: "gone".into(),
                ..Default::default()
            }),
            None,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn get_object_reads_a_soft_deleted_generation() {
    let (storage, _dir) = setup().await;
    let deleted = upload(&storage, "gone", 3).await;
    delete(&storage, RESOURCE).await;

    let object = get(&storage, request(RESOURCE, deleted.generation, Some(true)))
        .await
        .unwrap();
    assert_eq!(object.generation, deleted.generation);
    assert!(object.soft_delete_time.is_some());
    assert!(object.hard_delete_time.is_some());
    // The conditions are compared with the soft-deleted generation.
    let mut failing = request(RESOURCE, deleted.generation, Some(true));
    failing.if_metageneration_match = Some(deleted.metageneration + 1);
    assert_code(
        get(&storage, failing).await,
        tonic::Code::FailedPrecondition,
    );
    // Without `soft_deleted` the generation is gone, and no other generation was soft-deleted.
    for missing in [
        request(RESOURCE, deleted.generation, None),
        request(RESOURCE, deleted.generation, Some(false)),
        request(RESOURCE, deleted.generation + 1, Some(true)),
    ] {
        assert_code(get(&storage, missing).await, tonic::Code::NotFound);
    }
    storage.state.catalog.close().await.unwrap();
}

/// Recorded 2026-11-18 for a deleted generation in a bucket without soft delete.
#[tokio::test]
async fn get_object_soft_deleted_needs_a_soft_delete_policy() {
    const BUCKET: &str = OTHER_RESOURCE;
    let (storage, _dir) = setup().await;
    without_soft_delete(&storage).await;
    let deleted = upload_to(&storage, "update-copy-other", "gone", 3).await;
    delete(&storage, BUCKET).await;
    assert_bad_request(
        get(&storage, request(BUCKET, deleted.generation, Some(true))).await,
        "Soft delete policy is required to request a soft-deleted version.",
    );
    storage.state.catalog.close().await.unwrap();
}
