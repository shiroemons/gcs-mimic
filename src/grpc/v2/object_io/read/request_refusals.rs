//! GetObject, ReadObject and ListObjects requests refused before any object is returned.
//! Malformed fields answer INVALID_ARGUMENT or OUT_OF_RANGE; fields naming semantics gcs-mimic
//! does not model (an explicit soft-deleted lookup, a restore token, a listing filter) answer
//! UNIMPLEMENTED with ErrorInfo.

use super::super::tests::{message, setup, write};
use super::*;
use crate::grpc::test_support::{assert_code, assert_unsupported};

const BUCKET: &str = "projects/_/buckets/grpc-io";
const CONDITIONS: &str =
    "Preconditions must be nonnegative; use if_generation_match=0 to create only";
const GENERATION: &str = "generation must be positive, or 0 to select the live object";

#[track_caller]
fn assert_status<T>(result: Result<T, Status>, code: tonic::Code, message: &str) {
    let status = assert_code(result, code);
    assert_eq!(status.message(), message);
}

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
async fn get_object_refuses_unmodeled_lookups_and_negative_values() {
    let (_dir, storage) = setup(false).await;
    write(&storage, vec![message("object", b"data")])
        .await
        .unwrap();
    let get = async |request| storage.get_object(Request::new(request), None).await;
    for request in [
        pb::GetObjectRequest {
            soft_deleted: Some(true),
            ..get_request()
        },
        pb::GetObjectRequest {
            restore_token: "token".into(),
            ..get_request()
        },
    ] {
        assert_unsupported(get(request).await, "GetObject soft_deleted/restore_token");
    }
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
    assert_status(
        get(pb::GetObjectRequest {
            if_generation_not_match: Some(-1),
            ..get_request()
        })
        .await,
        tonic::Code::InvalidArgument,
        CONDITIONS,
    );
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
        tonic::Code::InvalidArgument,
        CONDITIONS,
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
