use super::*;
use crate::{auth::Permission, bucket::BucketAccess, storage::ResumableOptions};

/// Runs the same assertion against the normal reader, the former serialized route and a
/// reader whose database cannot prepare the query. Each count belongs to this catalog only.
pub(in crate::storage::catalog) async fn assert_point_read(
    catalog: &Catalog,
    check: impl AsyncFn(),
) {
    let (writes, reads) = catalog.job_counts();
    check().await;
    assert_eq!(catalog.job_counts(), (writes, reads + 1));

    // A held lease forces the exclusive job to reserve the gate while leaving the writer free.
    let lease = catalog.activity().unwrap();
    let exclusive = catalog.wait_exclusive(LONG_LIMIT.deadline());
    tokio::pin!(exclusive);
    tokio::select! {
        biased;
        _ = &mut exclusive => panic!("the held lease must keep the reservation pending"),
        () = std::future::ready(()) => {}
    }
    assert!(catalog.0.activity.is_reserved());
    check().await;
    assert_eq!(catalog.job_counts(), (writes + 1, reads + 1));
    drop(lease);
    drop(exclusive.await.unwrap());

    let idle = &pool(catalog).shared;
    let empty = ReaderConnection::open(std::path::Path::new(":memory:")).unwrap();
    let original = idle.idle().last_mut().unwrap().connection.replace(empty);
    check().await;
    idle.idle().last_mut().unwrap().connection = original;
    assert_eq!(catalog.job_counts(), (writes + 2, reads + 2));
    assert_eq!(catalog.readonly_answers(), 0);
}

#[tokio::test]
async fn permission_reads_see_the_new_grant() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    let access = BucketAccess::Policy {
        identity: None,
        permission: Permission::ObjectsGet,
        project: Some("project".into()),
    };
    assert!(
        !catalog
            .has_bucket_permission("bucket", &access)
            .await
            .unwrap()
    );
    set_members(&catalog, "roles/storage.objectViewer", "allUsers").await;
    assert_point_read(&catalog, async || {
        assert!(
            catalog
                .has_bucket_permission("bucket", &access)
                .await
                .unwrap()
        );
    })
    .await;
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn policy_reads_see_the_committed_revision() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    let mut policy = catalog.get_policy("bucket", PERMISSIVE).await.unwrap();
    policy.bindings.clear();
    let written = catalog
        .set_policy("bucket", policy, PERMISSIVE)
        .await
        .unwrap();
    assert_point_read(&catalog, async || {
        assert_eq!(
            catalog.get_policy("bucket", PERMISSIVE).await.unwrap(),
            written
        );
    })
    .await;
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn bucket_pages_include_a_new_bucket() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let written = catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    assert_point_read(&catalog, async || {
        let page = catalog
            .list_buckets_page_among(Some(vec!["project".into()]), None, None, 1)
            .await
            .unwrap();
        assert_eq!(page.items.as_slice(), std::slice::from_ref(&written));
        assert!(!page.has_more);
    })
    .await;
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn session_ids_read_the_committed_upload() {
    let dir = TestDir::new();
    let catalog = open_with_object(&dir).await;
    let written = catalog
        .create_resumable_session(
            "bucket",
            "resumed",
            ResumableOptions::new(BucketAccess::Permissive),
            Some(7),
        )
        .await
        .unwrap();
    let id = written.id.to_string();
    assert_point_read(&catalog, async || {
        assert_eq!(
            catalog.resumable_session(&id, "bucket").await.unwrap(),
            written
        );
    })
    .await;
    catalog.close().await.unwrap();
}
