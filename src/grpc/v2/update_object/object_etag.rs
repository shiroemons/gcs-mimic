//! UpdateObject with an etag in the request's object. The pinned v2 proto says of
//! `Object.etag`: "If included in the metadata of an update or delete request message, the
//! operation is only performed if the etag matches that of the live object." Cloud Storage
//! applied an update whose etag an earlier update had superseded (recorded 2026-11-18, case
//! grpc-probe-object-masks), so the etag sets no condition.

use super::super::test_support::{RESOURCE, setup, stored_object, upload};
use super::*;
use crate::grpc::test_support as fixtures;

fn relabel(etag: &str, language: &str) -> pb::UpdateObjectRequest {
    pb::UpdateObjectRequest {
        object: Some(pb::Object {
            bucket: RESOURCE.into(),
            name: "object".into(),
            etag: etag.into(),
            content_language: language.into(),
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

#[tokio::test]
async fn update_object_ignores_the_request_etag() {
    let (storage, _dir) = setup().await;
    let original = upload(&storage, "object", 4).await;
    let first = update(&storage, relabel("", "fr")).await.unwrap();

    let second = update(&storage, relabel(&first.etag, "de")).await.unwrap();
    assert_ne!(second.etag, first.etag);
    // `second` superseded the etag of `first`; a made-up one names no version at all.
    let stale = update(&storage, relabel(&first.etag, "it")).await.unwrap();
    assert_eq!(stale.content_language, "it");
    let unknown = update(&storage, relabel("not an etag", "es"))
        .await
        .unwrap();
    assert_eq!(unknown.content_language, "es");
    let current = update(&storage, relabel(&unknown.etag, "pt"))
        .await
        .unwrap();
    assert_eq!(current.content_language, "pt");

    let stored = stored_object(&storage, "object").await.unwrap();
    assert_eq!(stored.content_language.as_deref(), Some("pt"));
    assert_eq!(stored.metageneration, original.metageneration + 5);
    storage.state.catalog.close().await.unwrap();
}
