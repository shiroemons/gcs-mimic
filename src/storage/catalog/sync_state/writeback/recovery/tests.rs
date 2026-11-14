#![expect(
    clippy::unwrap_used,
    reason = "test fixtures fail at the failing operation"
)]

use super::*;
use crate::storage::catalog::sync_state::tests::Journal;
use crate::storage::test_fixtures::PERMISSIVE;
use crate::{
    clock::SystemClock,
    storage::{
        Catalog,
        admin::{BucketDefinition, Origin, Publication, PublishMode},
    },
    test_support::TestDir,
};
use std::{fs, io::Read};

mod snapshots;

const API: &[u8] = b"api\0\xffwinner";

struct Fixture {
    data: TestDir,
    tree: TestDir,
    catalog: Catalog,
    lease: RootLease,
    writeback: Writeback,
    bucket: BucketName,
    record: crate::object::ObjectRecord,
}
impl Fixture {
    async fn new() -> Self {
        let data = TestDir::new();
        let tree = TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let bucket = BucketName::new("test-bucket").unwrap();
        catalog
            .admin(Origin::SyncIngest)
            .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        fs::write(data.0.join("input"), API).unwrap();
        let record = catalog
            .admin(Origin::Api)
            .publish_from_file(
                &bucket,
                &ObjectName::new("object").unwrap(),
                publication(),
                File::open(data.0.join("input")).unwrap(),
            )
            .await
            .unwrap();
        catalog.enable_change_journal().await.unwrap();
        let lease = catalog
            .sync_state()
            .activate_root(&tree.0, super::super::super::root::Activation::Open)
            .await
            .unwrap();
        let writeback = lease.writeback(limits()).await.unwrap();
        Self {
            data,
            tree,
            catalog,
            lease,
            writeback,
            bucket,
            record,
        }
    }
    async fn prepare(&self) -> Pending {
        let operation = begin_observation(
            &self.writeback,
            &self.bucket,
            &ObjectName::new("object").unwrap(),
            Generation::new(self.record.generation).unwrap(),
            self.catalog
                .read_changes(None, None, 1)
                .await
                .unwrap()
                .fence,
        )
        .await;
        live_pending(&operation).await
    }
    async fn armed(&self) -> Pending {
        armed_observation(
            &self.writeback,
            &self.bucket,
            &ObjectName::new("object").unwrap(),
            Generation::new(self.record.generation).unwrap(),
            self.catalog
                .read_changes(None, None, 1)
                .await
                .unwrap()
                .fence,
        )
        .await
    }
    async fn durable(&self) -> Pending {
        let operation = self.live().await;
        for _ in 0..3 {
            self.writeback
                .advance_live_export(operation.clone())
                .await
                .unwrap();
        }
        let item = live_pending(&operation).await;
        assert_eq!(item.phase, Phase::Durable);
        item
    }
    async fn live(&self) -> LiveExport {
        let name = ObjectName::new("object").unwrap();
        let binding = self
            .catalog
            .sync_state()
            .binding(&self.bucket)
            .await
            .unwrap();
        let (_, reader) = self
            .catalog
            .admin(Origin::Api)
            .open_object(&self.bucket, &name, None)
            .await
            .unwrap();
        self.writeback
            .begin_live_export(
                binding,
                name,
                Generation::new(self.record.generation).unwrap(),
                None,
                self.catalog
                    .read_changes(None, None, 1)
                    .await
                    .unwrap()
                    .fence,
                LiveSource::ApiCopy(reader.into_file().await.unwrap()),
            )
            .await
            .unwrap()
    }
    async fn restart(self, journal: Journal) -> Self {
        let (data, tree, bucket, record) = self.shut_down().await;
        let catalog = Catalog::open(&data.0).await.unwrap();
        journal.restore(&catalog).await;
        let lease = catalog
            .sync_state()
            .activate_root(&tree.0, super::super::super::root::Activation::Open)
            .await
            .unwrap();
        let writeback = lease.writeback(limits()).await.unwrap();
        Self {
            data,
            tree,
            catalog,
            lease,
            writeback,
            bucket,
            record,
        }
    }
    async fn api(&self) {
        assert_api(&self.catalog, &self.record).await;
    }
    async fn close(self) {
        self.shut_down().await;
    }
    async fn shut_down(self) -> (TestDir, TestDir, BucketName, crate::object::ObjectRecord) {
        drop(self.writeback);
        drop(self.lease);
        self.catalog.close().await.unwrap();
        (self.data, self.tree, self.bucket, self.record)
    }
}
fn limits() -> Limits {
    Limits::new(1 << 30, 1000).unwrap()
}
fn publication() -> Publication {
    Publication {
        content_type: "application/octet-stream".into(),
        metadata: Default::default(),
        expected: Default::default(),
        condition: None,
        max_bytes: 4096,
        mode: PublishMode::Replace,
    }
}
async fn assert_api(catalog: &Catalog, expected: &crate::object::ObjectRecord) {
    let bucket = BucketName::new("test-bucket").unwrap();
    let name = ObjectName::new("object").unwrap();
    assert_eq!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket, &name, None)
            .await
            .unwrap(),
        *expected
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
    assert_eq!(bytes, API);
}

