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
mod empty_directory;
mod exchange;
mod migration;
mod removal;
mod timestamps;

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
    let PathMapping::File(path) =
        mapping::name_to_path(bucket, name, writeback.lease.naming().unwrap())
    else {
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
        Self::with_naming(crate::sync::naming::RootNaming::posix()).await
    }

    /// A fixture whose root assumes `naming` instead of probing its filesystem.
    async fn with_naming(naming: crate::sync::naming::RootNaming) -> Self {
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
            .unwrap()
            .assume_naming(naming);
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
        let fence = self.catalog.current_fence().await.unwrap();
        self.writeback
            .begin_live_export(
                binding,
                name,
                Generation::new(record.generation).unwrap(),
                None,
                fence,
                LiveSource::ApiCopy(reader.into_file().unwrap()),
            )
            .await
            .unwrap()
    }
    /// Begins exporting the live generation of `name` over the file its tracked record names.
    async fn begin_over_tracked(&self, name: &str) -> LiveExport {
        let name = ObjectName::new(name).unwrap();
        let binding = self
            .catalog
            .sync_state()
            .binding(&self.bucket)
            .await
            .unwrap();
        let PathMapping::File(path) =
            mapping::name_to_path(&self.bucket, &name, self.writeback.lease.naming().unwrap())
        else {
            panic!("fixture path must be representable")
        };
        let tracked = self
            .catalog
            .sync_state()
            .get_path(binding.clone(), path.to_str().unwrap().to_owned())
            .await
            .unwrap();
        assert!(tracked.is_some(), "{name:?} is not tracked");
        let (record, reader) = self
            .catalog
            .admin(Origin::Api)
            .open_object(&self.bucket, &name, None)
            .await
            .unwrap();
        self.writeback
            .begin_live_export(
                binding,
                name,
                Generation::new(record.generation).unwrap(),
                tracked,
                self.catalog.current_fence().await.unwrap(),
                LiveSource::ApiCopy(reader.into_file().unwrap()),
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
        InstallOutcome::Pending
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
        InstallOutcome::Conflict
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
            .join(item.token.id.to_string())
            .exists()
    );
    let usage = fixture.writeback.status().await.unwrap();
    assert_eq!(
        (usage.entries, usage.reserved_bytes, usage.stop()),
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

/// Installation renames the private entry by name after it confirmed the copy through an open
/// handle, so another file put in that entry meanwhile would be installed instead. Durability
/// hashes whatever the target holds, so such a file, here one of the same size, fails it and
/// never reaches tracking.
#[tokio::test]
async fn durable_rehash_never_tracks_a_file_installed_in_place_of_the_copy() {
    let fixture = Fixture::new().await;
    fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    for _ in 0..2 {
        assert_eq!(
            fixture
                .writeback
                .advance_live_export(operation.clone())
                .await
                .unwrap(),
            InstallOutcome::Pending
        );
    }
    let path = fixture.tree.0.join("test-bucket/object");
    let substitute = fixture.tree.0.join("test-bucket/substitute");
    fs::write(&substitute, b"API").unwrap();
    fs::rename(&substitute, &path).unwrap();
    let result = fixture
        .writeback
        .advance_live_export(operation.clone())
        .await;
    assert!(
        matches!(result, Err(Error::Raced(RaceReason::TargetChanged))),
        "{result:?}"
    );
    assert_eq!(pending(&operation).await.phase, Phase::Armed);
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
    assert_eq!(fs::read(&path).unwrap(), b"API");
    drop(operation);
    fixture.close().await;
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
                InstallOutcome::Pending
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
            InstallOutcome::Complete
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
    assert_eq!(
        fs::read(retained.join(item.token.id.to_string())).unwrap(),
        b"api"
    );
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
    let owned = fixture
        .tree
        .0
        .join(mapping::WORKSPACE)
        .join(item.token.id.to_string());
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
        (usage.stop(), usage.retained_bytes),
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

/// Docker Desktop's shared folders can report one inode number for a file just created and
/// another for it later. A staged copy recorded under a number that no later observation repeats
/// still installs once the same handle proves its bytes, and tracking records the installed file.
#[tokio::test]
async fn live_export_installs_a_staged_copy_whose_inode_number_alone_changed() {
    let fixture = Fixture::new().await;
    let record = fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    inject_renumbered_copy(fixture.root.status().root_id, 1);
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap(),
        InstallOutcome::Pending
    );
    let item = pending(&operation).await;
    let staged = operation.0.lock().await.progress.staged().unwrap();
    let copy = fixture
        .tree
        .0
        .join(mapping::WORKSPACE)
        .join(item.token.id.to_string());
    let observed = fsutil::stat_identity(&File::open(&copy).unwrap()).unwrap();
    assert!(staged.changed_fields(&observed).is_inode_only());
    for expected in [
        InstallOutcome::Pending,
        InstallOutcome::Pending,
        InstallOutcome::Complete,
    ] {
        assert_eq!(
            fixture
                .writeback
                .advance_live_export(operation.clone())
                .await
                .unwrap(),
            expected
        );
    }
    let path = fixture.tree.0.join(&item.path);
    assert_eq!(fs::read(&path).unwrap(), b"api");
    let binding = fixture
        .catalog
        .sync_state()
        .binding(&fixture.bucket)
        .await
        .unwrap();
    let tracked = fixture
        .catalog
        .sync_state()
        .get_path(binding, item.path.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (tracked.generation.get(), tracked.fingerprint.inode),
        (
            record.generation,
            fsutil::stat_identity(&File::open(&path).unwrap())
                .unwrap()
                .inode
        )
    );
    let usage = fixture.writeback.status().await.unwrap();
    assert_eq!((usage.entries, usage.stop()), (0, Stop::Ready));
    drop(operation);
    fixture.close().await;
}

/// A renumbered but unchanged private copy is still this operation's own: an API change before
/// arming cancels it as obsolete and releases its reservation instead of protecting it.
#[tokio::test]
async fn live_export_cancels_a_renumbered_unchanged_copy_as_obsolete() {
    let fixture = Fixture::new().await;
    fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    inject_renumbered_copy(fixture.root.status().root_id, 1);
    fixture
        .writeback
        .advance_live_export(operation.clone())
        .await
        .unwrap();
    let item = pending(&operation).await;
    fixture.publish("object", b"later api").await;
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap(),
        InstallOutcome::Obsolete
    );
    assert!(
        !fixture
            .tree
            .0
            .join(mapping::WORKSPACE)
            .join(item.token.id.to_string())
            .exists()
    );
    assert!(!fixture.tree.0.join(&item.path).exists());
    let usage = fixture.writeback.status().await.unwrap();
    assert_eq!(
        (usage.entries, usage.reserved_bytes, usage.stop()),
        (0, 0, Stop::Ready)
    );
    drop(operation);
    fixture.close().await;
}

/// Only the inode number may change, never the bytes. A private copy rewritten after staging is
/// protected at installation and at cancellation alike: when only its inode number differs from
/// the staged record, its bytes are hashed and differ; when its size differs too, nothing needs
/// reading.
#[tokio::test]
async fn live_export_protects_a_private_copy_whose_bytes_changed_after_staging() {
    for check in [CopyCheck::Install, CopyCheck::Cancel] {
        for inode_only in [true, false] {
            let fixture = Fixture::new().await;
            fixture.publish("object", b"api").await;
            let operation = fixture.begin("object").await;
            if !inode_only {
                inject_renumbered_copy(fixture.root.status().root_id, 1);
            }
            fixture
                .writeback
                .advance_live_export(operation.clone())
                .await
                .unwrap();
            let item = pending(&operation).await;
            let owned = fixture
                .tree
                .0
                .join(mapping::WORKSPACE)
                .join(item.token.id.to_string());
            let rewritten: &[u8] = if inode_only { b"API" } else { b"longer body" };
            fs::write(&owned, rewritten).unwrap();
            if inode_only {
                let current = fsutil::stat_identity(&File::open(&owned).unwrap()).unwrap();
                operation.0.lock().await.progress = Progress::Staged(Some(fsutil::FileIdentity {
                    inode: !current.inode,
                    ..current
                }));
            }
            if matches!(check, CopyCheck::Cancel) {
                fixture.publish("object", b"later api").await;
            }
            let result = fixture
                .writeback
                .advance_live_export(operation.clone())
                .await;
            let case = (check.as_str(), inode_only);
            assert!(
                matches!(result, Err(Error::Protected)),
                "{case:?} {result:?}"
            );
            assert_eq!(fs::read(&owned).unwrap(), rewritten, "{case:?}");
            assert!(!fixture.tree.0.join(&item.path).exists(), "{case:?}");
            assert_eq!(
                fixture.writeback.status().await.unwrap().stop(),
                Stop::ProtectedBody,
                "{case:?}"
            );
            drop(operation);
            fixture.close().await;
        }
    }
}

/// A target whose tracked record names another inode number, as Docker Desktop's shared folders
/// can report for an unchanged file, still holds the tracked body: its size, mtime and hashes
/// match, so a newer API generation replaces it instead of becoming a conflict. A target with the
/// same size and mtime but other bytes is still a conflict.
#[tokio::test]
async fn live_export_replaces_a_tracked_target_whose_inode_number_alone_changed() {
    use crate::storage::sync_state::{SyncFingerprint, SyncPath, SyncUpdate};
    for edited in [false, true] {
        let fixture = Fixture::new().await;
        let record = fixture.publish("object", b"api").await;
        drain(&fixture.writeback, fixture.begin("object").await).await;
        let name = ObjectName::new("object").unwrap();
        let path = fixture.tree.0.join("test-bucket/object");
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
            .unwrap()
            .unwrap();
        fixture
            .catalog
            .sync_state()
            .acknowledge(
                &name,
                Generation::new(record.generation).unwrap(),
                SyncUpdate::new(
                    binding.clone(),
                    crate::storage::sync_state::durable::Durable::assumed(
                        SyncPath::try_from(tracked.path.clone()).unwrap(),
                    ),
                    SyncFingerprint {
                        inode: !tracked.fingerprint.inode,
                        ..tracked.fingerprint.clone()
                    },
                )
                .unwrap(),
            )
            .await
            .unwrap();
        if edited {
            let modified = fs::metadata(&path).unwrap().modified().unwrap();
            fs::write(&path, b"API").unwrap();
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }
        fixture.publish("object", b"later api").await;
        let operation = fixture.begin_over_tracked("object").await;
        let expected: (InstallOutcome, &[u8]) = if edited {
            (InstallOutcome::Conflict, b"API")
        } else {
            (InstallOutcome::Complete, b"later api")
        };
        assert_eq!(
            (
                settle(&fixture.writeback, operation).await,
                fs::read(&path).unwrap().as_slice()
            ),
            expected,
            "edited: {edited}"
        );
        let usage = fixture.writeback.status().await.unwrap();
        assert_eq!(
            (usage.entries, usage.stop()),
            (0, Stop::Ready),
            "edited: {edited}"
        );
        fixture.close().await;
    }
}

/// A failure before installation confirms a renumbered copy leaves the operation's staged
/// identity as it was recorded. That copy, under its new number, is still this operation's own:
/// the failure stays an ordinary retryable error instead of a displacement that stops the root,
/// and the retry installs it.
#[tokio::test]
async fn live_export_retries_a_renumbered_copy_after_a_failure_before_installation_confirmed_it() {
    let fixture = Fixture::new().await;
    fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    let root = fixture.root.status().root_id;
    inject_renumbered_copy(root, 1);
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap(),
        InstallOutcome::Pending
    );
    inject_failure(root, "install", 1);
    let result = fixture
        .writeback
        .advance_live_export(operation.clone())
        .await;
    assert!(
        matches!(result, Err(Error::Admin(AdminError::DiskFull(_)))),
        "{result:?}"
    );
    // The unfinished operation keeps the root in recovery until it completes; a displacement
    // would also protect the entry and mark the operation failed.
    let usage = fixture.writeback.status().await.unwrap();
    assert_eq!(
        (usage.entries, usage.protected_entries, usage.stop()),
        (1, 0, Stop::RecoveryRequired)
    );
    drain(&fixture.writeback, operation).await;
    assert_eq!(
        fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
        b"api"
    );
    let usage = fixture.writeback.status().await.unwrap();
    assert_eq!((usage.entries, usage.stop()), (0, Stop::Ready));
    fixture.close().await;
}

