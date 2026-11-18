//! GetObject, ReadObject and ListObjects requests refused before any object is returned.
//! Malformed fields answer INVALID_ARGUMENT or OUT_OF_RANGE; a restore token answers INTERNAL, as
//! Cloud Storage does; a field naming semantics gcs-mimic does not model (a listing filter)
//! answers UNIMPLEMENTED with ErrorInfo.

use super::super::tests::{message, setup, write};
use super::test_support::assert_status;
use super::*;
use crate::grpc::test_support::assert_unsupported;

const BUCKET: &str = "projects/_/buckets/grpc-io";
/// A match condition that does not hold; a negative value is compared like any other (case
/// grpc-probe-inferred-conditions).
const NOT_HELD: &str = "At least one of the pre-conditions you specified did not hold.";
const GENERATION: &str = "generation must be positive, or 0 to select the live object";

fn get_request() -> pb::GetObjectRequest {
    pb::GetObjectRequest {
        bucket: BUCKET.into(),
        object: "object".into(),
        ..Default::default()
    }
}

fn read_request() -> pb::ReadObjectRequest {
    pb::ReadObjectRequest {
        bucket: BUCKET.into(),
        object: "object".into(),
        ..Default::default()
    }
}

/// Opens a ReadObject; refusals here all come before the stream starts.
async fn open(storage: &Storage, request: pb::ReadObjectRequest) -> Result<(), Status> {
    storage
        .read_object(Request::new(request), None)
        .await
        .map(drop)
}

#[tokio::test]
async fn get_object_refuses_restore_tokens_and_negative_values() {
    let (_dir, storage) = setup(false).await;
    write(&storage, vec![message("object", b"data")])
        .await
        .unwrap();
    let get = async |request| storage.get_object(Request::new(request), None).await;
    // Recorded 2026-11-18 for a live object in a bucket without a hierarchical namespace.
    assert_status(
        get(pb::GetObjectRequest {
            restore_token: "token".into(),
            ..get_request()
        })
        .await,
        tonic::Code::Internal,
        "We encountered an internal error. Please try again.",
    );
    // A soft-deleted lookup names the generation it wants.
    assert_status(
        get(pb::GetObjectRequest {
            soft_deleted: Some(true),
            ..get_request()
        })
        .await,
        tonic::Code::InvalidArgument,
        "soft_deleted=true requires generation; select the soft-deleted generation",
    );
    // `soft_deleted: false` is the default lookup.
    get(pb::GetObjectRequest {
        soft_deleted: Some(false),
        ..get_request()
    })
    .await
    .unwrap();
    assert_status(
        get(pb::GetObjectRequest {
            generation: -1,
            ..get_request()
        })
        .await,
        tonic::Code::InvalidArgument,
        GENERATION,
    );
    // A negative condition is evaluated: a not-match of -1 always holds (case
    // grpc-probe-inferred-conditions).
    get(pb::GetObjectRequest {
        if_generation_not_match: Some(-1),
        ..get_request()
    })
    .await
    .unwrap();
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn read_object_refuses_negative_limits_and_offsets_past_the_end() {
    let (_dir, storage) = setup(false).await;
    let written = write(&storage, vec![message("object", b"data")])
        .await
        .unwrap();
    let pb::bidi_write_object_response::WriteStatus::Resource(object) = written else {
        panic!("the write returned no resource: {written:?}");
    };
    assert_status(
        open(
            &storage,
            pb::ReadObjectRequest {
                read_limit: -1,
                ..read_request()
            },
        )
        .await,
        tonic::Code::InvalidArgument,
        "read_limit must be nonnegative; use 0 to read to the end",
    );
    assert_status(
        open(
            &storage,
            pb::ReadObjectRequest {
                read_offset: 5,
                ..read_request()
            },
        )
        .await,
        tonic::Code::OutOfRange,
        &format!(
            "Invalid read offset '5' for object 'projects/_/buckets/grpc-io/object#{}'. The read offset must not be greater than the object's size (4 bytes).",
            object.generation
        ),
    );
    assert_status(
        open(
            &storage,
            pb::ReadObjectRequest {
                generation: -1,
                ..read_request()
            },
        )
        .await,
        tonic::Code::InvalidArgument,
        GENERATION,
    );
    assert_status(
        open(
            &storage,
            pb::ReadObjectRequest {
                if_metageneration_match: Some(-1),
                ..read_request()
            },
        )
        .await,
        tonic::Code::FailedPrecondition,
        NOT_HELD,
    );
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn list_objects_refuses_a_filter_and_a_negative_page_size() {
    let (_dir, storage) = setup(false).await;
    let list = async |request| storage.list_objects(Request::new(request), None).await;
    let base = pb::ListObjectsRequest {
        parent: BUCKET.into(),
        ..Default::default()
    };
    assert_unsupported(
        list(pb::ListObjectsRequest {
            filter: "contexts.\"k\"=\"v\"".into(),
            ..base.clone()
        })
        .await,
        "ListObjects filter",
    );
    assert_status(
        list(pb::ListObjectsRequest {
            page_size: -1,
            ..base
        })
        .await,
        tonic::Code::InvalidArgument,
        "page_size must be nonnegative; use 1000",
    );
    storage.state.catalog.close().await.unwrap();
}

/// A write of a new object with this condition set.
fn conditioned_write(set: impl FnOnce(&mut pb::WriteObjectSpec)) -> pb::BidiWriteObjectRequest {
    let mut opening = message("created", b"x");
    let Some(pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec)) =
        opening.first_message.as_mut()
    else {
        unreachable!("message opens with a write spec")
    };
    set(spec);
    opening
}

/// WriteObject and DeleteObject compare a negative condition as any other value: a match of -1
/// never holds, and a not-match of -1 holds for a delete but not for a write of a new object
/// (case grpc-probe-inferred-conditions).
#[tokio::test]
async fn write_and_delete_object_compare_negative_conditions() {
    let (_dir, storage) = setup(false).await;
    write(&storage, vec![message("object", b"data")])
        .await
        .unwrap();
    for opening in [
        conditioned_write(|spec| spec.if_generation_match = Some(-1)),
        conditioned_write(|spec| spec.if_metageneration_match = Some(-1)),
        conditioned_write(|spec| spec.if_generation_not_match = Some(-1)),
    ] {
        assert_status(
            write(&storage, vec![opening]).await,
            tonic::Code::FailedPrecondition,
            NOT_HELD,
        );
    }
    let request = || pb::DeleteObjectRequest {
        bucket: BUCKET.into(),
        object: "object".into(),
        ..Default::default()
    };
    let delete = async |request| storage.delete_object(Request::new(request), None).await;
    assert_status(
        delete(pb::DeleteObjectRequest {
            if_generation_match: Some(-1),
            ..request()
        })
        .await,
        tonic::Code::FailedPrecondition,
        NOT_HELD,
    );
    delete(pb::DeleteObjectRequest {
        if_generation_not_match: Some(-1),
        ..request()
    })
    .await
    .unwrap();
    storage.state.catalog.close().await.unwrap();
}
