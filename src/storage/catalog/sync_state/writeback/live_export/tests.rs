#![expect(
    clippy::unwrap_used,
    reason = "platform-gated fixtures fail at the operation under test"
)]

use super::*;
use crate::{
    clock::SystemClock,
    storage::{
        Catalog,
        admin::{BucketDefinition, Origin, Publication, PublishMode},
    },
    test_support::TestDir,
};
use std::fs;
mod migration;

pub(crate) async fn pending(operation: &LiveExport) -> Pending {
    operation.0.lock().await.item.clone()
}

pub(crate) async fn observation(
    writeback: &Writeback,
    bucket: &BucketName,
    name: &ObjectName,
    generation: Generation,
    fence: JournalFence,
) -> LiveExport {
    let binding = writeback
        .lease
        .catalog()
        .sync_state()
        .binding(bucket)
        .await
        .unwrap();
    let PathMapping::File(path) = mapping::name_to_path(bucket, name) else {
        panic!("fixture path must be representable")
    };
    let tracked = writeback
        .lease
        .catalog()
        .sync_state()
        .get_path(binding.clone(), path.to_str().unwrap().to_owned())
        .await
        .unwrap();
    writeback
        .begin_live_export(
            binding,
            name.clone(),
            generation,
            tracked,
            fence,
            LiveSource::Observe,
        )
        .await
        .unwrap()
}

pub(crate) async fn armed_observation(
    writeback: &Writeback,
    bucket: &BucketName,
    name: &ObjectName,
    generation: Generation,
    fence: JournalFence,
) -> Pending {
    let operation = observation(writeback, bucket, name, generation, fence).await;
    inject_failure(writeback.identities().1, "install", 1);
    assert!(matches!(
        writeback.advance_live_export(operation.clone()).await,
        Err(Error::Admin(AdminError::DiskFull(_)))
    ));
    let item = pending(&operation).await;
    assert_eq!(item.phase, Phase::Armed);
    item
}

struct Fixture {
    data: TestDir,
    tree: TestDir,
    catalog: Catalog,
    root: RootLease,
    writeback: Writeback,
    bucket: BucketName,
}
impl Fixture {
    async fn new() -> Self {
        let data = TestDir::new();
        let tree = TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let bucket = BucketName::new("test-bucket").unwrap();
        catalog
            .admin(Origin::Api)
            .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        catalog.enable_change_journal().await.unwrap();
        let root = catalog
            .sync_state()
            .activate_root(&tree.0, super::super::super::root::Activation::Open)
            .await
            .unwrap();
        let writeback = root.default_writeback().await.unwrap();
        Self {
            data,
            tree,
            catalog,
            root,
            writeback,
            bucket,
        }
    }
    async fn publish(&self, name: &str, bytes: &[u8]) -> crate::object::ObjectRecord {
        fs::write(self.data.0.join("input"), bytes).unwrap();
        self.catalog
            .admin(Origin::Api)
            .publish_from_file(
                &self.bucket,
                &ObjectName::new(name).unwrap(),
                Publication {
                    content_type: "application/octet-stream".into(),
                    metadata: Default::default(),
                    expected: Default::default(),
                    condition: None,
                    max_bytes: 4096,
                    mode: PublishMode::Replace,
                },
                File::open(self.data.0.join("input")).unwrap(),
            )
            .await
            .unwrap()
    }
    async fn begin(&self, name: &str) -> LiveExport {
        let name = ObjectName::new(name).unwrap();
        let binding = self
            .catalog
            .sync_state()
            .binding(&self.bucket)
            .await
            .unwrap();
        let (record, reader) = self
            .catalog
            .admin(Origin::Api)
            .open_object(&self.bucket, &name, None)
            .await
            .unwrap();
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
                Generation::new(record.generation).unwrap(),
                None,
                fence,
                LiveSource::ApiCopy(reader.into_file().await.unwrap()),
            )
            .await
            .unwrap()
    }
    async fn close(self) {
        drop(self.writeback);
        drop(self.root);
        self.catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn live_export_observed_external_change_cancels_the_unpublished_copy_and_releases_its_reservation()
 {
    let fixture = Fixture::new().await;
    fixture.publish("nested/object", b"api").await;
    let operation = fixture.begin("nested/object").await;
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap(),
        LiveResult::Pending
    );
    let item = pending(&operation).await;
    fs::create_dir_all(fixture.tree.0.join("test-bucket/nested")).unwrap();
    fs::write(fixture.tree.0.join(&item.path), b"external").unwrap();
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap(),
        LiveResult::Conflict
    );
    assert_eq!(
        fs::read(fixture.tree.0.join(&item.path)).unwrap(),
        b"external"
    );
    assert!(
        !fixture
            .tree
            .0
            .join(mapping::WORKSPACE)
            .join(&item.token.id)
            .exists()
    );
    let usage = fixture.writeback.status().await.unwrap();
    assert_eq!(
        (usage.entries, usage.reserved_bytes, usage.stop),
        (0, 0, Stop::Ready)
    );
    drop(operation);
    fixture.close().await;
}

