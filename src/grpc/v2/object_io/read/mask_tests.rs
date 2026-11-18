//! When GetObject, ReadObject and ListObjects check their read masks. Which fields a mask may
//! name does not depend on the object, so the mask is checked with the request, before the caller
//! is authorized or anything is looked up, as GetBucket checks its mask.

use super::super::tests::{message, setup, write};
use super::*;
use crate::grpc::test_support::{assert_code, assert_unsupported, mask};

const BUCKET: &str = "projects/_/buckets/grpc-io";
const UNKNOWN_FIELD: &str = "There is no field for this resource named 'noSuchField'.";

fn get_request(object: &str, paths: &[&str]) -> pb::GetObjectRequest {
    pb::GetObjectRequest {
        bucket: BUCKET.into(),
        object: object.into(),
        read_mask: mask(paths),
        ..Default::default()
    }
}

fn read_request(object: &str, paths: &[&str]) -> pb::ReadObjectRequest {
    pb::ReadObjectRequest {
        bucket: BUCKET.into(),
        object: object.into(),
        read_mask: mask(paths),
        ..Default::default()
    }
}

fn list_request(paths: &[&str]) -> pb::ListObjectsRequest {
    pb::ListObjectsRequest {
        parent: BUCKET.into(),
        prefix: "absent/".into(),
        read_mask: mask(paths),
        ..Default::default()
    }
}

async fn get(
    storage: &Storage,
    request: pb::GetObjectRequest,
    caller: Option<Caller<'_>>,
) -> Result<pb::Object, Status> {
    storage
        .get_object(Request::new(request), caller)
        .await
        .map(Response::into_inner)
}

/// The messages of a ReadObject, whether it fails when opened or while streaming.
async fn read(
    storage: &Storage,
    request: pb::ReadObjectRequest,
    caller: Option<Caller<'_>>,
) -> Result<Vec<pb::ReadObjectResponse>, Status> {
    let mut stream = storage
        .read_object(Request::new(request), caller)
        .await?
        .into_inner();
    let mut messages = Vec::new();
    while let Some(message) = stream.next().await {
        messages.push(message?);
    }
    Ok(messages)
}

async fn list(
    storage: &Storage,
    request: pb::ListObjectsRequest,
    caller: Option<Caller<'_>>,
) -> Result<pb::ListObjectsResponse, Status> {
    storage
        .list_objects(Request::new(request), caller)
        .await
        .map(Response::into_inner)
}

#[track_caller]
fn assert_unknown_field<T>(result: Result<T, Status>) {
    let status = assert_code(result, tonic::Code::InvalidArgument);
    assert_eq!(status.message(), UNKNOWN_FIELD);
}

#[tokio::test]
async fn an_invalid_read_mask_is_reported_for_a_missing_object() {
    let (_dir, storage) = setup(false).await;
    assert_unknown_field(get(&storage, get_request("missing", &["noSuchField"]), None).await);
    assert_unsupported(
        get(&storage, get_request("missing", &["owner.entity"]), None).await,
        "read_mask path owner.entity",
    );
    assert_unknown_field(
        read(
            &storage,
            read_request("missing", &["checksummed_data", "metadata.noSuchField"]),
            None,
        )
        .await,
    );
    assert_unsupported(
        read(
            &storage,
            read_request("missing", &["checksummed_data", "metadata.owner.entity"]),
            None,
        )
        .await,
        "read_mask path owner.entity",
    );
    assert_unsupported(
        read(
            &storage,
            read_request("missing", &["checksummed_data", "read_handle"]),
            None,
        )
        .await,
        "ReadObject read_mask read_handle",
    );
    // A valid mask still reports the missing object.
    let missing = get(&storage, get_request("missing", &["name"]), None).await;
    assert_code(missing, tonic::Code::NotFound);
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn an_invalid_read_mask_is_reported_for_an_empty_listing() {
    let (_dir, storage) = setup(false).await;
    assert_unknown_field(list(&storage, list_request(&["noSuchField"]), None).await);
    assert_unsupported(
        list(&storage, list_request(&["name", "owner.entity"]), None).await,
        "read_mask path owner.entity",
    );
    let empty = list(&storage, list_request(&["name"]), None).await.unwrap();
    assert!(empty.objects.is_empty());
    storage.state.catalog.close().await.unwrap();
}

/// The mask is checked before the caller is authorized and before the generation, the
/// conditions and the read range, which the RPCs check after authorization.
#[tokio::test]
async fn an_invalid_read_mask_precedes_authorization_and_the_other_checks() {
    let (_dir, storage) = setup(true).await;
    write(&storage, vec![message("object", b"data")])
        .await
        .unwrap();
    let anonymous = Some(Caller::Anonymous);
    assert_unknown_field(get(&storage, get_request("object", &["noSuchField"]), anonymous).await);
    assert_unknown_field(
        read(
            &storage,
            read_request("object", &["checksummed_data", "metadata.noSuchField"]),
            anonymous,
        )
        .await,
    );
    assert_unknown_field(list(&storage, list_request(&["noSuchField"]), anonymous).await);
    // Without the mask, the same anonymous requests are refused.
    assert!(
        get(&storage, get_request("object", &[]), anonymous)
            .await
            .is_err()
    );
    assert!(list(&storage, list_request(&[]), anonymous).await.is_err());

    let admin = crate::grpc::test_support::caller(&storage, "admin").await;
    let mut negative = get_request("object", &["noSuchField"]);
    negative.generation = -1;
    negative.if_metageneration_match = Some(-1);
    assert_unknown_field(get(&storage, negative, admin).await);
    let mut past_end = read_request("object", &["checksummed_data", "metadata.noSuchField"]);
    past_end.read_offset = 100;
    assert_unknown_field(read(&storage, past_end, admin).await);
    storage.state.catalog.close().await.unwrap();
}

/// A malformed key, the unsupported soft-deleted lookup and a malformed name are still
/// reported before the mask.
#[tokio::test]
async fn request_checks_before_the_mask_keep_their_order() {
    let (_dir, storage) = setup(false).await;
    let mut keyed = get_request("missing", &["noSuchField"]);
    keyed.common_object_request_params = Some(pb::CommonObjectRequestParams {
        encryption_algorithm: crate::csek::ALGORITHM.into(),
        encryption_key_bytes: vec![1; 5],
        ..Default::default()
    });
    crate::grpc::test_support::assert_customer_encryption(
        get(&storage, keyed, None).await,
        crate::csek::CsekError::KeyFormatInvalid,
    );
    let mut soft_deleted = get_request("missing", &["noSuchField"]);
    soft_deleted.soft_deleted = Some(true);
    assert_unsupported(
        get(&storage, soft_deleted, None).await,
        "GetObject soft_deleted/restore_token",
    );
    let unnamed = assert_code(
        get(&storage, get_request("", &["noSuchField"]), None).await,
        tonic::Code::InvalidArgument,
    );
    assert!(
        !unnamed.message().contains("noSuchField"),
        "the name is checked first: {unnamed}"
    );
    let missing_content = read(&storage, read_request("missing", &["metadata.name"]), None).await;
    let status = assert_code(missing_content, tonic::Code::InvalidArgument);
    assert_eq!(
        status.message(),
        "The 'read_mask' field must include checksummed_data.content, but was 'metadata.name'."
    );
    storage.state.catalog.close().await.unwrap();
}
