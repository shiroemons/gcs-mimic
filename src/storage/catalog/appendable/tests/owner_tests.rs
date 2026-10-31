use super::*;
use crate::{
    auth::{Auth, Registry},
    config::Backend,
    object::owner::ObjectOwner,
};
use serde_json::json;

struct Clock;
impl crate::clock::Clock for Clock {
    fn now(&self) -> SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(42)
    }
}

fn auth(creator_alias: &str, finalizer_alias: &str, finalizer_roles: &[&str]) -> Auth {
    let finalizer_roles = if finalizer_roles.is_empty() {
        json!({})
    } else {
        json!({"project":finalizer_roles})
    };
    Auth::strict(Registry::parse(json!({"identities":[
        {"id":"local:creator","tokens":[],"acl_entity":creator_alias,"project_roles":{"project":["roles/storage.objectCreator"]}},
        {"id":"local:finalizer","tokens":[],"acl_entity":finalizer_alias,"project_roles":finalizer_roles},
        {"id":"local:other","tokens":[]}
    ]}).to_string().as_bytes()).unwrap())
}

async fn open_catalog(data: &TestDir, backend: Backend, auth: &Auth) -> Catalog {
    let catalog = match backend {
        Backend::Disk => Catalog::open_for_server(
            &data.0,
            Arc::new(Clock),
            super::super::super::admin::LocalAdmin::Disabled,
        )
        .await
        .unwrap(),
        Backend::Memory => Catalog::open_memory_for_server(
            8192,
            1024 * 1024,
            Arc::new(Clock),
            super::super::super::admin::LocalAdmin::Disabled,
        )
        .await
        .unwrap(),
    };
    catalog.configure_authentication(auth).await.unwrap();
    catalog
}

