//! The stored release of an event-based hold, which restarts the bucket retention period.

use crate::{
    clock::{Clock, ManualClock},
    object::{Hold, ObjectError, ObjectMetadataUpdate, ObjectRecord},
    storage::{
        Catalog, ObjectUpload,
        catalog::test_support,
        test_fixtures::{PERMISSIVE, assert_current_schema, body, bucket, live, permissive_upload},
    },
    test_support::TestDir,
};
use bytes::Bytes;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn at(seconds: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}

async fn open(dir: &TestDir, clock: &Arc<ManualClock>) -> Catalog {
    Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap()
}

async fn put(catalog: &Catalog, name: &str, event_based_hold: Option<bool>) {
    catalog
        .put_object(
            ObjectUpload {
                metadata: ObjectMetadataUpdate {
                    event_based_hold,
                    ..Default::default()
                },
                ..permissive_upload("bucket", name, 100)
            },
            body(vec![Bytes::from_static(b"data")]),
        )
        .await
        .unwrap();
}

async fn set_event_based_hold(catalog: &Catalog, name: &str, held: bool) -> ObjectRecord {
    catalog
        .update_object_metadata(
            live("bucket", name),
            Default::default(),
            ObjectMetadataUpdate {
                event_based_hold: Some(held),
                ..Default::default()
            },
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap()
}

async fn deadline(catalog: &Catalog, name: &str) -> Option<SystemTime> {
    catalog
        .get_object(live("bucket", name), Default::default(), PERMISSIVE)
        .await
        .unwrap()
        .retention_expiration_time
}

/// Schema 56 never stored a release, so every object it holds keeps the deadline that schema
/// gave it, counted from creation, even one whose hold it saw released. The migrated catalog
/// records the next release and keeps it across a restart.
#[tokio::test]
async fn migrating_from_schema_56_keeps_deadlines_and_records_later_releases() {
    let dir = TestDir::new();
    let clock = Arc::new(ManualClock::new(at(1000)));
    let catalog = open(&dir, &clock).await;
    let mut record = bucket("bucket", "project");
    record.retention_policy = Some(crate::bucket::RetentionPolicy {
        retention_period: 10,
        effective_time: clock.now(),
        is_locked: false,
    });
    catalog.insert_bucket(record).await.unwrap();
    put(&catalog, "plain", None).await;
    put(&catalog, "released", Some(true)).await;
    put(&catalog, "held", Some(true)).await;
    clock.set(at(1500));
    // Dropping the column below forgets this release, as schema 56 never stored it.
    set_event_based_hold(&catalog, "released", false).await;
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    test_support::downgrade_event_hold_release_schema(&connection);
    connection.pragma_update(None, "user_version", 56).unwrap();
    drop(connection);

    let catalog = open(&dir, &clock).await;
    assert_eq!(deadline(&catalog, "plain").await, Some(at(1010)));
    assert_eq!(deadline(&catalog, "released").await, Some(at(1010)));
    assert_eq!(deadline(&catalog, "held").await, None);
    clock.set(at(2000));
    let released = set_event_based_hold(&catalog, "held", false).await;
    assert_eq!(released.event_based_hold, Hold::Released);
    assert_eq!(released.retention_expiration_time, Some(at(2010)));
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_current_schema(&connection);
    drop(connection);

    let catalog = open(&dir, &clock).await;
    assert_eq!(deadline(&catalog, "held").await, Some(at(2010)));
    assert_eq!(deadline(&catalog, "plain").await, Some(at(1010)));
    let refused = catalog
        .delete_object(live("bucket", "held"), Default::default(), PERMISSIVE)
        .await
        .unwrap_err();
    let ObjectError::ObjectRetentionNotMet(retained) = refused else {
        panic!("a restarted period protects the object: {refused:?}");
    };
    assert_eq!(retained.until, at(2010).into());
    clock.set(at(2010));
    catalog
        .delete_object(live("bucket", "held"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    catalog.close().await.unwrap();
}
