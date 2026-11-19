use super::super::tests::drain;
use super::*;
use crate::storage::test_fixtures::{PERMISSIVE, live};
use crate::{
    object::{ObjectMetadataUpdate, ObjectRecord},
    storage::{Catalog, admin::Origin},
    sync::test_support::{operation_entries, restore_tree},
    test_support::TestDir,
};
use std::{fs, io::Read, path::Path, time::SystemTime};

const BODY: &[u8] = b"\0\xff immutable API bytes";

/// Exporting over a tracked file reserves its whole size, so a budget below the object size limit
/// would pause the root for good: no conflict exists to resolve and a rescan changes nothing.
#[tokio::test]
async fn a_tracked_body_past_one_gibibyte_reserves_without_pausing_the_root() {
    const TRACKED_SIZE: u64 = (1 << 30) + 1;
    let fixture = Fixture::new().await;
    fixture
        .writeback
        .db(|connection| {
            connection
                .prepare_cached("UPDATE sync_objects SET size=?1 WHERE name='object'")?
                .execute([TRACKED_SIZE])?;
            Ok(())
        })
        .await
        .unwrap();
    let operation = fixture.begin().await;
    let status = fixture.writeback.status().await.unwrap();
    assert_ne!(status.stop(), Stop::ConflictCapacity);
    assert_eq!(status.reserved_bytes, u128::from(TRACKED_SIZE));
    drain(&fixture.writeback, operation).await;
    let status = fixture.writeback.status().await.unwrap();
    assert_eq!(status.stop(), Stop::Ready);
    assert_eq!(status.reserved_bytes, 0);
    fixture.shut_down().await;
}

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
        let source = reader.into_file().unwrap();
        let fence = self.catalog.current_fence().await.unwrap();
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
    reader.into_file().unwrap().read_to_end(&mut bytes).unwrap();
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

/// The one operation entry of `workspace`.
fn only_entry(workspace: &Path) -> std::path::PathBuf {
    let entries = operation_entries(workspace);
    assert_eq!(entries.len(), 1, "{}", workspace.display());
    entries[0].clone()
}

/// A retry refills the private copy that a failed attempt left, in place: the workspace entry
/// keeps its inode through a failed sync and through a failed write after the refill emptied it,
/// and the export then completes with the API bytes.
#[tokio::test]
async fn a_retried_copy_refills_the_partial_private_copy_in_place() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new().await;
    let operation = fixture.begin().await;
    let copied = |path: &Path| {
        let metadata = fs::metadata(path).unwrap();
        (metadata.ino(), fs::read(path).unwrap())
    };
    fixture.catalog.inject(
        Step::SyncCopySync,
        0,
        fault::Fault::Io(io::ErrorKind::WriteZero),
    );
    assert!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .is_err()
    );
    let copy = only_entry(&fixture.tree.0.join(mapping::WORKSPACE));
    let (inode, bytes) = copied(&copy);
    assert_eq!(bytes, BODY);
    fixture.catalog.inject(
        Step::SyncCopyWrite,
        0,
        fault::Fault::Io(io::ErrorKind::WriteZero),
    );
    assert!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .is_err()
    );
    assert_eq!(copied(&copy), (inode, Vec::new()));
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap(),
        LiveResult::Pending
    );
    assert_eq!(copied(&copy), (inode, BODY.to_vec()));
    drain(&fixture.writeback, operation).await;
    assert_eq!(
        fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
        BODY
    );
    unchanged_api(&fixture.catalog, &fixture.record).await;
    fixture.shut_down().await;
}

