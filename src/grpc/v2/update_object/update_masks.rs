//! UpdateObject update masks as Cloud Storage answered them (recorded 2026-11-18, case
//! grpc-probe-object-masks): `*` replaces the editable metadata, and an output-only path is
//! accepted and changes nothing but the metageneration.

use super::super::test_support::{RESOURCE, grants, setup, stored_object, upload};
use super::*;
use crate::grpc::test_support as fixtures;

fn request(object: pb::Object, paths: &[&str]) -> pb::UpdateObjectRequest {
    pb::UpdateObjectRequest {
        object: Some(pb::Object {
            bucket: RESOURCE.into(),
            name: "object".into(),
            ..object
        }),
        update_mask: fixtures::mask(paths),
        ..Default::default()
    }
}

async fn update(storage: &Storage, request: pb::UpdateObjectRequest) -> pb::Object {
    storage
        .update_object(Request::new(request), None)
        .await
        .unwrap()
        .into_inner()
}

#[tokio::test]
async fn a_wildcard_mask_replaces_the_editable_metadata_and_keeps_the_acl() {
    let (storage, _dir) = setup().await;
    upload(&storage, "object", 4).await;
    let described = update(
        &storage,
        request(
            pb::Object {
                content_type: "text/plain".into(),
                content_language: "it".into(),
                cache_control: "no-cache".into(),
                metadata: [("k".into(), "v".into())].into(),
                ..Default::default()
            },
            &[
                "content_type",
                "content_language",
                "cache_control",
                "metadata",
            ],
        ),
    )
    .await;

    let replaced = update(
        &storage,
        request(
            pb::Object {
                content_type: "text/x-wildcard".into(),
                ..Default::default()
            },
            &["*"],
        ),
    )
    .await;
    assert_eq!(replaced.content_type, "text/x-wildcard");
    assert_eq!(replaced.content_language, "");
    assert_eq!(replaced.cache_control, "");
    assert!(replaced.metadata.is_empty());
    assert_eq!(replaced.metageneration, described.metageneration + 1);
    assert_eq!(grants(&replaced), grants(&described));
    assert!(!replaced.acl.is_empty());
    assert_eq!(replaced.owner, described.owner);
    storage.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn an_output_only_path_changes_nothing_but_the_metageneration() {
    let (storage, _dir) = setup().await;
    let original = upload(&storage, "object", 4).await;
    let stamp = Some(prost_types::Timestamp {
        seconds: 1_577_836_800,
        nanos: 0,
    });
    let mut metageneration = original.metageneration;
    for path in ["create_time", "update_time"] {
        let updated = update(
            &storage,
            request(
                pb::Object {
                    create_time: stamp,
                    update_time: stamp,
                    content_language: "fr".into(),
                    ..Default::default()
                },
                &[path],
            ),
        )
        .await;
        metageneration += 1;
        assert_eq!(updated.metageneration, metageneration, "{path}");
        assert_ne!(updated.create_time, stamp, "{path}");
        assert_ne!(updated.update_time, stamp, "{path}");
        assert_eq!(updated.content_language, "", "{path}");
    }
    let stored = stored_object(&storage, "object").await.unwrap();
    assert_eq!(stored.time_created, original.time_created);
    storage.state.catalog.close().await.unwrap();
}