/// Accepting a renumbered copy does not hide a real displacement: when the exchange already
/// moved the original target into the private copy's entry and a later step fails, that entry
/// holds other bytes, so the failure is still reported as displaced and the original kept.
#[tokio::test]
async fn live_export_still_reports_a_displaced_target_beside_a_renumbered_copy() {
    let fixture = Fixture::new().await;
    fixture.publish("object", b"api").await;
    drain(&fixture.writeback, fixture.begin("object").await).await;
    fixture.publish("object", b"later api").await;
    let operation = fixture.begin_over_tracked("object").await;
    let root = fixture.root.status().root_id;
    inject_renumbered_copy(root, 1);
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap(),
        InstallOutcome::Pending
    );
    let item = pending(&operation).await;
    inject_failure(root, "installed", 1);
    let result = fixture
        .writeback
        .advance_live_export(operation.clone())
        .await;
    assert!(matches!(result, Err(Error::Displaced(_))), "{result:?}");
    assert_eq!(
        fs::read(
            fixture
                .tree
                .0
                .join(mapping::WORKSPACE)
                .join(item.token.id.to_string())
        )
        .unwrap(),
        b"api"
    );
    assert_eq!(
        fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
        b"later api"
    );
    let usage = fixture.writeback.status().await.unwrap();
    assert_eq!(
        (usage.protected_entries, usage.stop()),
        (1, Stop::IoFailure)
    );
    drop(operation);
    fixture.close().await;
}

