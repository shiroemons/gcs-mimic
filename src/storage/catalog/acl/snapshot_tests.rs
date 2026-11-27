use super::super::read_pool::tests::assert_point_read;
use super::*;
use crate::storage::test_fixtures::{PERMISSIVE, body, live};
use crate::{
    auth::{Auth, Registry},
    clock::SystemClock,
    object::{ExpectedHashes, ObjectMetadataUpdate},
    storage::{ObjectUpload, admin::LocalAdmin},
    test_support::TestDir,
};
use bytes::Bytes;
use std::sync::Arc;

#[test]
fn malformed_stored_acl_keeps_the_json_error_source() {
    let error = decode("[", AclKind::Bucket).unwrap_err();
    assert!(crate::error::causes(&error).any(<dyn std::error::Error>::is::<serde_json::Error>));
}

#[test]
fn public_policy_is_read_only_for_an_eligible_public_grant() {
    let private = [AclEntry {
        entity: "user-delegate@example.test".parse().unwrap(),
        role: AclRole::Owner,
    }];
    assert!(
        !entries_grant(
            &private,
            None,
            |_| true,
            || panic!("private entities do not consult public policy")
        )
        .unwrap()
    );
    let public = [
        AclEntry {
            entity: "allUsers".parse().unwrap(),
            role: AclRole::Reader,
        },
        AclEntry {
            entity: "allAuthenticatedUsers".parse().unwrap(),
            role: AclRole::Reader,
        },
    ];
    assert!(
        !entries_grant(
            &public,
            None,
            |_| false,
            || panic!("irrelevant roles do not consult public policy")
        )
        .unwrap()
    );
    let mut reads = 0;
    assert!(
        !entries_grant(
            &public,
            None,
            |_| true,
            || {
                reads += 1;
                Ok(crate::auth::PublicAccess::Prevented)
            }
        )
        .unwrap()
    );
    assert_eq!(reads, 1);
}