#[tokio::test]
async fn restart_cancels_only_unarmed_owned_copies_including_short_and_zero_bytes() {
    for body in [None, Some(&b""[..]), Some(&b"api"[..]), Some(API)] {
        let fixture = Fixture::new().await;
        let item = fixture.prepare().await;
        let workspace = fixture.tree.0.join(mapping::WORKSPACE);
        if let Some(body) = body {
            fs::write(workspace.join(item.token.id()), body).unwrap();
        }
        fs::write(workspace.join("unrelated"), b"keep workspace").unwrap();
        fs::create_dir(fixture.tree.0.join("test-bucket")).unwrap();
        fs::write(
            fixture.tree.0.join("test-bucket/object"),
            b"new external target",
        )
        .unwrap();
        let fixture = fixture.restart(Journal::Disabled).await;
        let status = fixture.writeback.recover_pending().await.unwrap();
        assert_eq!(
            (status.entries, status.reserved_bytes, status.stop),
            (0, 0, Stop::Ready)
        );
        assert!(!workspace.join(item.token.id()).exists());
        assert_eq!(
            fs::read(workspace.join("unrelated")).unwrap(),
            b"keep workspace"
        );
        assert_eq!(
            fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
            b"new external target"
        );
        assert_eq!(
            fixture.writeback.recover_pending().await.unwrap().entries,
            0
        );
        fixture.api().await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn restart_armed_and_durable_absence_preserves_the_current_target() {
    for phase in [Phase::Armed, Phase::Durable] {
        let fixture = Fixture::new().await;
        match phase {
            Phase::Durable => fixture.durable().await,
            _ => fixture.armed().await,
        };
        fs::create_dir_all(fixture.tree.0.join("test-bucket")).unwrap();
        fs::write(fixture.tree.0.join("test-bucket/object"), b"later writer").unwrap();
        let fixture = fixture.restart(Journal::Disabled).await;
        assert_eq!(
            fixture.writeback.recover_pending().await.unwrap().stop,
            Stop::Ready
        );
        assert_eq!(
            fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
            b"later writer"
        );
        fixture.api().await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn restart_protects_actual_armed_and_durable_bodies_without_quota_eviction() {
    for (phase, size) in [
        (Phase::Armed, 0),
        (Phase::Armed, 10),
        (Phase::Armed, 1 << 30),
        (Phase::Durable, (1 << 30) + 1),
        (Phase::Durable, (1 << 40) + 1),
    ] {
        let fixture = Fixture::new().await;
        let item = match phase {
            Phase::Durable => fixture.durable().await,
            _ => fixture.armed().await,
        };
        let path = fixture
            .tree
            .0
            .join(mapping::WORKSPACE)
            .join(item.token.id());
        File::create(&path).unwrap().set_len(size).unwrap();
        let fixture = fixture.restart(Journal::Disabled).await;
        let usage = fixture.writeback.recover_pending().await.unwrap();
        assert_eq!(
            (
                usage.entries,
                usage.protected_entries,
                usage.retained_bytes,
                usage.reserved_bytes
            ),
            (1, 1, u128::from(size), 0)
        );
        assert_eq!(
            usage.stop,
            if size > (1 << 30) {
                Stop::ConflictCapacity
            } else {
                Stop::ProtectedBody
            }
        );
        let again = fixture.writeback.recover_pending().await.unwrap();
        assert_eq!((again.entries, again.retained_bytes), (1, u128::from(size)));
        assert_eq!(fs::metadata(path).unwrap().len(), size);
        let pending = fixture.writeback.page(0, 256).await.unwrap();
        assert_eq!(pending[0].owner, "owner");
        assert_eq!(pending[0].incarnation, item.incarnation);
        fixture.api().await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn restart_live_copy_at_prepare_copy_install_durable_and_ack_boundaries() {
    for advances in 0..=4 {
        let fixture = Fixture::new().await;
        fs::create_dir(fixture.tree.0.join("test-bucket")).unwrap();
        let operation = fixture.live().await;
        for _ in 0..advances {
            fixture
                .writeback
                .advance_live_export(operation.clone())
                .await
                .unwrap();
        }
        drop(operation);
        let fixture = fixture.restart(Journal::Disabled).await;
        assert_eq!(
            fixture.writeback.recover_pending().await.unwrap().stop,
            Stop::Ready
        );
        assert_eq!(
            fixture.tree.0.join("test-bucket/object").exists(),
            advances >= 2
        );
        if advances >= 2 {
            assert_eq!(
                fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
                API
            );
        }
        fixture.api().await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn recovery_keyset_pages_drain_more_than_one_page_without_skipping_cancelled_rows() {
    let fixture = Fixture::new().await;
    let first = fixture.prepare().await;
    // Legacy catalogs can contain multiple prepared intents; current exports serialize them.
    fixture.writeback.db(move |connection| {
        let tx=connection.transaction()?;
        for index in 1..=256 {
            tx.prepare_cached("INSERT INTO sync_writeback_ops(id,root_id,owner,bucket,incarnation,name,path,generation,kind,phase,revision,fence_sequence,fence_epoch,reservation,size,md5,crc32c) SELECT ?1,root_id,owner,bucket,incarnation,?2,?3,generation,kind,0,1,fence_sequence,fence_epoch,reservation,size,md5,crc32c FROM sync_writeback_ops WHERE id=?4")?.execute(params![format!("{index:032x}"),format!("extra-{index}"),format!("test-bucket/extra-{index}"),first.token.id])?;
        }
        tx.prepare_cached("UPDATE sync_writeback_roots SET entries=257 WHERE root_id=?1")?.execute([first.token.root])?;
        tx.commit()?;Ok(())
    }).await.unwrap();
    assert_eq!(fixture.writeback.status().await.unwrap().entries, 257);
    let fixture = fixture.restart(Journal::Disabled).await;
    let usage = fixture.writeback.recover_pending().await.unwrap();
    assert_eq!(
        (usage.entries, usage.reserved_bytes, usage.stop),
        (0, 0, Stop::Ready)
    );
    assert!(fixture.writeback.page(0, 256).await.unwrap().is_empty());
    fixture.api().await;
    fixture.close().await;
}

#[tokio::test]
async fn recovery_keeps_the_original_owner_after_api_change_delete_and_bucket_recreation() {
    let mut fixture = Fixture::new().await;
    let item = fixture.armed().await;
    let owned = fixture
        .tree
        .0
        .join(mapping::WORKSPACE)
        .join(item.token.id());
    fs::write(&owned, b"external").unwrap();
    let name = ObjectName::new("object").unwrap();
    let updated = fixture
        .catalog
        .admin(Origin::Api)
        .publish_from_file(
            &fixture.bucket,
            &name,
            publication(),
            File::open(fixture.data.0.join("input")).unwrap(),
        )
        .await
        .unwrap();
    fixture
        .catalog
        .admin(Origin::Api)
        .delete_object(
            &fixture.bucket,
            &name,
            crate::storage::admin::GenerationCondition::Is(
                Generation::new(updated.generation).unwrap(),
            ),
        )
        .await
        .unwrap();
    fixture
        .catalog
        .delete_bucket(fixture.bucket.as_str(), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    fixture
        .catalog
        .admin(Origin::BucketInit)
        .ensure_bucket(
            BucketDefinition::default_for(&fixture.bucket, "new-owner", &SystemClock).unwrap(),
        )
        .await
        .unwrap();
    fixture.record = fixture
        .catalog
        .admin(Origin::Api)
        .publish_from_file(
            &fixture.bucket,
            &name,
            publication(),
            File::open(fixture.data.0.join("input")).unwrap(),
        )
        .await
        .unwrap();
    let fixture = fixture.restart(Journal::Disabled).await;
    let usage = fixture.writeback.recover_pending().await.unwrap();
    assert_eq!((usage.stop, usage.retained_bytes), (Stop::ProtectedBody, 8));
    let pending = fixture.writeback.page(0, 1).await.unwrap().pop().unwrap();
    assert_eq!(pending.owner, "owner");
    assert_eq!(pending.incarnation, item.incarnation);
    assert_eq!(pending.generation, item.generation);
    assert_eq!(fs::read(owned).unwrap(), b"external");
    fixture.api().await;
    fixture.close().await;
}

#[tokio::test]
async fn recovery_protects_unexpected_unarmed_remove_bodies_and_rejects_workspace_replacement() {
    let fixture = Fixture::new().await;
    let item = fixture.prepare().await;
    let selected = item.token.id.clone();
    fixture
        .writeback
        .db(move |connection| {
            connection
                .prepare_cached("UPDATE sync_writeback_ops SET kind=1 WHERE id=?1")?
                .execute([selected])?;
            Ok(())
        })
        .await
        .unwrap();
    let workspace = fixture.tree.0.join(mapping::WORKSPACE);
    fs::write(workspace.join(item.token.id()), b"unexplained").unwrap();
    assert!(matches!(
        fixture.writeback.recover_pending().await,
        Err(Error::Protected)
    ));
    assert_eq!(fixture.writeback.status().await.unwrap().retained_bytes, 11);
    assert_eq!(
        fs::read(workspace.join(item.token.id())).unwrap(),
        b"unexplained"
    );
    let displaced = fixture.tree.0.join("retained-workspace");
    fs::rename(&workspace, &displaced).unwrap();
    fs::create_dir(&workspace).unwrap();
    assert!(matches!(
        fixture.writeback.recover_pending().await,
        Err(Error::WorkspaceChanged)
    ));
    assert_eq!(
        fs::read(displaced.join(item.token.id())).unwrap(),
        b"unexplained"
    );
    fixture.close().await;
}

#[tokio::test]
async fn restart_after_arming_a_live_copy_retains_the_uninstalled_body() {
    let fixture = Fixture::new().await;
    fs::create_dir(fixture.tree.0.join("test-bucket")).unwrap();
    let operation = fixture.live().await;
    fixture
        .writeback
        .advance_live_export(operation.clone())
        .await
        .unwrap();
    super::super::live_export::inject_failure(fixture.lease.status().root_id, "install", 1);
    assert!(
        fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .is_err()
    );
    drop(operation);
    let fixture = fixture.restart(Journal::Disabled).await;
    let usage = fixture.writeback.recover_pending().await.unwrap();
    assert_eq!(
        (usage.stop, usage.retained_bytes),
        (Stop::ProtectedBody, API.len() as u128)
    );
    assert!(!fixture.tree.0.join("test-bucket/object").exists());
    fixture.api().await;
    fixture.close().await;
}
