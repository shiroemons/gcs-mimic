//! Requests that UpdateObject, ComposeObject, RewriteObject and RestoreObject refuse before they
//! change anything: malformed fields answer INVALID_ARGUMENT, and fields naming semantics
//! gcs-mimic does not model answer UNIMPLEMENTED with ErrorInfo. Each refusal leaves the stored
//! objects as they were.

use super::tests::{RESOURCE, setup, stored_object, upload};
use super::*;
use crate::grpc::test_support::{self as fixtures, assert_code, assert_unsupported};
use tonic::Code;

const CONDITIONS: &str = "Invalid generation conditions; use one match or not-match per generation, for example if_generation_match=0";

#[track_caller]
fn assert_invalid<T>(result: Result<T, Status>, message: &str) {
    let status = assert_code(result, Code::InvalidArgument);
    assert_eq!(status.message(), message);
}

fn relabel() -> pb::UpdateObjectRequest {
    pb::UpdateObjectRequest {
        object: Some(pb::Object {
            bucket: RESOURCE.into(),
            name: "object".into(),
            content_language: "fr".into(),
            ..Default::default()
        }),
        update_mask: fixtures::mask(&["content_language"]),
        ..Default::default()
    }
}

/// `request` with its object changed by `change`.
fn with_object(
    mut request: pb::UpdateObjectRequest,
    change: impl FnOnce(&mut pb::Object),
) -> pb::UpdateObjectRequest {
    if let Some(object) = request.object.as_mut() {
        change(object);
    }
    request
}

#[tokio::test]
async fn update_object_refuses_malformed_and_unmodeled_requests_without_changing_the_object() {
    let (s, _dir) = setup().await;
    let original = upload(&s, "object", 4).await;
    let update = async |request| s.update_object(Request::new(request), None).await;

    assert_invalid(
        update(pb::UpdateObjectRequest {
            object: None,
            ..relabel()
        })
        .await,
        "object is required; supply object.bucket and object.name",
    );
    assert_invalid(
        update(with_object(relabel(), |object| object.generation = -1)).await,
        "generation must be positive or omitted",
    );
    assert_invalid(
        update(pb::UpdateObjectRequest {
            update_mask: None,
            ..relabel()
        })
        .await,
        "update_mask is required; use paths such as content_type or versioning.enabled",
    );
    assert_invalid(
        update(pb::UpdateObjectRequest {
            if_generation_match: Some(original.generation),
            if_generation_not_match: Some(1),
            ..relabel()
        })
        .await,
        CONDITIONS,
    );
    let retention = |mode: pb::object::retention::Mode, time| {
        with_object(
            pb::UpdateObjectRequest {
                update_mask: fixtures::mask(&["retention"]),
                ..relabel()
            },
            |object| {
                object.retention = Some(pb::object::Retention {
                    mode: mode as i32,
                    retain_until_time: time,
                });
            },
        )
    };
    assert_invalid(
        update(retention(pb::object::retention::Mode::Unspecified, None)).await,
        "retention.mode must be UNLOCKED or LOCKED",
    );
    assert_invalid(
        update(retention(pb::object::retention::Mode::Unlocked, None)).await,
        "retention.retain_until_time is required with retention.mode",
    );
    // The proto's `*` stands for the JSON API's update. Cloud Storage served it (case
    // grpc-unsupported-options, recorded 2026-11-13); gcs-mimic still refuses it, as it refuses
    // object contexts, which it does not model, and a storage class, which only a rewrite changes.
    for path in ["*", "contexts", "contexts.custom", "storage_class"] {
        assert_unsupported(
            update(pb::UpdateObjectRequest {
                update_mask: fixtures::mask(&[path]),
                ..relabel()
            })
            .await,
            &format!("UpdateObject update_mask {path}"),
        );
    }
    let stored = stored_object(&s, "object").await.unwrap();
    assert_eq!(stored.metageneration, original.metageneration);
    assert_eq!(stored.content_language, None);
    s.state.catalog.close().await.unwrap();
}