/// A sync that fails after a retry refilled the private copy leaves that copy for the next retry
/// as the first failure did: it keeps its inode and the refilled bytes, nothing is installed or
/// protected, and the next retry completes the export.
#[tokio::test]
async fn a_failed_sync_after_a_refill_keeps_the_private_copy_for_the_next_retry() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new().await;
    let operation = fixture.begin().await;
    let mut copies = Vec::new();
    for _ in 0..2 {
        fixture.catalog.inject(
            Step::SyncCopySync,
            0,
            fault::Fault::Io(io::ErrorKind::WriteZero),
        );
        assert!(
            fixture
                .writeback
                .advance_live_export(operation.clone())
                .await
                .is_err()
        );
        assert_eq!(fixture.catalog.pending_faults(), 0);
        let copy = only_entry(&fixture.tree.0.join(mapping::WORKSPACE));
        copies.push((fs::metadata(&copy).unwrap().ino(), fs::read(&copy).unwrap()));
        let usage = fixture.writeback.status().await.unwrap();
        assert_eq!((usage.entries, usage.protected_entries), (1, 0));
        assert!(!fixture.tree.0.join("test-bucket/object").exists());
    }
    assert_eq!(copies[0], copies[1]);
    assert_eq!(copies[1].1, BODY);
    drain(&fixture.writeback, operation).await;
    assert_eq!(
        fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
        BODY
    );
    assert_eq!(
        fixture.writeback.status().await.unwrap().stop(),
        Stop::Ready
    );
    unchanged_api(&fixture.catalog, &fixture.record).await;
    fixture.shut_down().await;
}

/// A macOS clone keeps its source's mode, so the private copy of a read-only blob cannot be
/// opened for writing on a retry. That retry alone removes the copy and clones the blob again,
/// and the installed file keeps the blob's mode. Linux creates its copies with mode 0o600 and
/// refills this one in place like any other.
#[tokio::test]
async fn a_retried_copy_of_a_read_only_blob_completes_with_the_mode_a_first_copy_gets() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let fixture = Fixture::new().await;
    let (_, reader) = fixture
        .catalog
        .admin(Origin::Api)
        .open_object(
            &BucketName::new("test-bucket").unwrap(),
            &ObjectName::new("object").unwrap(),
            None,
        )
        .await
        .unwrap();
    reader
        .into_file()
        .unwrap()
        .set_permissions(fs::Permissions::from_mode(0o400))
        .unwrap();
    let operation = fixture.begin().await;
    fixture.catalog.inject(
        Step::SyncCopySync,
        0,
        fault::Fault::Io(io::ErrorKind::WriteZero),
    );
    assert!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .is_err()
    );
    let copy = only_entry(&fixture.tree.0.join(mapping::WORKSPACE));
    let failed = fs::metadata(&copy).unwrap();
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap(),
        LiveResult::Pending
    );
    let retried = fs::metadata(&copy).unwrap();
    let (mode, copied_again) = if cfg!(target_os = "macos") {
        (0o400, true)
    } else {
        (0o600, false)
    };
    assert_eq!(
        (
            failed.mode() & 0o777,
            retried.mode() & 0o777,
            retried.ino() != failed.ino()
        ),
        (mode, mode, copied_again)
    );
    drain(&fixture.writeback, operation).await;
    let target = fixture.tree.0.join("test-bucket/object");
    assert_eq!(fs::read(&target).unwrap(), BODY);
    assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, mode);
    assert_eq!(
        fixture.writeback.status().await.unwrap().stop(),
        Stop::Ready
    );
    unchanged_api(&fixture.catalog, &fixture.record).await;
    fixture.shut_down().await;
}

/// Emptying a private copy that another name also links would empty that name too, so a retry
/// that finds one stops as Protected instead, and both names keep the copied bytes.
#[tokio::test]
async fn a_retry_never_empties_a_private_copy_that_another_name_links() {
    let fixture = Fixture::new().await;
    let operation = fixture.begin().await;
    fixture.catalog.inject(
        Step::SyncCopySync,
        0,
        fault::Fault::Io(io::ErrorKind::WriteZero),
    );
    assert!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .is_err()
    );
    let copy = only_entry(&fixture.tree.0.join(mapping::WORKSPACE));
    let elsewhere = TestDir::new();
    let linked = elsewhere.0.join("linked");
    fs::hard_link(&copy, &linked).unwrap();
    let result = fixture
        .writeback
        .advance_live_export(operation.clone())
        .await;
    assert!(matches!(result, Err(Error::Protected)), "{result:?}");
    assert_eq!(fs::read(&linked).unwrap(), BODY);
    assert_eq!(fs::read(&copy).unwrap(), BODY);
    assert!(!fixture.tree.0.join("test-bucket/object").exists());
    unchanged_api(&fixture.catalog, &fixture.record).await;
    drop(operation);
    fixture.shut_down().await;
}

