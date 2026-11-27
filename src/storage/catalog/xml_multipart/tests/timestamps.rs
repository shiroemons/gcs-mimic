//! Upload and part times keep whole microseconds through listings, part replacement, restart and
//! the schema 54 upgrade.

use super::*;
use crate::storage::catalog::SESSION_LIFETIME_MILLIS;
use crate::storage::test_fixtures::assert_current_schema;

/// 2026-11-16T06:37:53.548719321Z: the `Initiated` time Cloud Storage reported in the recorded
/// multipart listing, with nanoseconds below its microseconds.
const INITIATED: Duration = Duration::new(1_794_811_073, 548_719_321);

/// Reads the upload's creation time through its lease, the upload listing and the part listing,
/// and its parts' times through the part listing.
async fn assert_times(
    catalog: &Catalog,
    target: &Target,
    initiated: SystemTime,
    updated: &[SystemTime],
) {
    let lease = catalog
        .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
        .await
        .unwrap();
    assert_eq!(lease.upload.created, initiated);
    drop(lease);
    let query = UploadQuery {
        bucket: "bucket".into(),
        prefix: String::new(),
        delimiter: String::new(),
        key_marker: String::new(),
        upload_marker: None,
        limit: 10,
    };
    let page = catalog
        .list_xml_uploads(query, BucketAccess::Permissive)
        .await
        .unwrap();
    let created: Vec<_> = page.uploads.iter().map(|upload| upload.created).collect();
    assert_eq!(created, [initiated]);
    let page = catalog
        .list_xml_parts(target.clone(), BucketAccess::Permissive, 0, 10)
        .await
        .unwrap();
    assert_eq!(page.upload.created, initiated);
    let listed: Vec<_> = page.parts.iter().map(|part| part.updated).collect();
    assert_eq!(listed, updated);
}

#[tokio::test]
async fn upload_and_part_times_keep_microseconds_through_listings_replacement_and_restart() {
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH + INITIATED));
    let (dir, catalog) = setup(clock.clone()).await;
    let initiated = UNIX_EPOCH + Duration::new(1_794_811_073, 548_719_000);
    let options = Options {
        content_type: "text/plain".into(),
        metadata: Default::default(),
    };
    let upload = catalog
        .begin_xml_multipart(
            ObjectKey::from_parts_unchecked("bucket", "value"),
            options,
            BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(upload.created, initiated);
    let target = upload.target;

    clock.set(UNIX_EPOCH + Duration::new(1_794_811_074, 250_031_999));
    let first = put(&catalog, &target, 1, b"first").await.unwrap();
    assert_eq!(
        first.updated,
        UNIX_EPOCH + Duration::new(1_794_811_074, 250_031_000)
    );
    // The replacement lands on a whole millisecond, so a remainder left from the first write
    // would show.
    clock.set(UNIX_EPOCH + Duration::new(1_794_811_075, 7_000_999));
    let replaced = UNIX_EPOCH + Duration::new(1_794_811_075, 7_000_000);
    let second = put(&catalog, &target, 1, b"second").await.unwrap();
    assert_eq!(second.updated, replaced);
    clock.set(UNIX_EPOCH + Duration::new(1_794_811_076, 999_999_999));
    let other = UNIX_EPOCH + Duration::new(1_794_811_076, 999_999_000);
    assert_eq!(
        put(&catalog, &target, 2, b"other").await.unwrap().updated,
        other
    );
    assert_times(&catalog, &target, initiated, &[replaced, other]).await;
    catalog.close().await.unwrap();

    let catalog = Catalog::open_for_server(&dir.0, clock, LocalAdmin::Disabled)
        .await
        .unwrap();
    assert_times(&catalog, &target, initiated, &[replaced, other]).await;
    catalog.close().await.unwrap();
}

/// Schema 53 stored whole milliseconds. Its rows read back with no microseconds past them, and
/// the upload still expires at its millisecond deadline.
#[tokio::test]
async fn schema_53_uploads_and_parts_upgrade_to_whole_milliseconds_and_keep_their_deadline() {
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH + INITIATED));
    let (dir, catalog) = setup(clock.clone()).await;
    let target = begin(&catalog).await;
    put(&catalog, &target, 1, b"part").await.unwrap();
    catalog.close().await.unwrap();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    crate::storage::catalog::test_support::downgrade_multipart_micros_schema(&connection);
    connection.pragma_update(None, "user_version", 53).unwrap();
    drop(connection);

    let catalog = Catalog::open_for_server(&dir.0, clock.clone(), LocalAdmin::Disabled)
        .await
        .unwrap();
    let whole_millis = UNIX_EPOCH + Duration::new(1_794_811_073, 548_000_000);
    assert_times(&catalog, &target, whole_millis, &[whole_millis]).await;
    for invalid in [-1, 1000] {
        let rejected = catalog
            .run(move |connection| {
                let uploads = sql::exec_on_writer(
                    connection,
                    "UPDATE xml_multipart_uploads SET created_sub_milli_micros=?1",
                    [invalid],
                );
                let parts = sql::exec_on_writer(
                    connection,
                    "UPDATE xml_multipart_parts SET updated_sub_milli_micros=?1",
                    [invalid],
                );
                Ok::<_, CatalogError>((uploads.is_err(), parts.is_err()))
            })
            .await
            .unwrap();
        assert_eq!(rejected, (true, true), "{invalid}");
    }

    let deadline =
        whole_millis + Duration::from_millis(u64::try_from(SESSION_LIFETIME_MILLIS).unwrap());
    assert_eq!(
        catalog
            .maintain_xml_multipart(UnixMillis::try_from(clock.now()).unwrap())
            .await
            .unwrap(),
        Some(deadline)
    );
    clock.set(deadline - Duration::from_millis(1));
    drop(
        catalog
            .lease_xml_multipart(target.clone(), BucketAccess::Permissive)
            .await
            .unwrap(),
    );
    clock.set(deadline);
    assert!(matches!(
        catalog
            .lease_xml_multipart(target, BucketAccess::Permissive)
            .await,
        Err(Error::NoSuchUpload)
    ));
    catalog
        .maintain_xml_multipart(UnixMillis::try_from(clock.now()).unwrap())
        .await
        .unwrap();
    assert_eq!(upload_count(&catalog).await, 0);
    catalog.close().await.unwrap();
    assert_current_schema(&rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap());
}
