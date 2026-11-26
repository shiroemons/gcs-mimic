//! How multipart maintenance selects due uploads and orphaned parts.

use super::*;

#[tokio::test]
async fn stopping_multipart_cleanup_keeps_real_failures_visible() {
    let (_dir, catalog) = setup(Arc::new(SystemClock)).await;
    catalog.0.shutdown.token.cancel();
    catalog.0.shutdown.tasks.drain().await;
    catalog.multipart_cleanup_failed(catalog.drain_xml_multipart().await.unwrap_err());
    assert!(catalog.resumable_maintenance_error().is_none());
    catalog.multipart_cleanup_failed(object_unavailable(io::Error::from(
        io::ErrorKind::StorageFull,
    )));
    assert!(catalog.resumable_maintenance_error().is_some());
    catalog.close().await.unwrap();
}

async fn orphan_every_upload(catalog: &Catalog) {
    catalog
        .run::<_, CatalogError>(|connection| {
            sql::exec_on_writer(
                connection,
                "UPDATE xml_multipart_uploads SET incarnation='00000000000000000000000000000000'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_busy_expired_upload_keeps_the_next_upload_deadline_scheduled() {
    let clock = Arc::new(ManualClock::new(
        UNIX_EPOCH + Duration::from_secs(1_000_000),
    ));
    let (_dir, catalog) = setup(clock.clone()).await;
    let first = begin(&catalog).await;
    let held = catalog
        .lease_xml_multipart(first, BucketAccess::Permissive)
        .await
        .unwrap();
    let lifetime = Duration::from_secs(7 * 86_400);
    let first_due = clock.now() + lifetime;
    clock.advance(Duration::from_secs(1));
    begin(&catalog).await;
    let second_due = clock.now() + lifetime;

    clock.set(first_due);
    let sweep = || catalog.maintain_xml_multipart(UnixMillis::try_from(clock.now()).unwrap());
    assert_eq!(sweep().await.unwrap(), Some(second_due));
    assert_eq!(upload_count(&catalog).await, 2);
    clock.set(second_due);
    assert_eq!(sweep().await.unwrap(), None);
    assert_eq!(upload_count(&catalog).await, 1);
    drop(held);
    assert_eq!(sweep().await.unwrap(), None);
    assert_eq!(upload_count(&catalog).await, 0);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn expired_uploads_are_found_through_the_deadline_index() {
    let (_dir, catalog) = setup(Arc::new(SystemClock)).await;
    let plans = catalog
        .run::<_, CatalogError>(|connection| {
            let plan = |sql: &str, params: &[&dyn rusqlite::ToSql]| {
                let mut statement = connection
                    .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
                    .unwrap();
                let rows = statement
                    .query_map(params, |row| row.get::<_, String>(3))
                    .unwrap();
                rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
            };
            Ok([
                plan(super::super::maintenance::EXPIRED_PAGE, params![0, 0, ""]),
                plan(super::super::maintenance::EXPIRED_FIRST_PAGE, params![0]),
            ])
        })
        .await
        .unwrap();
    for plan in plans {
        assert!(
            plan.iter()
                .any(|step| step.contains("xml_multipart_uploads_deadline")),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|step| step.starts_with("SCAN")),
            "{plan:?}"
        );
    }
    catalog.close().await.unwrap();
}

/// The orphan walk reads every upload, so it runs only when a deleted bucket asks for it. An orphan
/// nothing asked about still goes at the next open, and at its deadline before that.
#[tokio::test]
async fn orphans_are_looked_for_only_after_a_bucket_delete_and_at_open() {
    let clock = Arc::new(ManualClock::new(
        UNIX_EPOCH + Duration::from_secs(1_000_000),
    ));
    let (dir, catalog) = setup(clock.clone()).await;
    begin(&catalog).await;
    orphan_every_upload(&catalog).await;
    let now = UnixMillis::try_from(clock.now()).unwrap();
    catalog.maintain_xml_multipart(now).await.unwrap();
    assert_eq!(upload_count(&catalog).await, 1);

    catalog.schedule_multipart_orphan_scan();
    catalog.maintain_xml_multipart(now).await.unwrap();
    assert_eq!(upload_count(&catalog).await, 0);

    begin(&catalog).await;
    orphan_every_upload(&catalog).await;
    let at_deadline = now
        .checked_add_millis(crate::storage::catalog::SESSION_LIFETIME_MILLIS)
        .unwrap();
    catalog.maintain_xml_multipart(at_deadline).await.unwrap();
    assert_eq!(upload_count(&catalog).await, 0);

    begin(&catalog).await;
    orphan_every_upload(&catalog).await;
    catalog.close().await.unwrap();
    let reopened = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    assert_eq!(upload_count(&reopened).await, 0);
    reopened.close().await.unwrap();
}
