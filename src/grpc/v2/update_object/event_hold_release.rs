//! UpdateObject releasing an event-based hold under a bucket retention policy. The pinned v2
//! proto says "the retention duration is calculated from the time the event based hold was
//! lifted, rather than the time the object was created" (`Object.event_based_hold`), and that
//! `retention_expire_time` "is not provided for objects with an active event-based hold".

use super::super::test_support::upload_to;
use super::*;
use crate::{
    auth::Auth,
    bucket::{BucketRecord, RetentionPolicy},
    clock::{Clock, ManualClock},
    grpc::test_support as fixtures,
    object::unix_nanos::UnixNanos,
    storage::Catalog,
    test_support::TestDir,
};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tonic::Code;

const BUCKET: &str = "release-policy";

fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

fn hold(held: bool) -> Request<pb::UpdateObjectRequest> {
    Request::new(pb::UpdateObjectRequest {
        object: Some(pb::Object {
            bucket: format!("projects/_/buckets/{BUCKET}"),
            name: "kept".into(),
            event_based_hold: Some(held),
            ..Default::default()
        }),
        update_mask: fixtures::mask(&["event_based_hold"]),
        ..Default::default()
    })
}

fn delete() -> Request<pb::DeleteObjectRequest> {
    Request::new(pb::DeleteObjectRequest {
        bucket: format!("projects/_/buckets/{BUCKET}"),
        object: "kept".into(),
        ..Default::default()
    })
}

#[tokio::test]
async fn update_object_releasing_the_event_based_hold_restarts_the_retention_period() {
    let dir = TestDir::new();
    let clock = Arc::new(ManualClock::new(at(1000)));
    let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap();
    let now = clock.now();
    let bucket = BucketRecord {
        retention_policy: Some(RetentionPolicy {
            retention_period: 10,
            effective_time: now,
            is_locked: false,
        }),
        ..BucketRecord::for_test(BUCKET, "local-project", now)
    };
    catalog.insert_bucket(bucket).await.unwrap();
    let storage = Storage {
        compat_profile: crate::config::CompatProfile::Native,
        state: Arc::new(crate::server::State::for_test(catalog, Auth::permissive())),
        max_object_bytes: 1024,
    };
    upload_to(&storage, BUCKET, "kept", 3).await;
    let held = storage.update_object(hold(true), None).await.unwrap();
    assert_eq!(held.get_ref().retention_expire_time, None);
    clock.set(at(2000));
    let released = storage
        .update_object(hold(false), None)
        .await
        .unwrap()
        .into_inner();
    assert_eq!(released.event_based_hold, Some(false));
    assert_eq!(
        released.retention_expire_time,
        Some(prost_types::Timestamp {
            seconds: 2010,
            nanos: 0
        })
    );
    let denied = storage.delete_object(delete(), None).await.unwrap_err();
    assert_eq!(denied.code(), Code::PermissionDenied);
    assert_eq!(
        denied.message(),
        format!(
            "Object '{BUCKET}/kept' is subject to bucket's retention policy or object retention and cannot be deleted or overwritten until {}",
            crate::object::retention::pacific_rfc3339(UnixNanos::from(at(2010)))
        )
    );
    clock.set(at(2010));
    storage.delete_object(delete(), None).await.unwrap();
    storage.state.catalog.close().await.unwrap();
}