/// Advances `operation` until it stops being pending, and returns how it finished.
async fn settle(writeback: &Writeback, operation: LiveExport) -> InstallOutcome {
    for _ in 0..8 {
        let result = writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap();
        if result != InstallOutcome::Pending {
            return result;
        }
    }
    panic!("the live export did not finish")
}

/// Advances `operation` to completion.
pub(super) async fn drain(writeback: &Writeback, operation: LiveExport) {
    assert_eq!(settle(writeback, operation).await, InstallOutcome::Complete);
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
        fixture.writeback.status().await.unwrap().stop(),
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

/// A changed API generation makes an export obsolete. A data directory synchronizes one
/// directory, so another root exists only after a switch, which forgets the replaced root's
/// operations: the root that replaced it refuses such a handle, and so does the replaced root's
/// own lease.
#[tokio::test]
async fn live_export_rejects_forgotten_handles_and_obsoletes_a_changed_api_generation() {
    let fixture = Fixture::new().await;
    fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
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
        InstallOutcome::Obsolete
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

    fixture.publish("pending", b"api").await;
    let pending = fixture.begin("pending").await;
    let other_tree = TestDir::new();
    let other_root = fixture
        .catalog
        .sync_state()
        .activate_root(&other_tree.0, super::super::super::root::Activation::Open)
        .await
        .unwrap()
        .assume_naming(crate::sync::naming::RootNaming::posix());
    let other = other_root.default_writeback().await.unwrap();
    assert_eq!(
        other.advance_live_export(pending.clone()).await.err(),
        Some(Error::Obsolete(StaleReason::Root))
    );
    assert!(
        fixture
            .writeback
            .advance_live_export(pending.clone())
            .await
            .is_err()
    );
    assert!(!fixture.tree.0.join("test-bucket/pending").exists());
    assert!(!other_tree.0.join("test-bucket/pending").exists());
    drop(pending);
    drop(other);
    drop(other_root);
    fixture.close().await;
}

/// An API deletion reaches the tree before the baseline completes, but only for the generation
/// the tree is tracked at.
#[tokio::test]
async fn live_export_removal_requires_the_tracked_generation_but_not_a_complete_baseline() {
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
    let fence = fixture.catalog.current_fence().await.unwrap();
    assert!(!fixture.root.observation().await.unwrap().baseline_complete);
    assert_eq!(
        fixture
            .writeback
            .begin_live_export(
                binding.clone(),
                name.clone(),
                Generation::new(record.generation + 1).unwrap(),
                tracked.clone(),
                fence,
                LiveSource::Observe
            )
            .await
            .err(),
        Some(Error::Obsolete(StaleReason::Tracked))
    );
    assert!(fixture.tree.0.join("test-bucket/object").exists());
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
            InstallOutcome::Pending
        );
    }
    assert_eq!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap(),
        InstallOutcome::Complete
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
        InstallOutcome::Complete
    );
    drop(operation);
    fixture.close().await;
}

