use super::*;
use rusqlite::trace::{TraceEvent, TraceEventCodes};

#[derive(Debug, Default, PartialEq, Eq)]
struct Reads {
    transactions: usize,
    buckets: usize,
    policies: usize,
    bucket_acls: usize,
}

#[tokio::test]
async fn deletion_and_upload_bucket_authority_costs() {
    let data = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let mut record = bucket("bucket", "project");
    record.versioning = crate::bucket::Versioning::from_columns((true, false));
    catalog.insert_bucket(record).await.unwrap();
    let reads = Arc::new(Mutex::new(Reads::default()));
    let counts = reads.clone();
    catalog
        .run::<_, CatalogError>(move |connection| {
            // A stored public grant forces both the bucket ACL decode and the PAP check.
            sql::exec(
                connection,
                "INSERT OR REPLACE INTO acl_bucket_entries VALUES ('bucket','bucket',?1)",
                [r#"[{"entity":"allUsers","role":"WRITER"}]"#],
            )?;
            connection.trace_v2(
                TraceEventCodes::SQLITE_TRACE_STMT,
                Some(move |event: TraceEvent<'_>| {
                    if let TraceEvent::Stmt(stmt, query) = event {
                        let mut counts = counts.lock().unwrap();
                        counts.transactions += usize::from(query.starts_with("BEGIN"));
                        counts.buckets += usize::from(
                            query.starts_with("SELECT")
                                && query.contains(" FROM buckets WHERE name"),
                        );
                        counts.policies +=
                            usize::from(query.contains(" FROM bucket_policies WHERE"));
                        counts.bucket_acls += usize::from(
                            query.starts_with("SELECT entries FROM acl_bucket_entries")
                                && stmt.expanded_sql().is_some_and(|query| {
                                    query.contains("'bucket'") && !query.contains("'defaultObject'")
                                }),
                        );
                    }
                }),
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let (writes, pooled) = catalog.job_counts();
    publish(
        &catalog,
        "object",
        catalog.admit_object_upload().unwrap(),
        BucketAccess::Permissive,
    )
    .await
    .unwrap();
    assert_eq!(catalog.job_counts(), (writes + 1, pooled));
    assert_eq!(
        std::mem::take(&mut *reads.lock().unwrap()),
        Reads {
            transactions: 1,
            buckets: 4,
            ..Default::default()
        }
    );
    let access = BucketAccess::Policy {
        identity: None,
        permission: crate::auth::OBJECTS_DELETE,
        project: Some("project".into()),
    };
    catalog
        .delete_object(live("bucket", "object"), Default::default(), &access)
        .await
        .unwrap();
    assert_eq!(catalog.job_counts(), (writes + 2, pooled));
    assert_eq!(
        *reads.lock().unwrap(),
        Reads {
            transactions: 2,
            buckets: 9,
            policies: 2,
            bucket_acls: 4
        }
    );
    catalog.close().await.unwrap();
}