#[tokio::test]
async fn durable_rehash_rejects_same_inode_edits_with_restored_mtime() {
    let fixture = Fixture::new().await;
    fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    for _ in 0..2 {
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap();
    }
    let path = fixture.tree.0.join("test-bucket/object");
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    let before = fsutil::stat_identity(&File::open(&path).unwrap()).unwrap();
    fs::write(&path, b"bad").unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let after = fsutil::stat_identity(&File::open(&path).unwrap()).unwrap();
    assert_eq!(
        (before.device, before.inode, before.size, before.modified_ns),
        (after.device, after.inode, after.size, after.modified_ns)
    );
    let result = fixture
        .writeback
        .advance_live_export(operation.clone())
        .await;
    let bytes = fs::read(&path).unwrap();
    drop(operation);
    fixture.close().await;
    assert!(
        result.is_err(),
        "durability accepted modified bytes: {result:?}"
    );
    assert_eq!(bytes, b"bad");
}

#[tokio::test]
async fn live_export_durable_then_ack_tracks_the_installed_fingerprint_and_finishes() {
    for body in [&b""[..], &b"\0\xff binary"[..]] {
        let fixture = Fixture::new().await;
        let record = fixture.publish("nested/object", body).await;
        let operation = fixture.begin("nested/object").await;
        for _ in 0..3 {
            assert_eq!(
                fixture
                    .writeback
                    .advance_live_export(operation.clone())
                    .await
                    .unwrap(),
                LiveResult::Pending
            );
        }
        let item = pending(&operation).await;
        assert_eq!(item.phase, Phase::Durable);
        let binding = fixture
            .catalog
            .sync_state()
            .binding(&fixture.bucket)
            .await
            .unwrap();
        assert!(
            fixture
                .catalog
                .sync_state()
                .get_path(binding.clone(), item.path.clone())
                .await
                .unwrap()
                .is_none()
        );
        inject_failure(fixture.root.status().root_id, "ack", 1);
        assert!(
            fixture
                .writeback
                .advance_live_export(operation.clone())
                .await
                .is_err()
        );
        assert!(
            fixture
                .catalog
                .sync_state()
                .get_path(binding.clone(), item.path.clone())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(fixture.writeback.status().await.unwrap().entries, 1);
        assert_eq!(
            fixture
                .writeback
                .advance_live_export(operation.clone())
                .await
                .unwrap(),
            LiveResult::Complete
        );
        let tracked = fixture
            .catalog
            .sync_state()
            .get_path(binding, item.path.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                tracked.generation.get(),
                tracked.fingerprint.size,
                tracked.fingerprint.md5
            ),
            (record.generation, body.len() as u64, record.md5)
        );
        assert_eq!(fs::read(fixture.tree.0.join(&item.path)).unwrap(), body);
        assert_eq!(fixture.writeback.status().await.unwrap().entries, 0);
        assert!(fixture.writeback.page(0, 256).await.unwrap().is_empty());
        drop(operation);
        fixture.close().await;
    }
}

#[tokio::test]
async fn live_export_workspace_replacement_preserves_owned_bytes_and_requires_recovery() {
    let fixture = Fixture::new().await;
    fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    fixture
        .writeback
        .advance_live_export(operation.clone())
        .await
        .unwrap();
    let item = pending(&operation).await;
    let workspace = fixture.tree.0.join(mapping::WORKSPACE);
    let retained = fixture.tree.0.join("retained-workspace");
    fs::rename(&workspace, &retained).unwrap();
    fs::create_dir(&workspace).unwrap();
    assert!(matches!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await,
        Err(Error::WorkspaceChanged)
    ));
    assert_eq!(fs::read(retained.join(&item.token.id)).unwrap(), b"api");
    drop(operation);
    fixture.close().await;
}