/// An operation that failed stops the root until recovery. A switch to another directory forgets
/// it, and the replaced root's lease can no longer advance it; the root that replaced it exports
/// the object into its own tree, and the replaced tree is left as the failure left it.
#[tokio::test]
async fn live_export_failed_operation_is_forgotten_by_a_switch() {
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
        fixture.writeback.status().await.unwrap().stop(),
        Stop::IoFailure
    );
    let before = fs::read_dir(&fixture.tree.0).unwrap().count();
    let tree = TestDir::new();
    let root = fixture
        .catalog
        .sync_state()
        .activate_root(&tree.0, super::super::super::root::Activation::Open)
        .await
        .unwrap()
        .assume_naming(crate::sync::naming::RootNaming::posix());
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
            fixture.catalog.current_fence().await.unwrap(),
            LiveSource::ApiCopy(reader.into_file().unwrap()),
        )
        .await
        .unwrap();
    for _ in 0..3 {
        assert_eq!(
            other.advance_live_export(next.clone()).await.unwrap(),
            InstallOutcome::Pending
        );
    }
    assert_eq!(
        other.advance_live_export(next.clone()).await.unwrap(),
        InstallOutcome::Complete
    );
    assert_eq!(fs::read(tree.0.join("test-bucket/object")).unwrap(), b"api");
    assert_eq!(other.status().await.unwrap().stop(), Stop::Ready);
    assert!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .is_err()
    );
    assert_eq!(fs::read_dir(&fixture.tree.0).unwrap().count(), before);
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
    let fence = fixture.catalog.current_fence().await.unwrap();
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
    let fence = fixture.catalog.current_fence().await.unwrap();
    assert_eq!(
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
            .await
            .err(),
        Some(Error::Obsolete(StaleReason::Tracked))
    );
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
        fixture
            .tree
            .0
            .join(mapping::WORKSPACE)
            .join(item.token.id.to_string()),
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
        (usage.entries, usage.retained_bytes, usage.stop()),
        (1, 19, Stop::ProtectedBody)
    );
    assert_eq!(
        fs::read(
            fixture
                .tree
                .0
                .join(mapping::WORKSPACE)
                .join(item.token.id.to_string())
        )
        .unwrap(),
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
                fixture.catalog.current_fence().await.unwrap(),
                LiveSource::Observe
            )
            .await
            .is_err()
    );
    drop(other);
    drop(operation);
    fixture.close().await;
}

