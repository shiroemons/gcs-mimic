use super::*;
use crate::storage::test_fixtures::{PERMISSIVE, live};
use crate::{
    object::{ObjectMetadataUpdate, ObjectRecord},
    storage::{Catalog, admin::Origin},
    test_support::TestDir,
};
use std::{fs, io::Read, path::Path, time::SystemTime};

const BODY: &[u8] = b"\0\xff immutable API bytes";

#[tokio::test]
async fn descriptor_exhaustion_does_not_persist_workspace_recovery() {
    let fixture = Fixture::new().await;
    let identity = fixture
        .root
        .file_job(|root| fsutil::stat_identity(root).map_err(AdminError::backend))
        .await
        .unwrap();
    for reopen in [false, true] {
        WORKSPACE_FAILURES
            .lock()
            .unwrap()
            .push((identity.device, identity.inode, libc::EMFILE));
        let error = if reopen {
            fixture.root.writeback(limits()).await.err().unwrap()
        } else {
            fixture.writeback.status().await.unwrap_err()
        };
        assert!(crate::error::causes(&error).any(|cause| {
            cause
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.raw_os_error() == Some(libc::EMFILE))
        }));
        let status = fixture.writeback.status().await.unwrap();
        assert!(!status.recovery);
        assert!(!status.io_failure);
    }
    fixture.shut_down().await;
}

#[tokio::test]
async fn workspace_access_failures_keep_their_cause_without_persisting_recovery() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new().await;
    let workspace = fixture.tree.0.join(mapping::WORKSPACE);
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o000)).unwrap();
    let status = fixture.writeback.status().await;
    let reopen = fixture.root.writeback(limits()).await;
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        matches!(status, Err(Error::Admin(AdminError::Io(ref cause))) if cause.kind() == io::ErrorKind::PermissionDenied)
    );
    assert!(
        matches!(reopen, Err(Error::Admin(AdminError::Io(ref cause))) if cause.kind() == io::ErrorKind::PermissionDenied)
    );
    let status = fixture.writeback.status().await.unwrap();
    assert!(!status.io_failure);
    assert!(!status.recovery);
    fixture.shut_down().await;
}

struct Fixture {
    _data: TestDir,
    tree: TestDir,
    catalog: Catalog,
    root: RootLease,
    writeback: Writeback,
    record: ObjectRecord,
}
impl Fixture {
    async fn shut_down(self) -> (TestDir, TestDir, ObjectRecord) {
        drop(self.writeback);
        drop(self.root);
        self.catalog.close().await.unwrap();
        (self._data, self.tree, self.record)
    }

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
        assert!(
            crate::sync::reconcile(&catalog, &root)
                .await
                .unwrap()
                .baseline_complete
        );
        catalog
            .update_object_metadata(
                live("test-bucket", "object"),
                Default::default(),
                ObjectMetadataUpdate {
                    temporary_hold: Some(true),
                    ..Default::default()
                },
                |_| Ok(()),
                PERMISSIVE,
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
        fixture.shut_down().await;
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
    fixture.shut_down().await;
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
        for &crash in fault::Crash::ALL {
            let fixture = Fixture::new().await;
            let snapshots = TestDir::new();
            let data_snapshot = snapshots.0.join("catalog");
            let tree_snapshot = snapshots.0.join("tree");
            fault::arm(&fixture.tree.0, step, 0, crash.fault(tree_snapshot.clone()));
            fixture
                .catalog
                .inject(step, 0, fault::Fault::Snapshot(data_snapshot.clone()));
            drain(&fixture.writeback, fixture.begin().await).await;
            assert_eq!(fixture.catalog.pending_faults(), 0, "{step:?} {crash:?}");
            assert_eq!(fault::pending(&fixture.tree.0), 0, "{step:?} {crash:?}");
            unchanged_api(&fixture.catalog, &fixture.record).await;
            let (_data, tree, record) = fixture.shut_down().await;
            restore_tree(&tree_snapshot, &tree.0);
            let recovered = Catalog::open(&data_snapshot).await.unwrap();
            unchanged_api(&recovered, &record).await;
            let root = recovered
                .sync_state()
                .activate_root(&tree.0, super::super::super::super::root::Activation::Open)
                .await
                .unwrap();
            let writeback = root.writeback(limits()).await.unwrap();
            if step == Step::SyncCopyCommit {
                assert_eq!(
                    writeback.status().await.unwrap().stop,
                    Stop::Ready,
                    "{step:?} {crash:?}"
                );
            } else {
                assert_ne!(
                    writeback.status().await.unwrap().stop,
                    Stop::Ready,
                    "{step:?} {crash:?}"
                );
            }
            if matches!(step, Step::SyncCopyInstalled | Step::SyncCopyCommit) {
                assert_eq!(
                    fs::read(tree.0.join("test-bucket/object")).unwrap(),
                    BODY,
                    "{step:?} {crash:?}"
                );
            } else {
                assert!(
                    !tree.0.join("test-bucket/object").exists(),
                    "{step:?} {crash:?}"
                );
            }
            drop(writeback);
            drop(root);
            recovered.close().await.unwrap();
        }
    }
}
