//! UpdateObject update masks as Cloud Storage answered them (recorded 2026-11-18, cases
//! grpc-probe-object-masks and grpc-probe-inferred-object-masks): `*` replaces the editable
//! metadata, and an output-only path, or one of a field the service keeps, is accepted and
//! changes nothing but the metageneration.

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

/// Cloud Storage took the paths of fields it keeps or that name the object like an output-only
/// path, and refused `storage_class` and `kms_key` whatever the value (recorded in case
/// grpc-probe-inferred-object-masks).
#[tokio::test]
async fn service_kept_paths_change_nothing_and_a_class_or_key_path_is_refused() {
    let (storage, _dir) = setup().await;
    let original = upload(&storage, "object", 4).await;
    let mut metageneration = original.metageneration;
    for path in [
        "etag",
        "generation",
        "retention_expire_time",
        "name",
        "bucket",
    ] {
        let updated = update(
            &storage,
            request(
                pb::Object {
                    etag: "other".into(),
                    generation: original.generation,
                    retention_expire_time: Some(prost_types::Timestamp {
                        seconds: 1_577_836_800,
                        nanos: 0,
                    }),
                    content_language: "fr".into(),
                    ..Default::default()
                },
                &[path],
            ),
        )
        .await;
        metageneration += 1;
        assert_eq!(updated.metageneration, metageneration, "{path}");
        assert_eq!(updated.generation, original.generation, "{path}");
        assert!(!updated.etag.is_empty(), "{path}");
        assert_ne!(updated.etag, "other", "{path}");
        assert_eq!(updated.retention_expire_time, None, "{path}");
        assert_eq!(updated.content_language, "", "{path}");
    }
    for (path, message) in [
        (
            "storage_class",
            "Client is not allowed to directly modify storage_class.",
        ),
        (
            "kms_key",
            "Cannot change the Cloud KMS key associated with a resource.",
        ),
    ] {
        let refused = storage
            .update_object(
                Request::new(request(
                    pb::Object {
                        storage_class: "NEARLINE".into(),
                        ..Default::default()
                    },
                    &[path],
                )),
                None,
            )
            .await;
        fixtures::assert_bad_request(refused, message);
    }
    let stored = stored_object(&storage, "object").await.unwrap();
    assert_eq!(stored.metageneration, metageneration);
    storage.state.catalog.close().await.unwrap();
}

/// A `*` beside another path replaces the editable metadata, and a `*` update releases the holds
/// the request leaves out (recorded in case grpc-probe-inferred-object-masks).
#[tokio::test]
async fn a_wildcard_beside_another_path_replaces_the_metadata_and_releases_holds() {
    let (storage, _dir) = setup().await;
    upload(&storage, "object", 4).await;
    let held = update(
        &storage,
        request(
            pb::Object {
                temporary_hold: true,
                content_language: "it".into(),
                ..Default::default()
            },
            &["temporary_hold", "content_language"],
        ),
    )
    .await;
    assert!(held.temporary_hold);
    let replaced = update(
        &storage,
        request(
            pb::Object {
                content_type: "text/plain".into(),
                ..Default::default()
            },
            &["*", "content_language"],
        ),
    )
    .await;
    assert_eq!(replaced.content_type, "text/plain");
    assert_eq!(replaced.content_language, "");
    assert!(!replaced.temporary_hold);
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
