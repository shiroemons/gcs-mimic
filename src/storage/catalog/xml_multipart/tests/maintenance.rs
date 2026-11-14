//! What one maintenance pass reads, which runs after every part request.

use super::*;

async fn orphan_every_upload(catalog: &Catalog) {
    catalog
        .run::<_, CatalogError>(|connection| {
            sql::exec(
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
async fn expired_uploads_are_found_through_the_deadline_index() {
    let (_dir, catalog) = setup(Arc::new(SystemClock)).await;
    let plan = catalog
        .run::<_, CatalogError>(|connection| {
            let mut statement = connection
                .prepare(&format!(
                    "EXPLAIN QUERY PLAN {}",
                    super::super::maintenance::EXPIRED_PAGE
                ))
                .unwrap();
            let rows = statement
                .query_map(params![0, 0, ""], |row| row.get::<_, String>(3))
                .unwrap();
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>().unwrap())
        })
        .await
        .unwrap();
    assert!(
        plan.iter()
            .any(|step| step.contains("xml_multipart_uploads_deadline")),
        "{plan:?}"
    );
    assert!(
        !plan.iter().any(|step| step.starts_with("SCAN")),
        "{plan:?}"
    );
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
    let at_deadline = now.checked_add_millis(LIFETIME_MILLIS).unwrap();
    catalog.maintain_xml_multipart(at_deadline).await.unwrap();
    assert_eq!(upload_count(&catalog).await, 0);

    begin(&catalog).await;
    orphan_every_upload(&catalog).await;
    catalog.close().await.unwrap();
    let reopened = Catalog::open_with_clock(&dir.0, clock).await.unwrap();
    assert_eq!(upload_count(&reopened).await, 0);
    reopened.close().await.unwrap();
}
