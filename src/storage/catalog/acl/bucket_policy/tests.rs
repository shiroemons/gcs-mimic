use super::*;
use crate::{
    auth::{Auth, Registry},
    storage::Catalog,
    test_support::TestDir,
};
use serde_json::json;

async fn fixture() -> (TestDir, Catalog) {
    let data = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let bucket =
        crate::json::parse_insert(br#"{"name":"bucket"}"#, "project", std::time::UNIX_EPOCH)
            .unwrap();
    catalog.insert_bucket(bucket).await.unwrap();
    (data, catalog)
}

#[tokio::test]
async fn legacy_reconciliation_preserves_existing_iam_bindings_and_permission_union() {
    let (_data, catalog) = fixture().await;
    let auth =
        Auth::strict(Registry::parse(br#"{"identities":[{"id":"reader","tokens":[]}]}"#).unwrap());
    let identity = auth.principals().unwrap()[0].clone();
    catalog
        .run(move |connection| {
            let transaction = connection.transaction().unwrap();
            let mut policy = Policy {
                bindings: vec![Binding {
                    role: "roles/storage.legacyBucketWriter".into(),
                    members: vec!["user:reader".into()],
                }],
                etag: None,
            };
            policies::store_policy(&transaction, "bucket", &mut policy).unwrap();
            transaction
                .execute(
                    UPSERT_BUCKET_ENTRIES,
                    params![
                        "bucket",
                        "bucket",
                        json!([{"entity":"user-reader","role":"OWNER"}]).to_string()
                    ],
                )
                .unwrap();
            transaction
                .execute(
                    "UPDATE buckets SET acl_policy_bound=0 WHERE name='bucket'",
                    [],
                )
                .unwrap();
            reconcile(&transaction, "bucket", &Profile::TrustedLocal, 1).unwrap();
            let union = policies::read_policy(&transaction, "bucket").unwrap();
            assert!(union.allows(
                Some(&identity),
                "storage.managedFolders.update",
                crate::auth::PublicAccess::Allowed
            ));
            let owner = crate::auth::roles::permissions("roles/storage.legacyBucketOwner").unwrap();
            for permission in
                crate::auth::roles::permissions("roles/storage.legacyBucketWriter").unwrap()
            {
                assert!(owner.contains(permission));
            }
            assert!(
                union
                    .bindings
                    .iter()
                    .any(|binding| binding.role == "roles/storage.legacyBucketWriter")
            );
            assert!(
                union
                    .bindings
                    .iter()
                    .any(|binding| binding.role == "roles/storage.legacyBucketOwner")
            );
            transaction.commit().unwrap();
            Ok(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn legacy_reconciliation_rejects_invalid_or_oversized_acls_and_rolls_back_partial_writes() {
    let (_data, catalog) = fixture().await;
    catalog.run(|connection| {
        for case in 0..3 {
            let entries = match case {
                0 => (0..100).map(|index| json!({"entity":format!("user-{index}"),"role":"READER"})).collect::<Vec<_>>(),
                1 => vec![json!({"entity":"user-bad-id","role":"OWNER"})],
                _ => vec![json!({"entity":"user-reader","role":"OWNER"})],
            };
            let before_acl = serde_json::to_string(&entries).unwrap();
            connection.execute(UPSERT_BUCKET_ENTRIES, params!["bucket", "bucket", before_acl]).unwrap();
            connection.execute("UPDATE buckets SET acl_policy_bound=0 WHERE name='bucket'", []).unwrap();
            let before_policy = policies::read_policy(connection, "bucket").unwrap();
            if case == 2 {
                connection.execute_batch("CREATE TEMP TRIGGER reject_policy_update BEFORE UPDATE ON bucket_policies BEGIN SELECT RAISE(ABORT,'injected policy update failure'); END;").unwrap();
            }
            let transaction = connection.transaction().unwrap();
            assert!(reconcile(&transaction, "bucket", &Profile::TrustedLocal, 1).is_err());
            if case == 2 {
                let staged = stored_bucket_entries(&transaction, "bucket", "bucket").unwrap().unwrap();
                assert_ne!(serde_json::to_string(&staged).unwrap(), before_acl);
            }
            drop(transaction);
            let after = stored_bucket_entries(connection, "bucket", "bucket").unwrap().unwrap();
            assert_eq!(serde_json::to_string(&after).unwrap(), before_acl);
            assert_eq!(policies::read_policy(connection, "bucket").unwrap(), before_policy);
            assert!(!bucket_row(connection, "bucket").unwrap().unwrap().policy_bound);
        }
        Ok(())
    }).await.unwrap();
    catalog.close().await.unwrap();
}
