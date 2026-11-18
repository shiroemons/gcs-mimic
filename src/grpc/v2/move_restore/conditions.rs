//! MoveObject and RestoreObject conditions as Cloud Storage answered them (recorded 2026-11-18,
//! case grpc-probe-object-conditions).

use super::super::test_support::{
    OTHER_RESOURCE, RESOURCE, setup, stored_object, upload, upload_to, without_soft_delete,
};
use super::*;
use crate::grpc::test_support::{assert_bad_request, assert_code};
use tonic::Code;

fn move_request() -> pb::MoveObjectRequest {
    pb::MoveObjectRequest {
        bucket: RESOURCE.into(),
        source_object: "source".into(),
        destination_object: "destination".into(),
        ..Default::default()
    }
}

/// MoveObject compares a metageneration match of 0 and, unlike the other object RPCs, refuses a
/// match beside a not-match, for the destination and for the source.
#[tokio::test]
async fn move_object_refuses_condition_pairs_and_failing_conditions_without_moving() {
    const CONFLICT: &str = "Found conflicting object preconditions. Mutually exclusive fields cannot be set simultaneously.";
    let (s, _dir) = setup().await;
    let source = upload(&s, "source", 4).await;
    let destination = upload(&s, "destination", 1).await;
    let move_with = async |request| s.move_object(Request::new(request), None).await;

    for zero in [
        pb::MoveObjectRequest {
            if_metageneration_match: Some(0),
            ..move_request()
        },
        pb::MoveObjectRequest {
            if_source_metageneration_match: Some(0),
            ..move_request()
        },
    ] {
        let status = assert_code(move_with(zero).await, Code::FailedPrecondition);
        assert_eq!(
            status.message(),
            "At least one of the pre-conditions you specified did not hold."
        );
    }
    for pair in [
        pb::MoveObjectRequest {
            if_generation_match: Some(destination.generation),
            if_generation_not_match: Some(destination.generation),
            ..move_request()
        },
        pb::MoveObjectRequest {
            if_metageneration_match: Some(1),
            if_metageneration_not_match: Some(1),
            ..move_request()
        },
        pb::MoveObjectRequest {
            if_source_generation_match: Some(source.generation),
            if_source_generation_not_match: Some(source.generation),
            ..move_request()
        },
        pb::MoveObjectRequest {
            if_source_metageneration_match: Some(1),
            if_source_metageneration_not_match: Some(1),
            ..move_request()
        },
        // The pair is refused for its shape, whatever its values: pairs whose conditions both
        // hold were refused too (case grpc-probe-inferred-conditions).
        pb::MoveObjectRequest {
            if_source_generation_match: Some(source.generation),
            if_source_generation_not_match: Some(source.generation + 1),
            ..move_request()
        },
    ] {
        assert_bad_request(move_with(pair).await, CONFLICT);
    }
    // A negative value is compared like any other, so a match of -1 never holds (case
    // grpc-probe-inferred-conditions, which recorded it on the other object RPCs).
    let negative = assert_code(
        move_with(pb::MoveObjectRequest {
            if_source_generation_match: Some(-1),
            ..move_request()
        })
        .await,
        Code::FailedPrecondition,
    );
    assert_eq!(
        negative.message(),
        "At least one of the pre-conditions you specified did not hold."
    );
    for (name, original) in [("source", source), ("destination", destination)] {
        let stored = stored_object(&s, name).await.unwrap();
        assert_eq!(stored.generation, original.generation, "{name}");
    }
    s.state.catalog.close().await.unwrap();
}

/// In a bucket without a soft delete policy RestoreObject answers that refusal, with a
/// BadRequest, whatever conditions the request carries.
#[tokio::test]
async fn restore_object_reports_a_missing_soft_delete_policy_before_its_conditions() {
    const BUCKET: &str = OTHER_RESOURCE;
    let (s, _dir) = setup().await;
    without_soft_delete(&s).await;
    let deleted = upload_to(&s, "update-copy-other", "source", 1).await;
    s.delete_object(
        Request::new(pb::DeleteObjectRequest {
            bucket: BUCKET.into(),
            object: "source".into(),
            ..Default::default()
        }),
        None,
    )
    .await
    .unwrap();
    let restore = pb::RestoreObjectRequest {
        bucket: BUCKET.into(),
        object: "source".into(),
        generation: deleted.generation,
        ..Default::default()
    };
    for request in [
        restore.clone(),
        pb::RestoreObjectRequest {
            if_metageneration_match: Some(0),
            ..restore.clone()
        },
        pb::RestoreObjectRequest {
            if_generation_match: Some(0),
            if_generation_not_match: Some(0),
            ..restore.clone()
        },
        pb::RestoreObjectRequest {
            if_metageneration_match: Some(1),
            if_metageneration_not_match: Some(1),
            ..restore
        },
    ] {
        assert_bad_request(
            s.restore_object(Request::new(request), None).await,
            "bucket soft delete policy must be set.",
        );
    }
    s.state.catalog.close().await.unwrap();
}

/// With a soft delete policy the conditions are compared with the live object the restore
/// would replace (inferred from the other object RPCs; no such restore was recorded).
#[tokio::test]
async fn restore_object_compares_its_conditions_with_the_live_object() {
    let (s, _dir) = setup().await;
    let deleted = upload(&s, "source", 4).await;
    s.delete_object(
        Request::new(pb::DeleteObjectRequest {
            bucket: RESOURCE.into(),
            object: "source".into(),
            ..Default::default()
        }),
        None,
    )
    .await
    .unwrap();
    let restore = pb::RestoreObjectRequest {
        bucket: RESOURCE.into(),
        object: "source".into(),
        generation: deleted.generation,
        ..Default::default()
    };
    assert_code(
        s.restore_object(
            Request::new(pb::RestoreObjectRequest {
                if_metageneration_match: Some(0),
                ..restore.clone()
            }),
            None,
        )
        .await,
        Code::FailedPrecondition,
    );
    assert!(stored_object(&s, "source").await.is_none());
    let restored = s
        .restore_object(
            Request::new(pb::RestoreObjectRequest {
                if_generation_match: Some(0),
                ..restore
            }),
            None,
        )
        .await
        .unwrap()
        .into_inner();
    assert_eq!(restored.size, 4);
    s.state.catalog.close().await.unwrap();
}