#[tokio::test]
async fn live_export_obsolete_copy_never_cancels_a_modified_workspace_body() {
    let fixture = Fixture::new().await;
    fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    fixture
        .writeback
        .advance_live_export(operation.clone())
        .await
        .unwrap();
    let item = pending(&operation).await;
    let owned = fixture.tree.0.join(mapping::WORKSPACE).join(&item.token.id);
    fs::write(&owned, b"retained external body").unwrap();
    let winner = fixture.publish("object", b"later api").await;
    assert!(matches!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await,
        Err(Error::Protected)
    ));
    let usage = fixture.writeback.status().await.unwrap();
    assert_eq!(
        (usage.stop, usage.retained_bytes),
        (Stop::ProtectedBody, 22)
    );
    assert_eq!(fs::read(owned).unwrap(), b"retained external body");
    assert_eq!(
        fixture
            .catalog
            .admin(Origin::Api)
            .get_object(&fixture.bucket, &ObjectName::new("object").unwrap(), None)
            .await
            .unwrap(),
        winner
    );
    drop(operation);
    fixture.close().await;
}

#[tokio::test]
async fn live_export_installed_copy_is_never_cancelled_after_arming() {
    let fixture = Fixture::new().await;
    fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    for _ in 0..2 {
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap();
    }
    fs::write(
        fixture.tree.0.join("test-bucket/object"),
        b"later external bytes",
    )
    .unwrap();
    assert!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .is_err()
    );
    assert_eq!(
        fixture.writeback.status().await.unwrap().stop,
        Stop::RecoveryRequired
    );
    assert_eq!(
        fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
        b"later external bytes"
    );
    let binding = fixture
        .catalog
        .sync_state()
        .binding(&fixture.bucket)
        .await
        .unwrap();
    assert!(
        fixture
            .catalog
            .sync_state()
            .get_path(binding, "test-bucket/object".into())
            .await
            .unwrap()
            .is_none()
    );
    drop(operation);
    fixture.close().await;
}

#[tokio::test]
async fn live_export_rejects_foreign_handles_and_obsoletes_a_changed_api_generation() {
    let fixture = Fixture::new().await;
    fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    let other_tree = TestDir::new();
    let other_root = fixture
        .catalog
        .sync_state()
        .activate_root(&other_tree.0, super::super::super::root::Activation::Open)
        .await
        .unwrap();
    let other = other_root.default_writeback().await.unwrap();
    assert!(matches!(
        other.advance_live_export(operation.clone()).await,
        Err(Error::Admin(AdminError::Precondition))
    ));
    fixture
        .writeback
        .advance_live_export(operation.clone())
        .await
        .unwrap();
    let winner = fixture.publish("object", b"new api winner").await;
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap(),
        LiveResult::Obsolete
    );
    assert!(!fixture.tree.0.join("test-bucket/object").exists());
    assert_eq!(
        fixture
            .catalog
            .admin(Origin::Api)
            .get_object(&fixture.bucket, &ObjectName::new("object").unwrap(), None)
            .await
            .unwrap(),
        winner
    );
    assert_eq!(fixture.writeback.status().await.unwrap().entries, 0);
    drop(operation);
    drop(other);
    drop(other_root);
    fixture.close().await;
}