/// An observation whose target fails to open with `ENOENT` for a moment, while the name still
/// holds the exported body, completes once the open succeeds again rather than taking the
/// target for changed.
#[tokio::test]
async fn an_observation_waits_out_a_target_that_briefly_fails_to_open() {
    let fixture = Fixture::new().await;
    let record = fixture.publish("object", b"api").await;
    let operation = fixture.begin("object").await;
    for _ in 0..8 {
        if fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap()
            == InstallOutcome::Complete
        {
            break;
        }
    }
    drop(operation);
    let operation = observation(
        &fixture.writeback,
        &fixture.bucket,
        &ObjectName::new("object").unwrap(),
        Generation::new(record.generation).unwrap(),
        fixture.catalog.current_fence().await.unwrap(),
    )
    .await;
    let vanishing =
        fsutil::FailingOpens::vanishing(&fixture.tree.0.join("test-bucket"), "object", 2);
    let mut outcome = InstallOutcome::Pending;
    for _ in 0..8 {
        outcome = fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap();
        if outcome != InstallOutcome::Pending {
            break;
        }
    }
    assert_eq!(outcome, InstallOutcome::Complete);
    assert_eq!(vanishing.left(), 0);
    assert_eq!(
        fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
        b"api"
    );
    drop(operation);
    fixture.close().await;
}

/// A private workspace entry that keeps failing to open while its name still holds it is a race
/// the operation is read again after, never a name that holds nothing: taken for empty, the
/// body an exchange displaced into the workspace would stay there with nothing tracking it. One
/// that fails only for a moment is the entry.
#[test]
fn a_workspace_entry_that_keeps_failing_to_open_is_never_empty() {
    let workspace = TestDir::new();
    let id = OpaqueId::SAMPLE;
    fs::write(workspace.0.join(id.to_hex()), b"displaced").unwrap();
    let directory = File::open(&workspace.0).unwrap();
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(std::time::SystemTime::now()));
    let lookup = fsutil::Lookup::probed(fsutil::Matching::Exact, clock);
    let briefly = fsutil::FailingOpens::vanishing(&workspace.0, &id.to_hex(), 2);
    assert!(entry(&directory, &lookup, id).unwrap().is_some());
    assert_eq!(briefly.left(), 0);
    drop(briefly);
    let _failing = fsutil::FailingOpens::vanishing(&workspace.0, &id.to_hex(), usize::MAX);
    assert_eq!(
        entry(&directory, &lookup, id).map(|file| file.is_some()),
        Err(Error::Raced(RaceReason::Unsettled))
    );
    assert_eq!(
        displaced_entry(&directory, &lookup, id).map(|file| file.is_some()),
        Err(Error::Raced(RaceReason::Unsettled))
    );
}
