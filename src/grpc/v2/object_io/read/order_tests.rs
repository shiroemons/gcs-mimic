//! The order in which ReadObject reports its failures. Google documents none and none was
//! recorded, so this order is a local choice (SPEC-GRPC-LOCAL-CHOICES): the customer key, the
//! name and the read mask are checked with the request; the caller is then authorized for the
//! bucket, which refuses a caller only when the bucket does not exist and the caller holds the
//! permission in no project; then the generation and the conditions are checked; the caller is
//! authorized for the object when it is looked up, before its conditions are evaluated; and only
//! then are the read offset and the read limit checked against the object.

use super::super::tests::{message, setup, write};
use super::test_support::assert_status;
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
    // In an existing bucket, the generation precedes the conditions, and both precede the
    // object's authorization and the lookup.
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

/// The bucket is authorized before the generation and the conditions are checked. In an
/// existing bucket that refuses no one, because the object is authorized with the lookup; in a
/// missing bucket it refuses a caller that holds `storage.objects.get` in no project.
#[tokio::test]
async fn a_missing_bucket_is_authorized_before_the_generation_and_the_conditions() {
    let (_dir, storage) = setup(true).await;
    let missing = |generation, condition| pb::ReadObjectRequest {
        bucket: "projects/_/buckets/no-such-bucket".into(),
        object: "object".into(),
        generation,
        if_metageneration_match: condition,
        ..Default::default()
    };
    assert_status(
        open(&storage, missing(-1, Some(-1)), Some(Caller::Anonymous)).await,
        tonic::Code::Unauthenticated,
        "Login Required",
    );
    let reader = crate::grpc::test_support::caller(&storage, "reader").await;
    assert_status(
        open(&storage, missing(-1, Some(-1)), reader).await,
        tonic::Code::InvalidArgument,
        GENERATION,
    );
    assert_status(
        open(&storage, missing(0, Some(-1)), reader).await,
        tonic::Code::InvalidArgument,
        CONDITIONS,
    );
    assert_code(
        open(&storage, missing(0, None), reader).await,
        tonic::Code::NotFound,
    );
    storage.state.catalog.close().await.unwrap();

    // A principal without the permission in any project is refused before the generation too.
    let dir = crate::test_support::TestDir::new();
    let catalog = crate::storage::Catalog::open(&dir.0).await.unwrap();
    let auth =
        crate::auth::Auth::strict(crate::grpc::test_support::registry(&[("nobody", "", "")]));
    let state = std::sync::Arc::new(crate::server::State::for_test(catalog, auth));
    let ungranted = crate::grpc::test_support::storage_with(state, 1024);
    let nobody = crate::grpc::test_support::caller(&ungranted, "nobody").await;
    assert_code(
        open(&ungranted, missing(-1, Some(-1)), nobody).await,
        tonic::Code::PermissionDenied,
    );
    ungranted.state.catalog.close().await.unwrap();
}
