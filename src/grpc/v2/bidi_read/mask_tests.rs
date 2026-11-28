//! The object metadata in BidiReadObject's first response. The deprecated `read_mask` defaults
//! to every field except `metadata.owner` and `metadata.acl`, and `*` means every field
//! (storage.proto lines 1183-1187). The `x-goog-fieldmask` header, which the proto names as the
//! field's alternative, takes `*` the same way.

use super::request_refusals::{opening, setup, spec, stream};
use super::*;
use crate::grpc::test_support::mask;
use tonic::codegen::tokio_stream::StreamExt;

/// The metadata ReadObject returns in its first response under `paths`.
async fn read_object_metadata(
    state: &std::sync::Arc<crate::service::state::State>,
    paths: &[&str],
) -> pb::Object {
    let storage = crate::grpc::test_support::storage_with(state.clone(), 1024 * 1024);
    let request = pb::ReadObjectRequest {
        bucket: spec().bucket,
        object: spec().object,
        read_mask: mask(paths),
        ..Default::default()
    };
    let mut messages = storage
        .read_object(Request::new(request), None)
        .await
        .unwrap()
        .into_inner();
    let first = messages.next().await.unwrap().unwrap();
    first.metadata.unwrap()
}

/// The metadata of the first BidiReadObject response.
async fn first_metadata(
    state: &std::sync::Arc<crate::service::state::State>,
    read_mask: Option<prost_types::FieldMask>,
    fieldmask: Option<&'static str>,
) -> pb::Object {
    #[expect(
        deprecated,
        reason = "the deprecated read_mask field is still documented and still honored"
    )]
    let spec = pb::BidiReadObjectSpec {
        read_mask,
        ..spec()
    };
    let (status, responses) = stream(state, &[opening(spec)], fieldmask).await;
    assert_eq!(status.code(), tonic::Code::Ok, "{status:?}");
    responses[0].metadata.clone().unwrap()
}

#[tokio::test]
async fn a_wildcard_mask_returns_the_acl_and_owner_the_default_omits() {
    let (_dir, state) = setup().await;
    let everything = read_object_metadata(&state, &["*"]).await;
    assert!(
        !everything.acl.is_empty() && everything.owner.is_some(),
        "{everything:?}"
    );
    for (read_mask, fieldmask) in [
        (mask(&["*"]), None),
        (None, Some("*")),
        (mask(&[]), Some("*")),
        (mask(&["*"]), Some("*")),
    ] {
        let metadata = first_metadata(&state, read_mask.clone(), fieldmask).await;
        assert_eq!(metadata, everything, "{read_mask:?} {fieldmask:?}");
    }

    let default = read_object_metadata(&state, &[]).await;
    assert!(
        default.acl.is_empty() && default.owner.is_none(),
        "{default:?}"
    );
    for read_mask in [None, mask(&[])] {
        let metadata = first_metadata(&state, read_mask.clone(), None).await;
        assert_eq!(metadata, default, "{read_mask:?}");
    }
    state.catalog.close().await.unwrap();
}
