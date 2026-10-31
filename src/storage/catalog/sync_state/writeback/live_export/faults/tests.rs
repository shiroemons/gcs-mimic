use super::*;
use crate::{
    config::Config,
    object::{ObjectMetadataUpdate, ObjectRecord},
    storage::{Catalog, admin::Origin},
    test_support::TestDir,
};
use std::{fs, io::Read, path::Path, time::SystemTime};

const BODY: &[u8] = b"\0\xff immutable API bytes";

struct Fixture {
    _data: TestDir,
    tree: TestDir,
    catalog: Catalog,
    root: RootLease,
    writeback: Writeback,
    record: ObjectRecord,
}
impl Fixture {
    async fn new() -> Self {
        let data = TestDir::new();
        let tree = TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        catalog
            .insert_bucket(
                crate::json::parse_insert(
                    br#"{"name":"test-bucket"}"#,
                    "project",
                    SystemTime::now(),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        fs::write(data.0.join("source"), BODY).unwrap();
        let bucket = BucketName::new("test-bucket").unwrap();
        let name = ObjectName::new("object").unwrap();
        catalog
            .admin(Origin::Api)
            .publish_from_file(
                &bucket,
                &name,
                crate::storage::admin::Publication {
                    content_type: "application/octet-stream".into(),
                    metadata: Default::default(),
                    expected: Default::default(),
                    condition: None,
                    max_bytes: 4096,
                    mode: crate::storage::admin::PublishMode::Replace,
                },
                File::open(data.0.join("source")).unwrap(),
            )
            .await
            .unwrap();
        let root = catalog
            .sync_state()
            .activate_root(&tree.0, super::super::super::super::root::Activation::Open)
            .await
            .unwrap();
        let config = Config {
            data_dir: data.0.clone(),
            sync_dir: Some(tree.0.clone()),
            ..Default::default()
        };
        assert!(
            crate::sync::reconcile(&catalog, &root, &config)
                .await
                .unwrap()
                .baseline_complete
        );
        catalog
            .update_object_metadata(
                "test-bucket",
                "object",
                None,
                Default::default(),
                ObjectMetadataUpdate {
                    temporary_hold: Some(true),
                    ..Default::default()
                },
                |_| Ok(()),
            )
            .await
            .unwrap();
        let record = catalog
            .admin(Origin::Api)
            .get_object(&bucket, &name, None)
            .await
            .unwrap();
        fs::remove_file(tree.0.join("test-bucket/object")).unwrap();
        let writeback = root.writeback(limits()).await.unwrap();
        Self {
            _data: data,
            tree,
            catalog,
            root,
            writeback,
            record,
        }
    }

    async fn begin(&self) -> LiveExport {
        let bucket = BucketName::new("test-bucket").unwrap();
        let name = ObjectName::new("object").unwrap();
        let binding = self.catalog.sync_state().binding(&bucket).await.unwrap();
        let tracked = self
            .catalog
            .sync_state()
            .get_path(binding.clone(), "test-bucket/object".into())
            .await
            .unwrap();
        let generation = Generation::new(self.record.generation).unwrap();
        let (_, reader) = self
            .catalog
            .admin(Origin::Api)
            .open_object(&bucket, &name, Some(generation))
            .await
            .unwrap();
        let source = reader.into_file().await.unwrap();
        let fence = self
            .catalog
            .read_changes(None, None, 1)
            .await
            .unwrap()
            .fence;
        self.writeback
            .begin_live_export(
                binding,
                name,
                generation,
                tracked,
                fence,
                LiveSource::ProtectedRestore(source),
            )
            .await
            .unwrap()
    }
}

fn limits() -> Limits {
    Limits::new(
        crate::sync::CONFLICT_MAX_BYTES,
        crate::sync::CONFLICT_MAX_ENTRIES,
    )
    .unwrap()
}

async fn drain(writeback: &Writeback, operation: LiveExport) {
    loop {
        match writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap()
        {
            LiveResult::Pending => {}
            result => {
                assert_eq!(result, LiveResult::Complete);
                return;
            }
        }
    }
}

async fn unchanged_api(catalog: &Catalog, record: &ObjectRecord) {
    let bucket = BucketName::new("test-bucket").unwrap();
    let name = ObjectName::new("object").unwrap();
    assert_eq!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket, &name, None)
            .await
            .unwrap(),
        *record
    );
    let (_, reader) = catalog
        .admin(Origin::Api)
        .open_object(&bucket, &name, None)
        .await
        .unwrap();
    let mut bytes = Vec::new();
    reader
        .into_file()
        .await
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, BODY);
}

