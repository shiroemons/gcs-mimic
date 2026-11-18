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
        create_time: stamp,
        update_time: stamp,
        ..Default::default()
    })
    .await
    .unwrap();
    assert_ne!(composed.generation, 1);
    assert_eq!(composed.metageneration, 1);
    assert_eq!(composed.size, 4);
    assert_ne!(composed.create_time, stamp);
    assert_ne!(composed.update_time, stamp);
    s.state.catalog.close().await.unwrap();
}
