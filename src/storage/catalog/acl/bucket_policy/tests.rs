use super::*;
use crate::{
    auth::{Auth, Registry},
    storage::Catalog,
    test_support::TestDir,
};
use serde_json::json;

#[test]
fn overlapping_legacy_roles_keep_the_highest_grant_in_both_orders() {
    for roles in [
        [
            "roles/storage.legacyBucketReader",
            "roles/storage.legacyBucketOwner",
        ],
        [
            "roles/storage.legacyBucketOwner",
            "roles/storage.legacyBucketReader",
        ],
        [
            "roles/storage.legacyBucketReader",
            "roles/storage.legacyBucketWriter",
        ],
        [
            "roles/storage.legacyBucketWriter",
            "roles/storage.legacyBucketReader",
        ],
        [
            "roles/storage.legacyBucketWriter",
            "roles/storage.legacyBucketOwner",
        ],
        [
            "roles/storage.legacyBucketOwner",
            "roles/storage.legacyBucketWriter",
        ],
    ] {
        let policy = Policy {
            bindings: roles
                .iter()
                .map(|role| Binding {
                    role: (*role).into(),
                    members: vec!["allUsers".into()],
                })
                .collect(),
            etag: None,
        };
        let entries = policy_entries(&policy, "project", None);
        let expected = if roles.contains(&"roles/storage.legacyBucketOwner") {
            "OWNER"
        } else {
            "WRITER"
        };
        assert_eq!(
            serde_json::to_string(&entries).unwrap(),
            format!(r#"[{{"entity":"allUsers","role":"{expected}"}}]"#),
            "{roles:?}"
        );
    }
}

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
        .run::<_, crate::bucket::CatalogError>(move |connection| {
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
                crate::auth::OBJECTS_CREATE,
                crate::auth::PublicAccess::Allowed
            ));
            // Reconciliation must also preserve snapshot grants outside modeled operations.
            assert!(union.bindings.iter().any(|binding| {
                binding.members.contains(&"user:reader".to_owned())
                    && crate::auth::roles::permissions(&binding.role).is_some_and(|permissions| {
                        permissions.contains(&"storage.managedFolders.update")
                    })
            }));
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
    catalog.run::<_, crate::bucket::CatalogError>(|connection| {
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
                let staged = stored_bucket_entries(&transaction, "bucket", AclKind::Bucket).unwrap().unwrap();
                assert_ne!(serde_json::to_string(&staged).unwrap(), before_acl);
            }
            drop(transaction);
            let after = stored_bucket_entries(connection, "bucket", AclKind::Bucket).unwrap().unwrap();
            assert_eq!(serde_json::to_string(&after).unwrap(), before_acl);
            assert_eq!(policies::read_policy(connection, "bucket").unwrap(), before_policy);
            assert!(!bucket_row(connection, "bucket").unwrap().unwrap().policy_bound);
        }
        Ok(())
    }).await.unwrap();
    catalog.close().await.unwrap();
}

fn occurrences(policy: &Policy) -> usize {
    policy
        .bindings
        .iter()
        .map(|binding| binding.members.len())
        .sum()
}

/// An entity naming the bucket's own project by ID maps to a member whose canonical entity names
/// the project by number, so the edit that removes the entry leaves the member bound. Adding the
/// entry again must not bind it a second time, or a repeated edit would grow the policy for ever.
#[tokio::test]
async fn repeated_acl_edits_do_not_grow_the_policy() {
    let (_data, catalog) = fixture().await;
    let sizes = catalog
        .run::<_, crate::bucket::CatalogError>(|connection| {
            let transaction = connection.transaction().unwrap();
            let without_entry = project_private("project");
            let mut with_entry = without_entry.clone();
            with_entry.push(AclEntry {
                entity: "project-owners-project".into(),
                role: AclRole::Reader,
            });
            let mut sizes = Vec::new();
            for entries in [&with_entry, &without_entry].repeat(8) {
                replace_policy(&transaction, "bucket", entries, None).unwrap();
                transaction
                    .execute(
                        UPSERT_BUCKET_ENTRIES,
                        params!["bucket", "bucket", serde_json::to_string(entries).unwrap()],
                    )
                    .unwrap();
                sizes.push(occurrences(
                    &policies::read_policy(&transaction, "bucket").unwrap(),
                ));
            }
            Ok(sizes)
        })
        .await
        .unwrap();
    assert_eq!(sizes.iter().min(), sizes.iter().max(), "{sizes:?}");
    catalog.close().await.unwrap();
}

/// Every writer stores through one check, including those no API request validated.
#[tokio::test]
async fn stored_policies_are_refused_as_client_errors_past_their_bounds() {
    let (_data, catalog) = fixture().await;
    catalog
        .run::<_, crate::bucket::CatalogError>(|connection| {
            let transaction = connection.transaction().unwrap();
            let members = |count: usize, length: usize| Policy {
                bindings: vec![Binding {
                    role: "roles/storage.objectViewer".into(),
                    members: (0..count)
                        .map(|index| format!("user:{index:04}{}", "x".repeat(length)))
                        .collect(),
                }],
                etag: None,
            };
            // The documented principals with the longest names, plus one full ACL.
            let mut largest = members(1600, 250);
            policies::store_policy(&transaction, "bucket", &mut largest).unwrap();
            for mut refused in [members(1601, 1), members(2, 1024 * 1024)] {
                let before = policies::read_policy(&transaction, "bucket").unwrap();
                assert!(matches!(
                    policies::store_policy(&transaction, "bucket", &mut refused),
                    Err(crate::bucket::CatalogError::InvalidMetadata(_))
                ));
                assert_eq!(
                    policies::read_policy(&transaction, "bucket").unwrap(),
                    before
                );
                assert!(matches!(
                    policies::insert_policy(&transaction, "other", &refused),
                    Err(crate::bucket::CatalogError::InvalidMetadata(_))
                ));
            }
            Ok(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
}