#[tokio::test]
async fn live_export_removal_requires_a_complete_baseline_and_the_tracked_generation() {
    let fixture = Fixture::new().await;
    let record = fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    for _ in 0..4 {
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap();
    }
    drop(operation);
    let name = ObjectName::new("object").unwrap();
    let generation = Generation::new(record.generation).unwrap();
    fixture
        .catalog
        .admin(Origin::Api)
        .delete_object(
            &fixture.bucket,
            &name,
            crate::storage::admin::GenerationCondition::Is(generation),
        )
        .await
        .unwrap();
    let binding = fixture
        .catalog
        .sync_state()
        .binding(&fixture.bucket)
        .await
        .unwrap();
    let tracked = fixture
        .catalog
        .sync_state()
        .get_path(binding.clone(), "test-bucket/object".into())
        .await
        .unwrap();
    let fence = fixture
        .catalog
        .read_changes(None, None, 1)
        .await
        .unwrap()
        .fence;
    assert!(matches!(
        fixture
            .writeback
            .begin_live_export(
                binding.clone(),
                name.clone(),
                generation,
                tracked.clone(),
                fence,
                LiveSource::Observe
            )
            .await,
        Err(Error::Admin(AdminError::Precondition))
    ));
    assert!(fixture.tree.0.join("test-bucket/object").exists());
    fixture.root.complete_baseline().await.unwrap();
    let operation = fixture
        .writeback
        .begin_live_export(
            binding.clone(),
            name,
            generation,
            tracked,
            fence,
            LiveSource::Observe,
        )
        .await
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            fixture
                .writeback
                .advance_live_export(operation.clone())
                .await
                .unwrap(),
            LiveResult::Pending
        );
    }
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap(),
        LiveResult::Complete
    );
    assert!(!fixture.tree.0.join("test-bucket/object").exists());
    assert!(
        fixture
            .catalog
            .sync_state()
            .get_path(binding, "test-bucket/object".into())
            .await
            .unwrap()
            .is_none()
    );
    drop(operation);
    fixture.close().await;
}

#[tokio::test]
async fn live_export_ack_sql_failure_rolls_back_tracking_and_can_be_retried() {
    let fixture = Fixture::new().await;
    fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    for _ in 0..3 {
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap();
    }
    fixture.writeback.db(|connection|{connection.execute_batch("CREATE TEMP TRIGGER refuse_completion BEFORE DELETE ON sync_writeback_ops BEGIN SELECT RAISE(ABORT,'injected completion failure'); END;")?;Ok(())}).await.unwrap();
    assert!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .is_err()
    );
    let binding = fixture
        .catalog
        .sync_state()
        .binding(&fixture.bucket)
        .await
        .unwrap();
    assert!(
        fixture
            .catalog
            .sync_state()
            .get_path(binding, "test-bucket/object".into())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(fixture.writeback.status().await.unwrap().entries, 1);
    fixture
        .writeback
        .db(|connection| {
            connection.execute_batch("DROP TRIGGER refuse_completion")?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap(),
        LiveResult::Complete
    );
    drop(operation);
    fixture.close().await;
}

#[tokio::test]
async fn live_export_other_root_completion_cannot_clear_a_failed_operation() {
    let fixture = Fixture::new().await;
    let record = fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    fixture
        .writeback
        .advance_live_export(operation.clone())
        .await
        .unwrap();
    inject_failure(fixture.root.status().root_id, "displaced-parent-sync", 1);
    assert!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .is_err()
    );
    assert_eq!(
        fixture.writeback.status().await.unwrap().stop,
        Stop::IoFailure
    );
    let tree = TestDir::new();
    let root = fixture
        .catalog
        .sync_state()
        .activate_root(&tree.0, super::super::super::root::Activation::Open)
        .await
        .unwrap();
    let other = root.default_writeback().await.unwrap();
    let name = ObjectName::new("object").unwrap();
    let (_, reader) = fixture
        .catalog
        .admin(Origin::Api)
        .open_object(&fixture.bucket, &name, None)
        .await
        .unwrap();
    let next = other
        .begin_live_export(
            fixture
                .catalog
                .sync_state()
                .binding(&fixture.bucket)
                .await
                .unwrap(),
            name,
            Generation::new(record.generation).unwrap(),
            None,
            fixture
                .catalog
                .read_changes(None, None, 1)
                .await
                .unwrap()
                .fence,
            LiveSource::ApiCopy(reader.into_file().await.unwrap()),
        )
        .await
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            other.advance_live_export(next.clone()).await.unwrap(),
            LiveResult::Pending
        );
    }
    assert_eq!(
        other.advance_live_export(next.clone()).await.unwrap(),
        LiveResult::Complete
    );
    assert_eq!(
        fixture.writeback.status().await.unwrap().stop,
        Stop::IoFailure
    );
    assert!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .is_err()
    );
    assert_eq!(
        fixture.writeback.recover_pending().await.unwrap().stop,
        Stop::Ready
    );
    assert_eq!(
        fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
        b"api"
    );
    drop(next);
    drop(other);
    drop(root);
    drop(operation);
    fixture.close().await;
}