fn compose() -> pb::ComposeObjectRequest {
    pb::ComposeObjectRequest {
        destination: Some(pb::Object {
            bucket: RESOURCE.into(),
            name: "composed".into(),
            ..Default::default()
        }),
        source_objects: vec![pb::compose_object_request::SourceObject {
            name: "source".into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

#[tokio::test]
async fn compose_object_refuses_malformed_and_unmodeled_requests_without_creating_the_destination()
{
    let (s, _dir) = setup().await;
    upload(&s, "source", 4).await;
    let compose_with = async |request| s.compose_object(Request::new(request), None).await;

    assert_invalid(
        compose_with(pb::ComposeObjectRequest {
            destination: None,
            ..compose()
        })
        .await,
        "destination is required; supply bucket and name",
    );
    let mut negative = compose();
    negative.source_objects[0].generation = -1;
    assert_invalid(
        compose_with(negative).await,
        "source generation must be positive or omitted",
    );
    let mut conditioned = compose();
    conditioned.source_objects[0].object_preconditions = Some(
        pb::compose_object_request::source_object::ObjectPreconditions {
            if_generation_match: Some(-1),
        },
    );
    assert_invalid(compose_with(conditioned).await, CONDITIONS);
    // Destination fields outside the editable metadata, such as an object retention, a KMS key
    // or an explicit ACL, are refused rather than dropped.
    let unmodeled: [fn(&mut pb::Object); 3] = [
        |object| {
            object.retention = Some(pb::object::Retention {
                mode: pb::object::retention::Mode::Unlocked as i32,
                retain_until_time: Some(prost_types::Timestamp {
                    seconds: 4_000_000_000,
                    nanos: 0,
                }),
            });
        },
        |object| object.kms_key = "projects/p/locations/l/keyRings/r/cryptoKeys/k".into(),
        |object| {
            object.acl = vec![pb::ObjectAccessControl {
                entity: "allUsers".into(),
                role: "READER".into(),
                ..Default::default()
            }];
        },
    ];
    for change in unmodeled {
        let mut request = compose();
        if let Some(destination) = request.destination.as_mut() {
            change(destination);
        }
        assert_unsupported(compose_with(request).await, "derived object fields");
    }
    assert!(stored_object(&s, "composed").await.is_none());
    s.state.catalog.close().await.unwrap();
}

fn rewrite() -> pb::RewriteObjectRequest {
    pb::RewriteObjectRequest {
        source_bucket: RESOURCE.into(),
        source_object: "source".into(),
        destination_bucket: RESOURCE.into(),
        destination_name: "rewritten".into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn rewrite_object_refuses_malformed_and_unmodeled_requests_without_creating_the_destination()
{
    let (s, _dir) = setup().await;
    upload(&s, "source", 4).await;
    let rewrite_with = async |request| s.rewrite_object(Request::new(request), None).await;
    let options = "source_generation must be positive or omitted; max_bytes_rewritten_per_call must be a multiple of 1048576";

    assert_invalid(
        rewrite_with(pb::RewriteObjectRequest {
            source_generation: -1,
            ..rewrite()
        })
        .await,
        options,
    );
    for bytes in [-1_048_576, 1000] {
        assert_invalid(
            rewrite_with(pb::RewriteObjectRequest {
                max_bytes_rewritten_per_call: bytes,
                ..rewrite()
            })
            .await,
            options,
        );
    }
    for (bucket, name) in [(RESOURCE, ""), ("", "rewritten")] {
        assert_invalid(
            rewrite_with(pb::RewriteObjectRequest {
                destination: Some(pb::Object {
                    bucket: bucket.into(),
                    name: name.into(),
                    ..Default::default()
                }),
                ..rewrite()
            })
            .await,
            "Rewrite destination metadata must omit name and bucket; use destination_name and destination_bucket",
        );
    }
    assert_unsupported(
        rewrite_with(pb::RewriteObjectRequest {
            destination: Some(pb::Object {
                kms_key: "projects/p/locations/l/keyRings/r/cryptoKeys/k".into(),
                ..Default::default()
            }),
            ..rewrite()
        })
        .await,
        "derived object fields",
    );
    assert_invalid(
        rewrite_with(pb::RewriteObjectRequest {
            rewrite_token: "not a token".into(),
            ..rewrite()
        })
        .await,
        "Invalid rewrite token; use the token from the preceding response",
    );
    assert_invalid(
        rewrite_with(pb::RewriteObjectRequest {
            if_source_metageneration_match: Some(1),
            if_source_metageneration_not_match: Some(2),
            ..rewrite()
        })
        .await,
        CONDITIONS,
    );
    assert!(stored_object(&s, "rewritten").await.is_none());
    s.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn restore_object_refuses_a_missing_generation_and_copying_the_source_acl() {
    let (s, _dir) = setup().await;
    let source = upload(&s, "source", 4).await;
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
        generation: source.generation,
        ..Default::default()
    };
    for generation in [0, -1] {
        assert_invalid(
            s.restore_object(
                Request::new(pb::RestoreObjectRequest {
                    generation,
                    ..restore.clone()
                }),
                None,
            )
            .await,
            "generation is required; select a positive soft-deleted generation",
        );
    }
    // Restored objects take the bucket's default ACL; copying the soft-deleted generation's ACL
    // is not modeled.
    assert_unsupported(
        s.restore_object(
            Request::new(pb::RestoreObjectRequest {
                copy_source_acl: Some(true),
                ..restore.clone()
            }),
            None,
        )
        .await,
        "RestoreObject ACL/token",
    );
    assert!(stored_object(&s, "source").await.is_none());
    let restored = s
        .restore_object(
            Request::new(pb::RestoreObjectRequest {
                copy_source_acl: Some(false),
                ..restore
            }),
            None,
        )
        .await
        .unwrap()
        .into_inner();
    // A restore publishes the soft-deleted bytes as a new live generation.
    assert_eq!(restored.size, 4);
    let live = stored_object(&s, "source").await.unwrap();
    assert_eq!(live.generation, restored.generation);
    s.state.catalog.close().await.unwrap();
}
