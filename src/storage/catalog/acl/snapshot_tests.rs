use super::*;
use crate::{
    auth::{Auth, Registry},
    clock::SystemClock,
    object::{ExpectedHashes, ObjectMetadataUpdate, UploadChunk},
    storage::{ObjectUpload, admin::LocalAdmin},
    test_support::TestDir,
};
use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::mpsc;

async fn setup(data: &TestDir, backend: &str) -> (Catalog, Auth, crate::object::ObjectRecord) {
    let auth = Auth::strict(Registry::parse(br#"{"identities":[{"id":"uploader@example.test","tokens":[],"project_roles":{"local":["roles/storage.objectCreator"]}},{"id":"delegate@example.test","tokens":[]}]}"#).unwrap());
    let catalog = if backend == "disk" {
        Catalog::open_for_server(&data.0, Arc::new(SystemClock), LocalAdmin::Disabled)
            .await
            .unwrap()
    } else {
        Catalog::open_memory_for_server(
            1024 * 1024,
            1024 * 1024,
            Arc::new(SystemClock),
            LocalAdmin::Disabled,
        )
        .await
        .unwrap()
    };
    catalog.configure_authentication(&auth).await.unwrap();
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
    let (sender, body) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(Bytes::from_static(b"snapshot bytes"))))
        .unwrap();
    sender.try_send(Ok(UploadChunk::End)).unwrap();
    let record = catalog
        .put_object_prechecked(
            ObjectUpload {
                bucket: "bucket",
                name: "object",
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
            generation: Some(record.generation),
        };
        catalog
            .change_acl(
                target.clone(),
                AclChange::Upsert {
                    entity: "user-delegate@example.test".into(),
                    role: Some("OWNER".into()),
                },
                &BucketAccess::Permissive,
            )
            .await
            .unwrap();
        let projection = catalog
            .acl_projection("bucket", vec![("object".into(), record.generation)])
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
                    .query_row([], |row| row.get::<_, String>(0))
                    .map_err(unavailable)
            })
            .await
            .unwrap();
        let snapshot = ObjectAclSnapshot::new(&record, projection.objects[0].clone(), &incarnation);
        assert!(
            !snapshot
                .entries
                .iter()
                .any(|entry| entry.entity == "user-uploader@example.test")
        );
        assert!(snapshot.matches_resource("bucket", Some("object"), Some(record.generation)));
        assert!(!snapshot.matches_resource("other", Some("object"), Some(record.generation)));
        assert!(!snapshot.matches_resource("bucket", Some("other"), Some(record.generation)));
        assert!(!snapshot.matches_resource("bucket", Some("object"), Some(record.generation + 1)));
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
                &BucketAccess::Permissive,
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
                .object_acl_projection("bucket", vec![("object".into(), record.generation)], &get)
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
            .delete_object("bucket", "object", None, Default::default())
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
        wrong.incarnation = "wrong-incarnation".into();
        assert!(
            catalog
                .object_acl_snapshot(wrong, &owner_access)
                .await
                .unwrap()
                .is_none()
        );
        catalog.delete_bucket("bucket", None, None).await.unwrap();
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
