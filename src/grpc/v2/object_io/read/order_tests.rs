//! The order in which ReadObject reports its failures. Google documents none and none was
//! recorded, so this order is a local choice (SPEC-GRPC-LOCAL-CHOICES): the customer key, the
//! name, the read mask, the generation and the conditions are checked with the request; the
//! caller is authorized when the object is looked up, before its conditions are evaluated; and
//! only then are the read offset and the read limit checked against the object.

use super::super::tests::{message, setup, write};
use super::*;
use crate::grpc::test_support::{assert_code, assert_customer_encryption};

const BUCKET: &str = "projects/_/buckets/grpc-io";
const CONDITIONS: &str =
    "Preconditions must be nonnegative; use if_generation_match=0 to create only";
const GENERATION: &str = "generation must be positive, or 0 to select the live object";
const CONTENT_MASK: &str =
    "The 'read_mask' field must include checksummed_data.content, but was 'metadata.name'.";

fn read_request(object: &str) -> pb::ReadObjectRequest {
    pb::ReadObjectRequest {
        bucket: BUCKET.into(),
        object: object.into(),
        ..Default::default()
    }
}

/// Opens a ReadObject; every failure here comes before the stream starts.
async fn open(
    storage: &Storage,
    request: pb::ReadObjectRequest,
    caller: Option<Caller<'_>>,
) -> Result<(), Status> {
    storage
        .read_object(Request::new(request), caller)
        .await
        .map(drop)
}

#[track_caller]
fn assert_status<T>(result: Result<T, Status>, code: tonic::Code, message: &str) {
    let status = assert_code(result, code);
    assert_eq!(status.message(), message);
}

#[tokio::test]
async fn read_object_reports_each_failure_before_the_ones_it_precedes() {
    let (_dir, storage) = setup(true).await;
    write(&storage, vec![message("object", b"data")])
        .await
        .unwrap();
    let anonymous = Some(Caller::Anonymous);
    let admin = crate::grpc::test_support::caller(&storage, "admin").await;

    // A malformed customer key precedes a malformed name.
    let mut keyed = read_request("");
    keyed.common_object_request_params = Some(pb::CommonObjectRequestParams {
        encryption_algorithm: crate::csek::ALGORITHM.into(),
        encryption_key_bytes: vec![1; 5],
        ..Default::default()
    });
    assert_customer_encryption(
        open(&storage, keyed, anonymous).await,
        crate::csek::CsekError::KeyFormatInvalid,
    );
    // A malformed name precedes the read mask.
    let masked = |object| pb::ReadObjectRequest {
        read_mask: crate::grpc::test_support::mask(&["metadata.name"]),
        generation: -1,
        ..read_request(object)
    };
    let unnamed = assert_code(
        open(&storage, masked(""), anonymous).await,
        tonic::Code::InvalidArgument,
    );
    assert_ne!(unnamed.message(), CONTENT_MASK);
    // The read mask precedes the generation.
    assert_status(
        open(&storage, masked("object"), anonymous).await,
        tonic::Code::InvalidArgument,
        CONTENT_MASK,
    );
    // The generation precedes the conditions, and both precede authorization and the lookup.
    let malformed = || pb::ReadObjectRequest {
        generation: -1,
        if_metageneration_match: Some(-1),
        ..read_request("missing")
    };
    assert_status(
        open(&storage, malformed(), anonymous).await,
        tonic::Code::InvalidArgument,
        GENERATION,
    );
    assert_status(
        open(
            &storage,
            pb::ReadObjectRequest {
                if_metageneration_match: Some(-1),
                ..read_request("missing")
            },
            anonymous,
        )
        .await,
        tonic::Code::InvalidArgument,
        CONDITIONS,
    );
    // Authorization with the lookup precedes the conditions, the offset and the limit, and
    // reveals nothing about a missing object.
    let out_of_range = |object| pb::ReadObjectRequest {
        if_generation_match: Some(1),
        read_offset: 100,
        read_limit: -1,
        ..read_request(object)
    };
    for object in ["object", "missing"] {
        assert_code(
            open(&storage, out_of_range(object), anonymous).await,
            tonic::Code::PermissionDenied,
        );
    }
    // The lookup precedes the conditions, which precede the offset.
    assert_code(
        open(&storage, out_of_range("missing"), admin).await,
        tonic::Code::NotFound,
    );
    assert_code(
        open(&storage, out_of_range("object"), admin).await,
        tonic::Code::FailedPrecondition,
    );
    // The offset precedes the limit.
    assert_code(
        open(
            &storage,
            pb::ReadObjectRequest {
                if_generation_match: None,
                ..out_of_range("object")
            },
            admin,
        )
        .await,
        tonic::Code::OutOfRange,
    );
    storage.state.catalog.close().await.unwrap();
}