#[tokio::test]
async fn live_export_rejects_held_and_stale_tracked_inputs() {
    use crate::{
        object::ObjectMetadataUpdate,
        storage::test_fixtures::{PERMISSIVE, live},
    };
    let fixture = Fixture::new().await;
    let record = fixture.publish("object", b"api").await;
    let name = ObjectName::new("object").unwrap();
    let generation = Generation::new(record.generation).unwrap();
    let binding = fixture
        .catalog
        .sync_state()
        .binding(&fixture.bucket)
        .await
        .unwrap();
    let fence = fixture
        .catalog
        .read_changes(None, None, 1)
        .await
        .unwrap()
        .fence;
    fixture
        .catalog
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
    assert!(matches!(
        fixture
            .writeback
            .begin_live_export(
                binding.clone(),
                name.clone(),
                generation,
                None,
                fence,
                LiveSource::Observe
            )
            .await,
        Err(Error::Admin(AdminError::Hold))
    ));
    fixture
        .catalog
        .update_object_metadata(
            live("test-bucket", "object"),
            Default::default(),
            ObjectMetadataUpdate {
                temporary_hold: Some(false),
                ..Default::default()
            },
            |_| Ok(()),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let operation = fixture.begin("object").await;
    for _ in 0..4 {
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap();
    }
    let mut tracked = fixture
        .catalog
        .sync_state()
        .get_path(binding.clone(), "test-bucket/object".into())
        .await
        .unwrap()
        .unwrap();
    tracked.fingerprint.inode ^= 1;
    let fence = fixture
        .catalog
        .read_changes(None, None, 1)
        .await
        .unwrap()
        .fence;
    assert!(matches!(
        fixture
            .writeback
            .begin_live_export(
                binding,
                name,
                generation,
                Some(tracked),
                fence,
                LiveSource::Observe
            )
            .await,
        Err(Error::Admin(AdminError::Precondition))
    ));
    assert_eq!(fixture.writeback.status().await.unwrap().entries, 0);
    drop(operation);
    fixture.close().await;
}

#[tokio::test]
async fn live_export_cancelled_install_serializes_other_handles_until_the_owned_job_drains() {
    use crate::storage::catalog::fault::{Fault, Step};
    use std::time::Duration;
    let fixture = Fixture::new().await;
    fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    fixture
        .writeback
        .advance_live_export(operation.clone())
        .await
        .unwrap();
    let item = pending(&operation).await;
    let other = fixture.root.default_writeback().await.unwrap();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    fixture.catalog.inject(
        Step::SyncCopyInstalled,
        0,
        Fault::Block {
            entered: Arc::new(std::sync::Mutex::new(Some(entered))),
            release: Arc::new(std::sync::Mutex::new(blocked)),
        },
    );
    let worker = fixture.writeback.clone();
    let selected = operation.clone();
    let work = crate::task::spawn_supervised("cancelled-install-test", async move {
        worker.advance_live_export(selected).await
    });
    started.await.unwrap();
    work.abort();
    fs::write(
        fixture.tree.0.join(mapping::WORKSPACE).join(&item.token.id),
        b"late displaced body",
    )
    .unwrap();
    let recovering = other.clone();
    let selected = operation.clone();
    let mut completion = crate::task::spawn_supervised("serialized-recovery-test", async move {
        recovering.advance_live_export(selected).await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut completion)
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), other.status())
            .await
            .is_err()
    );
    release.send(()).unwrap();
    assert!(matches!(completion.await.unwrap(), Err(Error::Protected)));
    let usage = other.status().await.unwrap();
    assert_eq!(
        (usage.entries, usage.retained_bytes, usage.stop),
        (1, 19, Stop::ProtectedBody)
    );
    assert_eq!(
        fs::read(fixture.tree.0.join(mapping::WORKSPACE).join(&item.token.id)).unwrap(),
        b"late displaced body"
    );
    assert!(
        other
            .begin_live_export(
                fixture
                    .catalog
                    .sync_state()
                    .binding(&fixture.bucket)
                    .await
                    .unwrap(),
                ObjectName::new("object").unwrap(),
                item.generation,
                None,
                fixture
                    .catalog
                    .read_changes(None, None, 1)
                    .await
                    .unwrap()
                    .fence,
                LiveSource::Observe
            )
            .await
            .is_err()
    );
    drop(other);
    drop(operation);
    fixture.close().await;
}