#[tokio::test]
async fn protected_read_copy_rejects_a_new_external_file_and_failed_writes_or_syncs_do_not_acknowledge()
 {
    for step in [Step::SyncCopyWrite, Step::SyncCopySync] {
        let fixture = Fixture::new().await;
        let operation = fixture.begin().await;
        fixture
            .catalog
            .inject(step, 0, fault::Fault::Io(io::ErrorKind::WriteZero));
        assert!(
            fixture
                .writeback
                .advance_live_export(operation.clone())
                .await
                .is_err()
        );
        assert_eq!(fixture.catalog.pending_faults(), 0);
        assert!(!fixture.tree.0.join("test-bucket/object").exists());
        assert_eq!(fixture.writeback.status().await.unwrap().entries, 1);
        unchanged_api(&fixture.catalog, &fixture.record).await;
        drain(&fixture.writeback, operation).await;
        assert_eq!(
            fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
            BODY
        );
        drop(fixture.writeback);
        drop(fixture.root);
        fixture.catalog.close().await.unwrap();
    }
    let fixture = Fixture::new().await;
    let operation = fixture.begin().await;
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap(),
        LiveResult::Pending
    );
    fs::write(
        fixture.tree.0.join("test-bucket/object"),
        b"new external file",
    )
    .unwrap();
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation)
            .await
            .unwrap(),
        LiveResult::Conflict
    );
    assert_eq!(
        fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
        b"new external file"
    );
    unchanged_api(&fixture.catalog, &fixture.record).await;
    drop(fixture.writeback);
    drop(fixture.root);
    fixture.catalog.close().await.unwrap();
}

fn restore_tree(snapshot: &Path, tree: &Path) {
    for relative in [mapping::MARKER, "test-bucket/object"] {
        let target = tree.join(relative);
        if snapshot.join(relative).exists() {
            fs::copy(snapshot.join(relative), target).unwrap();
        } else if target.exists() {
            fs::remove_file(target).unwrap();
        }
    }
    let workspace = tree.join(mapping::WORKSPACE);
    for entry in fs::read_dir(&workspace).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    for entry in fs::read_dir(snapshot.join(mapping::WORKSPACE)).unwrap() {
        let entry = entry.unwrap();
        fs::copy(entry.path(), workspace.join(entry.file_name())).unwrap();
    }
}

#[tokio::test]
async fn protected_read_copy_crash_and_power_loss_preserve_api_bytes_and_stop_unfinished_recovery()
{
    for step in [
        Step::SyncCopySync,
        Step::SyncCopyDurable,
        Step::SyncCopyInstalled,
        Step::SyncCopyCommit,
    ] {
        for power_loss in [false, true] {
            let fixture = Fixture::new().await;
            let snapshots = TestDir::new();
            let data_snapshot = snapshots.0.join("catalog");
            let tree_snapshot = snapshots.0.join("tree");
            fault::arm(
                &fixture.tree.0,
                step,
                0,
                if power_loss {
                    fault::Fault::PowerLoss(tree_snapshot.clone())
                } else {
                    fault::Fault::Snapshot(tree_snapshot.clone())
                },
            );
            fixture
                .catalog
                .inject(step, 0, fault::Fault::Snapshot(data_snapshot.clone()));
            drain(&fixture.writeback, fixture.begin().await).await;
            assert_eq!(fixture.catalog.pending_faults(), 0);
            assert_eq!(fault::pending(&fixture.tree.0), 0);
            unchanged_api(&fixture.catalog, &fixture.record).await;
            drop(fixture.writeback);
            drop(fixture.root);
            fixture.catalog.close().await.unwrap();
            restore_tree(&tree_snapshot, &fixture.tree.0);
            let recovered = Catalog::open(&data_snapshot).await.unwrap();
            unchanged_api(&recovered, &fixture.record).await;
            let root = recovered
                .sync_state()
                .activate_root(
                    &fixture.tree.0,
                    super::super::super::super::root::Activation::Open,
                )
                .await
                .unwrap();
            let writeback = root.writeback(limits()).await.unwrap();
            if step == Step::SyncCopyCommit {
                assert_eq!(writeback.status().await.unwrap().stop, Stop::Ready);
            } else {
                assert_ne!(writeback.status().await.unwrap().stop, Stop::Ready);
            }
            if matches!(step, Step::SyncCopyInstalled | Step::SyncCopyCommit) {
                assert_eq!(
                    fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
                    BODY
                );
            } else {
                assert!(!fixture.tree.0.join("test-bucket/object").exists());
            }
            drop(writeback);
            drop(root);
            recovered.close().await.unwrap();
        }
    }
}
