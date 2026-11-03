use super::*;
use crate::storage::test_fixtures::{PERMISSIVE, live};
use crate::{
    clock::SystemClock,
    object::{ExpectedHashes, ObjectRecord, UploadChunk},
    storage::{
        Catalog,
        admin::{BucketDefinition, BucketName, LocalAdmin, ObjectName, Publication, PublishMode},
    },
    test_support::TestDir,
};
use bytes::Bytes;
use std::{io, sync::Arc};
use tokio::sync::mpsc;

async fn setup() -> (TestDir, Catalog, Admin) {
    let dir = TestDir::new();
    let catalog = Catalog::open_for_server(&dir.0, Arc::new(SystemClock), LocalAdmin::Enabled)
        .await
        .unwrap();
    let admin = catalog.local_admin().unwrap();
    bucket(&admin).await;
    (dir, catalog, admin)
}

#[tokio::test]
async fn reset_classifies_every_catalog_table() {
    let (_dir, catalog, _admin) = setup().await;
    catalog.run::<_, CatalogError>(|connection| {
        let tables = connection.prepare_cached("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let cleared = CLEAR.iter().filter_map(|statement| statement.strip_prefix("DELETE FROM ")).collect::<Vec<_>>();
        for table in &tables {
            let classifications = [cleared.contains(&table.as_str()), RETAINED.contains(&table.as_str()), CASCADED.contains(&table.as_str())];
            assert_eq!(classifications.into_iter().filter(|classified| *classified).count(), 1, "ambiguous or missing reset policy: {table}");
        }
        for table in cleared.iter().chain(RETAINED).chain(CASCADED) {
            assert!(tables.iter().any(|existing| existing == table), "obsolete reset policy: {table}");
        }
        Ok(())
    }).await.unwrap();
    catalog.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn reclaim_retains_the_whole_page_until_every_directory_is_synced() {
    use crate::storage::{
        BlobId,
        catalog::fault::{Fault, Step},
    };
    let (dir, catalog, _admin) = setup().await;
    let ids = [
        "01000000000000000000000000000000",
        "01000000000000000000000000000001",
        "01000000000000000000000000000002",
    ]
    .map(|id| BlobId::parse(id).unwrap());
    for id in ids {
        for path in [
            id.blob_path(&dir.0),
            id.staging_path(&dir.0),
            dir.0.join("appendable").join(id.to_hex()),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"unreferenced").unwrap();
        }
    }
    let root = dir.0.clone();
    catalog
        .run::<_, CatalogError>(move |connection| {
            for id in ids {
                sql::exec(
                    connection,
                    "INSERT INTO pending_blob_deletions (blob_id) VALUES (?1)",
                    [id],
                )?;
            }
            super::super::super::fault::arm(
                &root,
                Step::ResetReclaimSync,
                1,
                Fault::Io(io::ErrorKind::Other),
            );
            assert!(reclaim(connection, &root).is_err());
            assert_eq!(
                sql::scalar::<i64>(
                    connection,
                    "SELECT count(*) FROM pending_blob_deletions",
                    []
                )?,
                3
            );
            for id in ids {
                assert!(!id.blob_path(&root).exists());
                assert!(!id.staging_path(&root).exists());
                assert!(!root.join("appendable").join(id.to_hex()).exists());
            }
            reclaim(connection, &root)?;
            assert_eq!(
                sql::scalar::<i64>(
                    connection,
                    "SELECT count(*) FROM pending_blob_deletions",
                    []
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

async fn bucket(admin: &Admin) {
    admin
        .ensure_bucket(
            BucketDefinition::default_for(
                &BucketName::new("reset-bucket").unwrap(),
                "project",
                &SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
}

async fn publish(admin: &Admin) -> ObjectRecord {
    let (sender, receiver) = mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(Bytes::from_static(b"original"))))
        .unwrap();
    sender.try_send(Ok(UploadChunk::End)).unwrap();
    admin
        .publish_object(
            &BucketName::new("reset-bucket").unwrap(),
            &ObjectName::new("value").unwrap(),
            Publication {
                content_type: "text/plain".into(),
                metadata: Default::default(),
                expected: ExpectedHashes::default(),
                condition: None,
                max_bytes: 1024,
                mode: PublishMode::Replace,
            },
            receiver,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn reset_preserves_high_water_identity_and_open_readers_and_survives_restart() {
    let (dir, catalog, admin) = setup().await;
    let old = publish(&admin).await;
    let identity = catalog.sync_state().catalog_id().await.unwrap();
    let (_, mut reader) = admin
        .open_object(
            &BucketName::new("reset-bucket").unwrap(),
            &ObjectName::new("value").unwrap(),
            None,
        )
        .await
        .unwrap();
    let outcome = admin.reset().await.unwrap();
    assert_eq!((outcome.buckets, outcome.objects), (1, 1));
    assert!(!outcome.cleanup_pending);
    assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"original"[..]);
    drop(reader);
    assert_eq!(catalog.sync_state().catalog_id().await.unwrap(), identity);
    bucket(&admin).await;
    assert!(publish(&admin).await.generation > old.generation);
    admin.reset().await.unwrap();
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert!(
        reopened
            .get_bucket("reset-bucket", PERMISSIVE)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(reopened.sync_state().catalog_id().await.unwrap(), identity);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn reset_busy_keeps_existing_state_and_closes_new_activity_under_exclusive_ownership() {
    let (_dir, catalog, admin) = setup().await;
    let old = publish(&admin).await;
    let upload = catalog.admit_object_upload().unwrap();
    assert!(matches!(admin.reset().await, Err(AdminError::Busy { .. })));
    drop(upload);
    let object = live("reset-bucket", "value");
    let source = catalog
        .resolve_derived_source(object, Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert!(matches!(admin.reset().await, Err(AdminError::Busy { .. })));
    drop(source);
    let exclusive = catalog.0.activity.clone().try_write_owned().unwrap();
    assert!(catalog.admit_object_upload().is_err());
    assert!(catalog.try_resumable_writer("session").is_err());
    assert!(
        admin
            .get_bucket(&BucketName::new("reset-bucket").unwrap())
            .await
            .is_err()
    );
    drop(exclusive);
    assert_eq!(
        admin
            .get_object(
                &BucketName::new("reset-bucket").unwrap(),
                &ObjectName::new("value").unwrap(),
                None
            )
            .await
            .unwrap(),
        old
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn reset_failed_commit_preserves_state_and_deferred_cleanup_recovers() {
    use crate::storage::catalog::fault::{Fault, Step};
    let (dir, catalog, admin) = setup().await;
    let old = publish(&admin).await;
    catalog.inject(Step::BeforeCommit, 0, Fault::Io(io::ErrorKind::StorageFull));
    assert!(admin.reset().await.is_err());
    assert_eq!(
        admin
            .get_object(
                &BucketName::new("reset-bucket").unwrap(),
                &ObjectName::new("value").unwrap(),
                None
            )
            .await
            .unwrap(),
        old
    );
    let cleanup_step = if cfg!(unix) {
        Step::ResetReclaimSync
    } else {
        Step::ResetReclaim
    };
    catalog.inject(cleanup_step, 0, Fault::Io(io::ErrorKind::Other));
    let result = admin.reset().await.unwrap();
    assert!(result.cleanup_pending);
    assert_eq!(catalog.pending_faults(), 0);
    catalog.close().await.unwrap();
    let reopened = Catalog::open(&dir.0).await.unwrap();
    assert!(
        reopened
            .get_bucket("reset-bucket", PERMISSIVE)
            .await
            .unwrap()
            .is_none()
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn reset_refuses_incomplete_import_and_resolves_post_commit_error() {
    use crate::storage::{
        catalog::fault::{Fault, Step},
        import_journal::{ImportBinding, ImportElement},
    };
    let dir = TestDir::new();
    let catalog = Catalog::open_for_server(&dir.0, Arc::new(SystemClock), LocalAdmin::Enabled)
        .await
        .unwrap();
    let admin = catalog.local_admin().unwrap();
    let binding = ImportBinding {
        source: [1; 32],
        fingerprint: [2; 32],
        expected_elements: 1,
    };
    catalog
        .import_journal()
        .begin(binding.clone())
        .await
        .unwrap();
    let name = BucketName::new("reset-bucket").unwrap();
    admin
        .publish_import_bucket(
            binding.clone(),
            0,
            ImportElement {
                ordinal: 1,
                fingerprint: [3; 32],
                inode: None,
                ctime_ns: None,
            },
            BucketDefinition::default_for(&name, "project", &SystemClock).unwrap(),
        )
        .await
        .unwrap();
    let previous = admin.get_bucket(&name).await.unwrap();
    assert!(matches!(admin.reset().await, Err(AdminError::Precondition)));
    assert_eq!(admin.get_bucket(&name).await.unwrap(), previous);
    assert!(catalog.import_journal().in_progress().await.unwrap());
    catalog.import_journal().finish(binding).await.unwrap();
    publish(&admin).await;
    catalog.inject(Step::AfterCommit, 0, Fault::Io(io::ErrorKind::Other));
    assert!(!admin.reset().await.unwrap().cleanup_pending);
    assert!(
        catalog
            .get_bucket("reset-bucket", PERMISSIVE)
            .await
            .unwrap()
            .is_none()
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn reset_waits_for_staged_files_session_and_appendable_writers_and_clears_retained_transfers()
{
    use crate::{bucket::BucketAccess, storage::ResumableOptions};
    let (dir, catalog, admin) = setup().await;
    publish(&admin).await;
    let input = TestDir::new();
    let path = input.0.join("fixture");
    std::fs::write(&path, b"fixture").unwrap();
    let staged = admin
        .stage_from_file(
            std::fs::File::open(&path).unwrap(),
            ExpectedHashes::default(),
            1024,
        )
        .await
        .unwrap();
    assert!(matches!(admin.reset().await, Err(AdminError::Busy { .. })));
    staged.discard().await.unwrap();
    let session = catalog
        .create_resumable_session(
            "reset-bucket",
            "pending",
            ResumableOptions::new(BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap();
    let lease = catalog
        .lock_resumable_session(&session.id, "reset-bucket")
        .await
        .unwrap();
    assert!(matches!(admin.reset().await, Err(AdminError::Busy { .. })));
    drop(lease);
    let mut writer = catalog
        .create_appendable(
            "reset-bucket",
            "append",
            ResumableOptions::new(BucketAccess::Permissive),
        )
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"append"), 1024)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    assert!(matches!(admin.reset().await, Err(AdminError::Busy { .. })));
    drop(writer);
    let object = live("reset-bucket", "value");
    let source = catalog
        .resolve_derived_source(object, Default::default(), PERMISSIVE)
        .await
        .unwrap();
    let transfer = catalog
        .begin_derived(vec![source.whole()], 1024)
        .await
        .unwrap();
    admin.reset().await.unwrap();
    assert!(catalog.get_derived(&transfer.id).await.is_err());
    catalog
        .run::<_, CatalogError>(|connection| {
            for table in CLEAR
                .iter()
                .filter_map(|statement| statement.strip_prefix("DELETE FROM "))
                .chain(CASCADED.iter().copied())
            {
                let count: i64 =
                    connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })?;
                assert_eq!(count, 0, "reset retained rows in {table}");
            }
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        catalog
            .resumable_session(&session.id, "reset-bucket")
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    assert_eq!(
        std::fs::read_dir(dir.0.join("appendable")).unwrap().count(),
        0
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"fixture");
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn reset_crash_and_power_loss_snapshots_recover_on_each_commit_and_cleanup_boundary() {
    use crate::storage::catalog::fault::{Crash, Step};
    let steps: &[Step] = if cfg!(unix) {
        &[
            Step::BeforeCommit,
            Step::AfterCommit,
            Step::ResetReclaim,
            Step::ResetReclaimSync,
        ]
    } else {
        &[Step::BeforeCommit, Step::AfterCommit, Step::ResetReclaim]
    };
    for &step in steps {
        for &crash in Crash::ALL {
            let (_dir, catalog, admin) = setup().await;
            publish(&admin).await;
            let snapshot = TestDir::new();
            catalog.inject(step, 0, crash.fault(snapshot.0.clone()));
            admin.reset().await.unwrap();
            assert_eq!(catalog.pending_faults(), 0);
            let recovered = Catalog::open(&snapshot.0).await.unwrap();
            assert_eq!(
                recovered
                    .get_bucket("reset-bucket", PERMISSIVE)
                    .await
                    .unwrap()
                    .is_some(),
                step == Step::BeforeCommit,
                "{step:?} {crash:?}"
            );
            recovered.close().await.unwrap();
            catalog.close().await.unwrap();
        }
    }
}
