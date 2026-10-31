use super::*;
use crate::{
    auth::Registry, bucket::BucketAccess, clock::SystemClock, storage::admin::LocalAdmin,
    test_support::TestDir,
};
use rusqlite::params;
use serde_json::json;

async fn bucket(catalog: &Catalog, name: &str, project: &str) {
    let record = crate::json::parse_insert(
        json!({"name":name}).to_string().as_bytes(),
        project,
        std::time::UNIX_EPOCH,
    )
    .unwrap();
    catalog.insert_bucket(record).await.unwrap();
}

#[tokio::test]
async fn empty_catalog_installs_each_authentication_profile_without_a_write_transaction() {
    for auth in [
        Auth::permissive(),
        Auth::strict(
            Registry::parse(br#"{"identities":[{"id":"reader@example.test","tokens":[] }]}"#)
                .unwrap(),
        ),
    ] {
        let data = TestDir::new();
        let catalog =
            Catalog::open_for_server(&data.0, Arc::new(SystemClock), LocalAdmin::Disabled)
                .await
                .unwrap();
        let writer = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        catalog.configure_authentication(&auth).await.unwrap();
        assert_eq!(
            matches!(catalog.0.principals.get(), Some(Profile::Strict(_))),
            auth.principals().is_some()
        );
        assert!(catalog.0.principals.get().is_some());
        assert!(matches!(
            catalog.configure_authentication(&auth).await,
            Err(CatalogError::AuthenticationAlreadyConfigured)
        ));
        writer.execute_batch("ROLLBACK").unwrap();
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn an_orphan_object_cannot_take_the_empty_catalog_authentication_path() {
    let data = TestDir::new();
    let catalog = Catalog::open_for_server(&data.0, Arc::new(SystemClock), LocalAdmin::Disabled)
        .await
        .unwrap();
    catalog
        .run(|connection| {
            connection.execute_batch("PRAGMA foreign_keys=OFF; INSERT INTO objects(bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id) VALUES ('missing-bucket','orphan',1,1,'text/plain',0,zeroblob(16),0,'STANDARD',0,0,'00000000000000000000000000000001'); PRAGMA foreign_keys=ON;").unwrap();
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        catalog.configure_authentication(&Auth::permissive()).await,
        Err(CatalogError::Corrupt(reason)) if reason == "object records exist without a bucket"
    ));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn partial_startup_reconciliation_does_not_union_an_already_bound_bucket_again() {
    let data = TestDir::new();
    let catalog = Catalog::open_for_server(&data.0, Arc::new(SystemClock), LocalAdmin::Disabled)
        .await
        .unwrap();
    bucket(&catalog, "first-bucket", "project").await;
    bucket(&catalog, "second-bucket", "project").await;
    let before = catalog.run(|connection| {
        let transaction = connection.transaction().unwrap();
        let mut policy = crate::auth::Policy { bindings:vec![crate::auth::Binding { role:"roles/storage.legacyBucketReader".into(), members:vec!["user:local:reader-user".into()] }],etag:None };
        crate::storage::catalog::policies::store_policy(&transaction, "first-bucket", &mut policy).unwrap();
        for (name, entity) in [("first-bucket", "user-reader@example.test"), ("second-bucket", "user-invalid-id")] {
            transaction.execute("INSERT INTO acl_bucket_entries(bucket,scope,entries) VALUES(?1,'bucket',?2)", params![name, json!([{"entity":entity,"role":"READER"}]).to_string()]).unwrap();
        }
        transaction.execute("UPDATE buckets SET acl_policy_bound=0", []).unwrap();
        transaction.commit().unwrap();
        Ok(policy)
    }).await.unwrap();
    let original = Auth::strict(Registry::parse(br#"{"identities":[{"id":"local:reader-user","tokens":[],"acl_entity":"user-reader@example.test"},{"id":"other@example.test","tokens":[]}]}"#).unwrap());
    assert!(catalog.configure_authentication(&original).await.is_err());
    catalog
        .run(|connection| {
            let flags: Vec<i64> = connection
                .prepare("SELECT acl_policy_bound FROM buckets ORDER BY name")
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert_eq!(flags, [1, 0]);
            Ok(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
    {
        let connection = rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
        connection.execute("UPDATE acl_bucket_entries SET entries='[]' WHERE bucket='second-bucket' AND scope='bucket'", []).unwrap();
    }
    let current = Auth::strict(Registry::parse(br#"{"identities":[{"id":"local:reader-user","tokens":[],"acl_entity":"user-new@example.test"},{"id":"other@example.test","tokens":[],"acl_entity":"user-reader@example.test"}]}"#).unwrap());
    let reopened = Catalog::open_for_server(&data.0, Arc::new(SystemClock), LocalAdmin::Disabled)
        .await
        .unwrap();
    reopened.configure_authentication(&current).await.unwrap();
    assert!(
        reopened
            .bucket_access(
                "first-bucket",
                Some(&current.principals().unwrap()[0]),
                crate::auth::BUCKETS_GET
            )
            .await
            .is_ok()
    );
    assert_eq!(
        reopened
            .bucket_access(
                "first-bucket",
                Some(&current.principals().unwrap()[1]),
                crate::auth::BUCKETS_GET
            )
            .await,
        Err(CatalogError::Forbidden)
    );
    assert_eq!(
        reopened
            .get_policy("first-bucket", &BucketAccess::Permissive)
            .await
            .unwrap(),
        before
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn new_and_recreated_buckets_start_bound_without_inheriting_old_grants() {
    let data = TestDir::new();
    let catalog = Catalog::open_for_server(&data.0, Arc::new(SystemClock), LocalAdmin::Disabled)
        .await
        .unwrap();
    catalog
        .configure_authentication(&Auth::permissive())
        .await
        .unwrap();
    bucket(&catalog, "bucket", "project").await;
    let original = catalog.run(|connection| {
        let bound: bool = connection.query_row("SELECT acl_policy_bound FROM buckets WHERE name='bucket'", [], |row| row.get(0)).unwrap();
        assert!(bound);
        let absent: bool = connection.query_row("SELECT NOT EXISTS(SELECT 1 FROM acl_bucket_entries WHERE bucket='bucket' AND scope='bucket')", [], |row| row.get(0)).unwrap();
        assert!(absent);
        let incarnation: String = connection.query_row("SELECT incarnation FROM bucket_incarnations WHERE bucket='bucket'", [], |row| row.get(0)).unwrap();
        connection.execute("UPDATE buckets SET acl_policy_bound=0 WHERE name='bucket'", []).unwrap();
        Ok(incarnation)
    }).await.unwrap();
    catalog.delete_bucket("bucket", None, None).await.unwrap();
    bucket(&catalog, "bucket", "another-project").await;
    catalog.run(move |connection| {
        let (bound, incarnation): (bool, String) = connection.query_row("SELECT acl_policy_bound,incarnation FROM buckets JOIN bucket_incarnations ON name=bucket WHERE name='bucket'", [], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert!(bound);
        assert_ne!(incarnation, original);
        Ok(())
    }).await.unwrap();
    let policy = catalog
        .get_policy("bucket", &BucketAccess::Permissive)
        .await
        .unwrap();
    assert!(
        policy
            .bindings
            .iter()
            .flat_map(|binding| &binding.members)
            .all(|member| member.ends_with(":another-project"))
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn old_bucket_without_stored_acl_does_not_invent_virtual_default_grants() {
    let data = TestDir::new();
    let catalog = Catalog::open_for_server(&data.0, Arc::new(SystemClock), LocalAdmin::Disabled)
        .await
        .unwrap();
    bucket(&catalog, "bucket", "project").await;
    let before = catalog
        .run(|connection| {
            let transaction = connection.transaction().unwrap();
            let mut policy = crate::auth::Policy {
                bindings: vec![],
                etag: None,
            };
            crate::storage::catalog::policies::store_policy(&transaction, "bucket", &mut policy)
                .unwrap();
            transaction
                .execute(
                    "UPDATE buckets SET acl_policy_bound=0 WHERE name='bucket'",
                    [],
                )
                .unwrap();
            transaction.commit().unwrap();
            Ok(policy)
        })
        .await
        .unwrap();
    catalog
        .configure_authentication(&Auth::permissive())
        .await
        .unwrap();
    assert_eq!(
        catalog
            .get_policy("bucket", &BucketAccess::Permissive)
            .await
            .unwrap(),
        before
    );
    let acl = catalog
        .read_acl(
            crate::acl::AclTarget::Bucket("bucket".into()),
            None,
            None,
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert!(acl.entries.is_empty());
    catalog.close().await.unwrap();
    let reopened = Catalog::open_for_server(&data.0, Arc::new(SystemClock), LocalAdmin::Disabled)
        .await
        .unwrap();
    reopened
        .configure_authentication(&Auth::permissive())
        .await
        .unwrap();
    assert_eq!(
        reopened
            .get_policy("bucket", &BucketAccess::Permissive)
            .await
            .unwrap(),
        before
    );
    let acl = reopened
        .read_acl(
            crate::acl::AclTarget::Bucket("bucket".into()),
            None,
            None,
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert!(acl.entries.is_empty());
    reopened.close().await.unwrap();
}
