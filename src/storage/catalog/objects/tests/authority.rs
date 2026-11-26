use super::*;
use rusqlite::trace::{TraceEvent, TraceEventCodes};

#[tokio::test]
async fn delete_checks_existence_authority_conditions_and_retention_in_order() {
    let data = crate::test_support::TestDir::new();
    let clock = Arc::new(crate::clock::ManualClock::new(
        UNIX_EPOCH + std::time::Duration::from_secs(1000),
    ));
    let catalog = Catalog::open_with_clock(&data.0, clock.clone())
        .await
        .unwrap();
    let mut record = bucket("bucket", "project");
    record.uniform_bucket_level_access_locked_time = Some(clock.now());
    record.retention_policy = Some(crate::bucket::RetentionPolicy {
        retention_period: 60,
        effective_time: clock.now(),
        is_locked: true,
    });
    catalog.insert_bucket(record).await.unwrap();
    let object = put(&catalog, "object", b"object").await;
    let target = ObjectTarget::new("bucket", "object", Some(object.generation.get()));
    let mut upload = permissive_upload("bucket", "held", 100);
    upload.metadata.temporary_hold = Some(true);
    catalog.put_object(upload, body(b"held")).await.unwrap();
    let anonymous = BucketAccess::Policy {
        identity: None,
        permission: crate::auth::OBJECTS_DELETE,
        project: Some("project".into()),
    };
    let strict = policy_access("project", Some("project"), true)
        .with_permission(crate::auth::OBJECTS_DELETE);
    let wrong_generation = ObjectConditions {
        if_generation_match: Some(0),
        ..Default::default()
    };
    let wrong_metadata = ObjectConditions {
        if_metageneration_match: Some(2),
        ..Default::default()
    };
    for access in [PERMISSIVE, &strict, &anonymous] {
        assert_eq!(
            catalog
                .delete_object(live("absent", "object"), wrong_generation, access)
                .await,
            Err(ObjectError::BucketNotFound)
        );
    }
    for target in [target.clone(), live("bucket", "absent")] {
        assert!(matches!(
            catalog
                .delete_object(target, wrong_metadata, &anonymous)
                .await,
            Err(ObjectError::Denied(_))
        ));
    }
    for access in [PERMISSIVE, &strict] {
        assert_eq!(
            catalog
                .delete_object(live("bucket", "absent"), wrong_generation, access)
                .await,
            Err(ObjectError::ObjectNotFound)
        );
        for conditions in [wrong_generation, wrong_metadata] {
            assert_eq!(
                catalog
                    .delete_object(target.clone(), conditions, access)
                    .await,
                Err(ObjectError::PreconditionFailed)
            );
        }
        assert!(matches!(
            catalog
                .delete_object(target.clone(), Default::default(), access)
                .await,
            Err(ObjectError::ObjectRetentionNotMet(_))
        ));
        assert!(matches!(
            catalog
                .delete_object(live("bucket", "held"), Default::default(), access)
                .await,
            Err(ObjectError::ActiveHold(_))
        ));
    }
    catalog
        .run::<_, CatalogError>(|connection| {
            sql::exec_on_writer(
                connection,
                "INSERT OR REPLACE INTO acl_bucket_entries VALUES ('bucket','bucket',?1)",
                [r#"[{"entity":"allUsers","role":"WRITER"}]"#],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    clock.advance(std::time::Duration::from_secs(60));
    catalog
        .configure_public_access_prevention(&["project".into()])
        .await
        .unwrap();
    assert!(matches!(
        catalog
            .delete_object(target.clone(), wrong_metadata, &anonymous)
            .await,
        Err(ObjectError::Denied(_))
    ));
    catalog
        .configure_public_access_prevention(&[])
        .await
        .unwrap();
    catalog
        .delete_object(target, Default::default(), &anonymous)
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn corrupt_publication_settings_preserve_denials_and_missing_objects() {
    let data = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    create_bucket(&catalog, "project").await;
    put(&catalog, "live", b"body").await;
    let denied = BucketAccess::Policy {
        identity: None,
        permission: crate::auth::OBJECTS_DELETE,
        project: None,
    };
    for corrupt in [
        "UPDATE buckets SET autoclass='broken' WHERE name='bucket'",
        "UPDATE buckets SET autoclass=NULL, soft_delete_policy='broken' WHERE name='bucket'",
    ] {
        catalog
            .run::<_, CatalogError>(move |connection| {
                sql::exec_on_writer(connection, corrupt, [])?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(matches!(
            catalog
                .delete_object(live("bucket", "live"), Default::default(), &denied)
                .await,
            Err(ObjectError::Denied(_))
        ));
        assert_eq!(
            catalog
                .delete_object(live("bucket", "absent"), Default::default(), PERMISSIVE)
                .await,
            Err(ObjectError::ObjectNotFound)
        );
        assert_eq!(
            catalog
                .delete_object(
                    live("bucket", "live"),
                    ObjectConditions {
                        if_metageneration_match: Some(2),
                        ..Default::default()
                    },
                    PERMISSIVE
                )
                .await,
            Err(ObjectError::PreconditionFailed)
        );
        assert!(
            catalog
                .delete_object(live("bucket", "live"), Default::default(), PERMISSIVE)
                .await
                .is_err()
        );
    }
    catalog.close().await.unwrap();
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Reads {
    transactions: usize,
    buckets: usize,
    policies: usize,
    bucket_acls: usize,
    // A post-change regression check, outside the before/after authority counts.
    retention_subqueries: usize,
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
            sql::exec_on_writer(
                connection,
                "INSERT OR REPLACE INTO acl_bucket_entries VALUES ('bucket','bucket',?1)",
                [r#"[{"entity":"allUsers","role":"WRITER"}]"#],
            )?;
            connection.trace_v2(
                TraceEventCodes::SQLITE_TRACE_STMT,
                Some(move |event: TraceEvent<'_>| {
                    if let TraceEvent::Stmt(stmt, query) = event {
                        let mut counts = crate::sync::lock(&counts);
                        counts.transactions += usize::from(query.starts_with("BEGIN"));
                        counts.buckets += usize::from(
                            query.starts_with("SELECT")
                                && query.contains(" FROM buckets WHERE name"),
                        );
                        counts.retention_subqueries += usize::from(
                            query.contains("FROM buckets WHERE buckets.name = objects.bucket"),
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
        std::mem::take(&mut *crate::sync::lock(&reads)),
        Reads {
            transactions: 1,
            buckets: 1,
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
        *crate::sync::lock(&reads),
        Reads {
            transactions: 1,
            buckets: 1,
            policies: 1,
            bucket_acls: 1,
            retention_subqueries: 0
        }
    );
    catalog.close().await.unwrap();
}