/// Refilled bytes are volatile until the refill syncs them: a crash or power loss while a retry
/// refills the private copy keeps no more than that, and restart recovery removes the unarmed copy
/// and resumes with the API unchanged and the target absent.
#[tokio::test]
async fn a_refilled_private_copy_keeps_only_synced_bytes_through_crash_and_power_loss() {
    for step in [
        Step::SyncCopyWrite,
        Step::SyncCopySync,
        Step::SyncCopyDurable,
    ] {
        for &crash in fault::Crash::ALL {
            let fixture = Fixture::new().await;
            let operation = fixture.begin().await;
            fixture.catalog.inject(
                Step::SyncCopySync,
                0,
                fault::Fault::Io(io::ErrorKind::WriteZero),
            );
            assert!(
                fixture
                    .writeback
                    .advance_live_export(operation.clone())
                    .await
                    .is_err()
            );
            let snapshots = TestDir::new();
            let data_snapshot = snapshots.0.join("catalog");
            let tree_snapshot = snapshots.0.join("tree");
            fault::arm(&fixture.tree.0, step, 0, crash.fault(tree_snapshot.clone()));
            fixture
                .catalog
                .inject(step, 0, fault::Fault::Snapshot(data_snapshot.clone()));
            drain(&fixture.writeback, operation).await;
            assert_eq!(fixture.catalog.pending_faults(), 0, "{step:?} {crash:?}");
            assert_eq!(fault::pending(&fixture.tree.0), 0, "{step:?} {crash:?}");
            let refilled = step != Step::SyncCopyWrite;
            let synced = step == Step::SyncCopyDurable;
            let kept: &[u8] = if synced || (refilled && crash == fault::Crash::Abort) {
                BODY
            } else {
                b""
            };
            assert_eq!(
                fs::read(only_entry(&tree_snapshot.join(mapping::WORKSPACE))).unwrap(),
                kept,
                "{step:?} {crash:?}"
            );
            let (_data, tree, record) = fixture.shut_down().await;
            restore_tree(&tree_snapshot, &tree.0, &RESTORED);
            let recovered = Catalog::open(&data_snapshot).await.unwrap();
            let root = recovered
                .sync_state()
                .activate_root(&tree.0, super::super::super::super::root::Activation::Open)
                .await
                .unwrap()
                .assume_naming(crate::sync::naming::RootNaming::host());
            let writeback = root.writeback(limits()).await.unwrap();
            assert_eq!(
                writeback.recover_pending().await.unwrap().stop(),
                Stop::Ready,
                "{step:?} {crash:?}"
            );
            assert!(
                operation_entries(&tree.0.join(mapping::WORKSPACE)).is_empty(),
                "{step:?} {crash:?}"
            );
            assert!(
                !tree.0.join("test-bucket/object").exists(),
                "{step:?} {crash:?}"
            );
            unchanged_api(&recovered, &record).await;
            drop(writeback);
            drop(root);
            recovered.close().await.unwrap();
        }
    }
}

/// The tree paths a snapshot restore puts back besides the workspace.
const RESTORED: [&str; 2] = [mapping::MARKER, "test-bucket/object"];

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
            restore_tree(&tree_snapshot, &tree.0, &RESTORED);
            let recovered = Catalog::open(&data_snapshot).await.unwrap();
            unchanged_api(&recovered, &record).await;
            let root = recovered
                .sync_state()
                .activate_root(&tree.0, super::super::super::super::root::Activation::Open)
                .await
                .unwrap()
                .assume_naming(crate::sync::naming::RootNaming::host());
            let writeback = root.writeback(limits()).await.unwrap();
            if step == Step::SyncCopyCommit {
                assert_eq!(
                    writeback.status().await.unwrap().stop(),
                    Stop::Ready,
                    "{step:?} {crash:?}"
                );
            } else {
                assert_ne!(
                    writeback.status().await.unwrap().stop(),
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