#[tokio::test]
async fn appendable_finalization_preserves_creator_when_a_different_principal_takes_over() {
    for backend in [Backend::Disk, Backend::Memory] {
        let data = TestDir::new();
        let mut registry = auth(
            "user-creator@example.test",
            "user-finalizer@example.test",
            &["roles/storage.admin"],
        );
        let mut catalog = open_catalog(&data, backend, &registry).await;
        insert_bucket(&catalog).await;
        let creator = registry.principals().unwrap()[0].clone();
        let access = catalog
            .bucket_access("append", Some(&creator), crate::auth::OBJECTS_CREATE)
            .await
            .unwrap();
        let mut original = catalog
            .create_appendable("append", "owned", ResumableOptions::new(access))
            .await
            .unwrap();
        original
            .append(0, Bytes::from_static(b"data"), 1024)
            .await
            .unwrap();
        original.flush().await.unwrap();
        let generation = original.record().generation;
        let created = original.record().time_created;
        assert!(
            matches!(&original.record().owner, Some(ObjectOwner::Principal { id, .. }) if id == "local:creator")
        );
        drop(original);
        let expected_alias = if backend == Backend::Disk {
            catalog.close().await.unwrap();
            registry = auth(
                "user-current-creator@example.test",
                "user-creator@example.test",
                &["roles/storage.admin"],
            );
            catalog = open_catalog(&data, backend, &registry).await;
            "user-current-creator@example.test"
        } else {
            "user-creator@example.test"
        };
        let finisher = registry.principals().unwrap()[1].clone();
        let finisher_access = catalog
            .bucket_access("append", Some(&finisher), crate::auth::OBJECTS_CREATE)
            .await
            .unwrap();
        let mut writer = catalog
            .take_over_appendable(
                "append",
                "owned",
                generation,
                None,
                Default::default(),
                finisher_access.clone(),
            )
            .await
            .unwrap();
        let wrong = BucketAccess::Policy {
            identity: Some(registry.principals().unwrap()[2].clone()),
            permission: crate::auth::Permission::ObjectsCreate,
            project: Some("project".into()),
        };
        assert!(matches!(
            writer.finalize(None, wrong, 1024).await,
            Err(AppendError::Object(ObjectError::Forbidden))
        ));
        assert!(
            matches!(&writer.record().owner, Some(ObjectOwner::Principal { id, .. }) if id == "local:creator")
        );
        let record = writer
            .finalize(Some(crc32c::crc32c(b"data")), finisher_access, 1024)
            .await
            .unwrap();
        assert_eq!(record.generation, generation);
        assert_eq!(record.time_created, created);
        assert_eq!(
            record.owner,
            Some(ObjectOwner::Principal {
                id: "local:creator".into(),
                entity: Some(expected_alias.into())
            })
        );
        drop(writer);
        let creator = registry.principals().unwrap()[0].clone();
        let read = catalog
            .bucket_access("append", Some(&creator), crate::auth::OBJECTS_GET)
            .await
            .unwrap();
        assert_eq!(
            catalog
                .get_object_authorized(
                    "append",
                    "owned",
                    Some(generation),
                    Default::default(),
                    &read
                )
                .await
                .unwrap()
                .owner,
            record.owner
        );
        catalog.close().await.unwrap();
        if backend == Backend::Disk {
            let registry = auth(
                "user-current-creator@example.test",
                "user-creator@example.test",
                &[],
            );
            let catalog = open_catalog(&data, backend, &registry).await;
            let former_finalizer = catalog
                .bucket_access(
                    "append",
                    Some(&registry.principals().unwrap()[1]),
                    crate::auth::OBJECTS_GET,
                )
                .await
                .unwrap();
            assert_eq!(
                catalog
                    .get_object_authorized(
                        "append",
                        "owned",
                        None,
                        Default::default(),
                        &former_finalizer
                    )
                    .await,
                Err(ObjectError::Forbidden)
            );
            let creator = catalog
                .bucket_access(
                    "append",
                    Some(&registry.principals().unwrap()[0]),
                    crate::auth::OBJECTS_GET,
                )
                .await
                .unwrap();
            assert_eq!(
                catalog
                    .get_object_authorized("append", "owned", None, Default::default(), &creator)
                    .await
                    .unwrap()
                    .owner,
                record.owner
            );
            catalog.close().await.unwrap();
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn appendable_owner_survives_power_loss_on_both_sides_of_finalization_commit() {
    use crate::storage::fault::{Fault, Step};
    for (power_loss, steps) in [
        (false, vec![Step::BeforeCommit]),
        (false, vec![Step::AfterCommit]),
        (true, vec![Step::BeforeCommit]),
        (true, vec![Step::AfterCommit]),
        (true, vec![Step::BeforeCommit, Step::AfterCommit]),
    ] {
        let data = TestDir::new();
        let snapshots = TestDir::new();
        let registry = auth(
            "user-creator@example.test",
            "user-finalizer@example.test",
            &["roles/storage.admin"],
        );
        let catalog = open_catalog(&data, Backend::Disk, &registry).await;
        insert_bucket(&catalog).await;
        let access = catalog
            .bucket_access(
                "append",
                Some(&registry.principals().unwrap()[0]),
                crate::auth::OBJECTS_CREATE,
            )
            .await
            .unwrap();
        let mut writer = catalog
            .create_appendable("append", "power-loss", ResumableOptions::new(access))
            .await
            .unwrap();
        writer
            .append(0, Bytes::from_static(b"data"), 1024)
            .await
            .unwrap();
        writer.flush().await.unwrap();
        let generation = writer.record().generation;
        let snapshots: Vec<_> = steps
            .into_iter()
            .enumerate()
            .map(|(index, step)| {
                let snapshot = snapshots.0.join(format!("snapshot-{index}"));
                let fault = if power_loss {
                    Fault::PowerLoss(snapshot.clone())
                } else {
                    Fault::Snapshot(snapshot.clone())
                };
                catalog.inject(step, 0, fault);
                (snapshot, step)
            })
            .collect();
        let access = catalog
            .bucket_access(
                "append",
                Some(&registry.principals().unwrap()[1]),
                crate::auth::OBJECTS_CREATE,
            )
            .await
            .unwrap();
        let finalized = writer.finalize(None, access, 1024).await.unwrap();
        assert_eq!(catalog.pending_faults(), 0);
        assert!(
            matches!(&finalized.owner, Some(ObjectOwner::Principal { id, .. }) if id == "local:creator")
        );
        drop(writer);
        catalog.close().await.unwrap();
        for (snapshot, step) in snapshots {
            let recovered = Catalog::open(&snapshot).await.unwrap();
            if step == Step::BeforeCommit {
                let pending = open(&recovered, "power-loss").await.unwrap();
                assert!(
                    matches!(&pending.owner, Some(ObjectOwner::Principal { id, .. }) if id == "local:creator")
                );
                assert_eq!(pending.generation, generation);
                assert_eq!(persisted_content(&recovered, "power-loss").await, b"data");
            } else {
                let record = recovered
                    .get_object("append", "power-loss", None, Default::default())
                    .await
                    .unwrap();
                assert_eq!(record.owner, finalized.owner);
                assert_eq!(record.generation, generation);
                let (_, mut reader) = recovered
                    .open_object("append", "power-loss", None, Default::default())
                    .await
                    .unwrap();
                assert_eq!(
                    reader.next_chunk().await.unwrap().unwrap(),
                    Bytes::from_static(b"data")
                );
                assert!(reader.next_chunk().await.unwrap().is_none());
                drop(reader);
            }
            recovered.close().await.unwrap();
        }
    }
}