#[tokio::test]
async fn page_projection_preserves_generation_order_owner_and_access_restrictions() {
    let data = TestDir::new();
    let (catalog, auth, record) = setup(&data, "disk").await;
    let generation = record.generation;
    catalog.run::<_, CatalogError>(move |connection| {
        sql::exec_on_writer(connection, "UPDATE acl_object_entries SET entries=?1 WHERE bucket='bucket' AND name='object' AND generation=?2", params![r#"[{"entity":"allUsers","role":"OWNER"},{"entity":"user-delegate@example.test","role":"OWNER"}]"#, generation])?;
        Ok(())
    }).await.unwrap();
    let principals = auth.principals().unwrap();
    for (uniform, prevented) in [(false, false), (false, true), (true, false)] {
        catalog
            .patch_bucket(
                "bucket",
                crate::bucket::BucketMetadataUpdate {
                    uniform_bucket_level_access_enabled: Some(uniform.into()),
                    public_access_prevention: Some(if prevented {
                        crate::bucket::PublicAccessPrevention::Enforced
                    } else {
                        crate::bucket::PublicAccessPrevention::Inherited
                    }),
                    ..Default::default()
                },
                Default::default(),
                PERMISSIVE,
            )
            .await
            .unwrap();
        for identity in [None, Some(&principals[0]), Some(&principals[1])] {
            let access = catalog
                .bucket_access("bucket", identity, crate::auth::OBJECTS_GET_IAM)
                .await
                .unwrap();
            let projection = catalog
                .object_acl_projection(
                    "bucket",
                    vec![
                        ("object".into(), generation.get()),
                        ("missing".into(), generation.get()),
                        ("object".into(), generation.get() + 1),
                        ("object".into(), generation.get()),
                    ],
                    &access,
                )
                .await
                .unwrap()
                .unwrap();
            let checked = access.clone();
            let allowed = catalog
                .run::<_, CatalogError>(move |connection| {
                    match super::super::objects::authorize_generation(
                        connection,
                        crate::object::ObjectKey::from_parts_unchecked("bucket", "object"),
                        generation.get(),
                        &checked,
                    ) {
                        Ok(()) => Ok(true),
                        Err(ObjectError::Denied(_)) => Ok(false),
                        Err(error) => Err(unavailable(error)),
                    }
                })
                .await
                .unwrap();
            let visible: Vec<bool> = projection
                .objects
                .iter()
                .map(|acl| matches!(acl, ObjectAcl::Visible { .. }))
                .collect();
            assert_eq!(
                visible,
                [allowed, false, false, allowed],
                "uniform={uniform}, prevented={prevented}"
            );
            assert_eq!(projection.objects[0], projection.objects[3]);
            if allowed {
                assert!(matches!(
                    &projection.objects[0],
                    ObjectAcl::Visible { owner, .. } if *owner == record.owner
                ));
            }
            assert_eq!(projection.objects[1], ObjectAcl::Hidden);
            if let BucketAccess::Policy {
                identity,
                permission,
                ..
            } = access
            {
                let wrong_project = BucketAccess::Policy {
                    identity,
                    permission,
                    project: Some("other".into()),
                };
                assert_eq!(
                    catalog
                        .object_acl_projection(
                            "bucket",
                            vec![("object".into(), generation.get())],
                            &wrong_project
                        )
                        .await
                        .unwrap()
                        .unwrap()
                        .objects,
                    [ObjectAcl::Hidden]
                );
            }
        }
    }
    catalog.close().await.unwrap();
}

#[test]
fn unknown_stored_acl_role_is_catalog_corruption() {
    for role in ["unknown", "reader", "", "FULL_CONTROL"] {
        let encoded = serde_json::json!([{"entity":"allUsers","role":role}]).to_string();
        assert!(
            matches!(
                decode(&encoded, AclKind::Bucket),
                Err(CatalogError::Corrupt(_))
            ),
            "{role}"
        );
    }
}

/// Every writer validates entities, so a stored one of no valid form is corruption, as an
/// unknown stored role is.
#[test]
fn invalid_stored_acl_entity_is_catalog_corruption() {
    for entity in ["user-bad-id", "user-", "nobody", ""] {
        let encoded = serde_json::json!([{"entity":entity,"role":"READER"}]).to_string();
        let error = decode(&encoded, AclKind::Bucket).unwrap_err();
        assert!(matches!(error, CatalogError::Corrupt(_)), "{entity}");
        assert!(
            crate::error::causes(&error).any(<dyn std::error::Error>::is::<serde_json::Error>),
            "{entity}"
        );
    }
}

/// A release before 0.12.0 stored entities past today's 272-byte cap. Such a row still reads,
/// so its bucket stays usable, while writing the entity back is refused as any request is.
#[tokio::test]
async fn oversized_stored_acl_entity_stays_readable() {
    let oversized = format!("project-viewers-{}", "p".repeat(257));
    assert_eq!(oversized.len(), 273);
    let encoded = serde_json::json!([{"entity":oversized,"role":"OWNER"}]).to_string();
    let entries = decode(&encoded, AclKind::Bucket).unwrap();
    assert_eq!(entries[0].entity.as_str(), oversized);
    assert_eq!(serde_json::to_string(&entries).unwrap(), encoded);
    let data = TestDir::new();
    let (catalog, _, _) = setup(&data, "memory").await;
    let refused = catalog
        .run(move |connection| {
            let transaction = sql::immediate(connection)?;
            replace_resource_acl(
                &transaction,
                ResolvedAclTarget::Bucket {
                    bucket: "bucket",
                    scope: BucketScope::Bucket,
                },
                &entries,
                PERMISSIVE,
                None,
            )
        })
        .await;
    assert_eq!(
        refused,
        Err(CatalogError::InvalidMetadata(
            crate::acl::AclError::EntityTooLong.to_string()
        ))
    );
    catalog.close().await.unwrap();
}

#[test]
fn stored_writer_role_is_only_valid_for_bucket_acls() {
    let encoded = r#"[{"entity":"allUsers","role":"WRITER"}]"#;
    assert!(decode(encoded, AclKind::Bucket).is_ok());
    for kind in [AclKind::Object, AclKind::DefaultObject] {
        assert!(
            matches!(decode(encoded, kind), Err(CatalogError::Corrupt(_))),
            "{kind:?}"
        );
    }
}

async fn setup(data: &TestDir, backend: &str) -> (Catalog, Auth, crate::object::ObjectRecord) {
    let auth = Auth::strict(Registry::parse(br#"{"identities":[{"id":"uploader@example.test","tokens":[],"project_roles":{"local":["roles/storage.objectCreator"]}},{"id":"delegate@example.test","tokens":[]}]}"#).unwrap());
    use crate::storage::{CatalogBacking, CatalogOptions};
    let backing = if backend == "disk" {
        CatalogBacking::Disk(&data.0)
    } else {
        CatalogBacking::Memory {
            blob_bytes: 1024 * 1024,
            catalog_bytes: 1024 * 1024,
        }
    };
    let catalog = Catalog::open_with_options(
        backing,
        CatalogOptions::authenticated(Arc::new(SystemClock), LocalAdmin::Disabled, &auth),
    )
    .await
    .unwrap();
    catalog
        .insert_bucket(
            crate::json::parse_insert(br#"{"name":"bucket"}"#, "local", std::time::UNIX_EPOCH)
                .unwrap(),
        )
        .await
        .unwrap();
    let access = catalog
        .bucket_access(
            "bucket",
            Some(&auth.principals().unwrap()[0]),
            crate::auth::OBJECTS_CREATE,
        )
        .await
        .unwrap();
    let body = body(vec![Bytes::from_static(b"snapshot bytes")]);
    let record = catalog
        .put_object_prechecked(
            ObjectUpload {
                key: crate::object::ObjectKey::from_parts_unchecked("bucket", "object"),
                content_type: "text/plain".into(),
                expected: ExpectedHashes::default(),
                conditions: Default::default(),
                max_bytes: 1024,
                metadata: ObjectMetadataUpdate::default(),
                access,
            },
            body,
            catalog.admit_object_upload().unwrap(),
        )
        .await
        .unwrap();
    (catalog, auth, record)
}

#[tokio::test]
async fn acl_reads_and_object_projections_route_with_the_committed_grant() {
    let data = TestDir::new();
    let (catalog, auth, record) = setup(&data, "disk").await;
    let access = catalog
        .bucket_access(
            "bucket",
            Some(&auth.principals().unwrap()[1]),
            crate::auth::OBJECTS_GET_IAM,
        )
        .await
        .unwrap();
    let generation = record.generation;
    let target = AclTarget::Object {
        bucket: "bucket".into(),
        name: "object".into(),
        generation: Some(generation.get()),
    };
    let (state, _) = catalog
        .change_acl(
            target.clone(),
            AclChange::Upsert {
                entity: "user-delegate@example.test".parse().unwrap(),
                role: Some(AclRole::Owner),
            },
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_point_read(&catalog, async || {
        assert_eq!(
            catalog
                .read_acl(target.clone(), Default::default(), &access)
                .await
                .unwrap(),
            state
        );
    })
    .await;
    let generations = || vec![("object".into(), generation.get())];
    assert_point_read(&catalog, async || {
        let projection = catalog
            .acl_projection("bucket", generations())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(projection.objects[0].entries(), state.entries);
    })
    .await;
    assert_point_read(&catalog, async || {
        let projection = catalog
            .object_acl_projection("bucket", generations(), &access)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(projection.objects[0].entries(), state.entries);
    })
    .await;
    let incarnation = catalog
        .run::<_, CatalogError>(|connection| Ok(bucket_incarnation(connection, "bucket")?))
        .await
        .unwrap();
    let snapshot = ObjectAclSnapshot::new(&record, state.entries.clone(), incarnation);
    // Replay appends the immutable owner's grant after the stored explicit entries.
    let mut replay_entries = snapshot.entries.clone();
    replay_entries.push(AclEntry {
        entity: "user-uploader@example.test".parse().unwrap(),
        role: AclRole::Owner,
    });
    assert_point_read(&catalog, async || {
        let visible = catalog
            .object_acl_snapshot(snapshot.clone(), &access)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(visible.entries, replay_entries);
        assert_eq!(visible.owner, record.owner);
    })
    .await;
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn bulk_bucket_projections_route_with_the_committed_default_acl() {
    let data = TestDir::new();
    let (catalog, _, _) = setup(&data, "disk").await;
    let (state, _) = catalog
        .change_acl(
            AclTarget::DefaultObject("bucket".into()),
            AclChange::Upsert {
                entity: "user-delegate@example.test".parse().unwrap(),
                role: Some(AclRole::Reader),
            },
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_point_read(&catalog, async || {
        let result = catalog
            .bucket_acl_projections(
                vec!["missing".into(), "bucket".into()],
                PERMISSIVE,
                MissingBucket::Omit,
            )
            .await
            .unwrap();
        let [None, Some(acl)] = result.as_slice() else {
            panic!("expected projections in request order");
        };
        assert_eq!(acl.default_object, state.entries);
        assert_eq!(acl.bucket_metageneration, state.metageneration);
    })
    .await;
    catalog.close().await.unwrap();
}

/// An upsert of the owner's own entity answers with the OWNER grant that ownership keeps,
/// not the role the change asked for; any other entity keeps the role it asked for.
#[tokio::test]
async fn owner_entity_upsert_returns_the_preserved_owner_grant() {
    let data = TestDir::new();
    let (catalog, _, record) = setup(&data, "memory").await;
    let target = AclTarget::Object {
        bucket: "bucket".into(),
        name: "object".into(),
        generation: Some(record.generation.get()),
    };
    let upsert = |entity: &str| AclChange::Upsert {
        entity: entity.parse().unwrap(),
        role: Some(AclRole::Reader),
    };
    let owner_grant = AclEntry {
        entity: "user-uploader@example.test".parse().unwrap(),
        role: AclRole::Owner,
    };
    let (state, changed) = catalog
        .change_acl(
            target.clone(),
            upsert("user-uploader@example.test"),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(changed, Some(owner_grant.clone()));
    assert!(state.entries.contains(&owner_grant));
    let reader = AclEntry {
        entity: "user-reader@example.test".parse().unwrap(),
        role: AclRole::Reader,
    };
    let (state, changed) = catalog
        .change_acl(
            target.clone(),
            upsert("user-reader@example.test"),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(changed, Some(reader.clone()));
    assert!(state.entries.contains(&reader) && state.entries.contains(&owner_grant));
    let delete = AclChange::Delete {
        entity: "user-reader@example.test".into(),
    };
    let (state, changed) = catalog
        .change_acl(target, delete, PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(changed, None);
    assert!(!state.entries.contains(&reader));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn replay_snapshot_rechecks_current_delegate_grants_and_binds_immutable_owner_to_incarnation()
{
    for backend in ["disk", "memory"] {
        let data = TestDir::new();
        let (catalog, auth, record) = setup(&data, backend).await;
        let uploader = auth.principals().unwrap()[0].clone();
        let delegate = auth.principals().unwrap()[1].clone();
        let access = catalog
            .bucket_access("bucket", Some(&delegate), crate::auth::OBJECTS_GET_IAM)
            .await
            .unwrap();
        let target = AclTarget::Object {
            bucket: "bucket".into(),
            name: "object".into(),
            generation: Some(record.generation.get()),
        };
        catalog
            .change_acl(
                target.clone(),
                AclChange::Upsert {
                    entity: "user-delegate@example.test".parse().unwrap(),
                    role: Some(crate::acl::AclRole::Owner),
                },
                PERMISSIVE,
            )
            .await
            .unwrap();
        let projection = catalog
            .acl_projection("bucket", vec![("object".into(), record.generation.get())])
            .await
            .unwrap()
            .unwrap();
        let incarnation = catalog
            .run(|connection| {
                connection
                    .prepare_cached(
                        "SELECT incarnation FROM bucket_incarnations WHERE bucket='bucket'",
                    )
                    .unwrap()
                    .query_row([], |row| row.get::<_, Incarnation>(0))
                    .map_err(unavailable)
            })
            .await
            .unwrap();
        let snapshot = ObjectAclSnapshot::new(
            &record,
            projection.objects[0].entries().to_vec(),
            incarnation,
        );
        assert!(
            !snapshot
                .entries
                .iter()
                .any(|entry| entry.entity.as_str() == "user-uploader@example.test")
        );
        assert!(snapshot.matches_resource(
            crate::object::ObjectKey::from_parts_unchecked("bucket", "object"),
            record.generation.get()
        ));
        assert!(!snapshot.matches_resource(
            crate::object::ObjectKey::from_parts_unchecked("other", "object"),
            record.generation.get()
        ));
        assert!(!snapshot.matches_resource(
            crate::object::ObjectKey::from_parts_unchecked("bucket", "other"),
            record.generation.get()
        ));
        assert!(!snapshot.matches_resource(
            crate::object::ObjectKey::from_parts_unchecked("bucket", "object"),
            record.generation.get() + 1
        ));
        assert!(
            catalog
                .object_acl_snapshot(snapshot.clone(), &access)
                .await
                .unwrap()
                .is_some()
        );
        catalog
            .change_acl(
                target,
                AclChange::Delete {
                    entity: "user-delegate@example.test".into(),
                },
                PERMISSIVE,
            )
            .await
            .unwrap();
        assert!(
            catalog
                .object_acl_snapshot(snapshot.clone(), &access)
                .await
                .unwrap()
                .is_none()
        );
        let get = catalog
            .bucket_access("bucket", Some(&uploader), crate::auth::OBJECTS_GET)
            .await
            .unwrap();
        assert!(
            catalog
                .object_acl_snapshot(snapshot.clone(), &get)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            catalog
                .object_acl_projection(
                    "bucket",
                    vec![("object".into(), record.generation.get())],
                    &get
                )
                .await,
            Err(CatalogError::Forbidden)
        );
        let owner_access = catalog
            .bucket_access("bucket", Some(&uploader), crate::auth::OBJECTS_GET_IAM)
            .await
            .unwrap();
        assert!(
            !catalog
                .has_bucket_permission("bucket", &owner_access)
                .await
                .unwrap()
        );
        assert!(
            catalog
                .object_acl_snapshot(snapshot.clone(), &owner_access)
                .await
                .unwrap()
                .is_some()
        );
        catalog
            .delete_object(live("bucket", "object"), Default::default(), PERMISSIVE)
            .await
            .unwrap();
        assert!(
            catalog
                .object_acl_snapshot(snapshot.clone(), &access)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            catalog
                .object_acl_snapshot(snapshot.clone(), &owner_access)
                .await
                .unwrap()
                .is_some()
        );
        let mut wrong = snapshot.clone();
        wrong.incarnation = "ffffffffffffffffffffffffffffffff".parse().unwrap();
        assert!(
            catalog
                .object_acl_snapshot(wrong, &owner_access)
                .await
                .unwrap()
                .is_none()
        );
        catalog
            .delete_bucket("bucket", Default::default(), PERMISSIVE)
            .await
            .unwrap();
        catalog
            .insert_bucket(
                crate::json::parse_insert(
                    br#"{"name":"bucket"}"#,
                    "different-project",
                    std::time::UNIX_EPOCH,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let recreated = BucketAccess::Policy {
            identity: Some(uploader),
            permission: crate::auth::Permission::ObjectsGetIam,
            project: Some("different-project".into()),
        };
        assert!(
            catalog
                .object_acl_snapshot(snapshot, &recreated)
                .await
                .unwrap()
                .is_none()
        );
        catalog.close().await.unwrap();
    }
}
