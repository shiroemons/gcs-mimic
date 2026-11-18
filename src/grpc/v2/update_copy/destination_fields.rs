//! Destination fields ComposeObject accepts (recorded 2026-11-18, case
//! grpc-probe-derived-writes): a storage class every source has, and fields only the service
//! sets, which change nothing.

use super::super::test_support::{RESOURCE, setup, upload};
use super::*;

fn compose(destination: pb::Object) -> pb::ComposeObjectRequest {
    pb::ComposeObjectRequest {
        destination: Some(pb::Object {
            bucket: RESOURCE.into(),
            ..destination
        }),
        source_objects: vec![pb::compose_object_request::SourceObject {
            name: "source".into(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// A destination loses the fields the proto marks `OUTPUT_ONLY` and the ones the service keeps
/// (the generation, the etag, the retention expiry); every other field, the name and the bucket
/// included, stays as the request set it.
#[test]
fn service_set_fields_are_the_generation_and_the_output_only_fields() {
    let stamp = Some(prost_types::Timestamp {
        seconds: 1_577_836_800,
        nanos: 0,
    });
    let editable = pb::Object {
        name: "name".into(),
        bucket: "bucket".into(),
        storage_class: "STANDARD".into(),
        content_encoding: "gzip".into(),
        content_disposition: "inline".into(),
        cache_control: "no-cache".into(),
        acl: vec![pb::ObjectAccessControl::default()],
        content_language: "en".into(),
        content_type: "text/plain".into(),
        kms_key: "key".into(),
        temporary_hold: true,
        metadata: [("key".into(), "value".into())].into(),
        contexts: Some(Default::default()),
        event_based_hold: Some(true),
        customer_encryption: Some(Default::default()),
        custom_time: stamp,
        retention: Some(Default::default()),
        ..Default::default()
    };
    let service_set = pb::Object {
        etag: "etag".into(),
        retention_expire_time: stamp,
        generation: 1,
        restore_token: Some("token".into()),
        metageneration: 2,
        size: 3,
        delete_time: stamp,
        finalize_time: stamp,
        create_time: stamp,
        component_count: 4,
        checksums: Some(Default::default()),
        update_time: stamp,
        update_storage_class_time: stamp,
        owner: Some(Default::default()),
        soft_delete_time: stamp,
        hard_delete_time: stamp,
        ..editable.clone()
    };
    assert_eq!(without_service_fields(service_set), editable);
}

#[tokio::test]
async fn compose_object_accepts_the_sources_class_and_ignores_service_set_fields() {
    let (s, _dir) = setup().await;
    upload(&s, "source", 4).await;
    let compose_with = async |destination| {
        s.compose_object(Request::new(compose(destination)), None)
            .await
            .map(Response::into_inner)
    };

    let same_class = compose_with(pb::Object {
        name: "same-class".into(),
        storage_class: "STANDARD".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(same_class.storage_class, "STANDARD");

    let stamp = Some(prost_types::Timestamp {
        seconds: 1_577_836_800,
        nanos: 0,
    });
    let composed = compose_with(pb::Object {
        name: "service-fields".into(),
        generation: 1,
        metageneration: 7,
        size: 99,
        etag: "gcsmimic-probe".into(),
        create_time: stamp,
        update_time: stamp,
        ..Default::default()
    })
    .await
    .unwrap();
    assert_ne!(composed.generation, 1);
    assert_eq!(composed.metageneration, 1);
    assert_eq!(composed.size, 4);
    assert_ne!(composed.etag, "gcsmimic-probe");
    assert_ne!(composed.create_time, stamp);
    assert_ne!(composed.update_time, stamp);
    s.state.catalog.close().await.unwrap();
}

/// A RewriteObject destination that carries one service-set field alone is rewritten as if it
/// had none, except for a generation (case grpc-probe-inferred-destinations).
#[tokio::test]
async fn rewrite_object_ignores_each_service_set_destination_field_but_the_generation() {
    let (s, _dir) = setup().await;
    upload(&s, "source", 4).await;
    let stamp = Some(prost_types::Timestamp {
        seconds: 1_577_836_800,
        nanos: 0,
    });
    let destinations = [
        (
            "metageneration",
            pb::Object {
                metageneration: 7,
                ..Default::default()
            },
        ),
        (
            "size",
            pb::Object {
                size: 99,
                ..Default::default()
            },
        ),
        (
            "etag",
            pb::Object {
                etag: "gcsmimic-probe".into(),
                ..Default::default()
            },
        ),
        (
            "create-time",
            pb::Object {
                create_time: stamp,
                ..Default::default()
            },
        ),
        (
            "update-time",
            pb::Object {
                update_time: stamp,
                ..Default::default()
            },
        ),
        (
            "checksums",
            pb::Object {
                checksums: Some(Default::default()),
                ..Default::default()
            },
        ),
        (
            "owner",
            pb::Object {
                owner: Some(Default::default()),
                ..Default::default()
            },
        ),
        (
            "component-count",
            pb::Object {
                component_count: 7,
                ..Default::default()
            },
        ),
    ];
    for (name, destination) in destinations {
        let rewritten = s
            .rewrite_object(
                Request::new(pb::RewriteObjectRequest {
                    source_bucket: RESOURCE.into(),
                    source_object: "source".into(),
                    destination_bucket: RESOURCE.into(),
                    destination_name: format!("rewritten-{name}"),
                    destination: Some(destination),
                    ..Default::default()
                }),
                None,
            )
            .await
            .unwrap()
            .into_inner();
        assert!(rewritten.done, "{name}");
        let resource = rewritten.resource.unwrap();
        assert_eq!(resource.size, 4, "{name}");
        assert_eq!(resource.metageneration, 1, "{name}");
        assert_eq!(resource.component_count, 0, "{name}");
        assert_ne!(resource.etag, "gcsmimic-probe", "{name}");
        assert_ne!(resource.create_time, stamp, "{name}");
    }
    s.state.catalog.close().await.unwrap();
}
