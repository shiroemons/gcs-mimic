#![expect(
    clippy::unwrap_used,
    reason = "test fixtures fail at the failing operation"
)]

use super::*;
use crate::{
    clock::SystemClock,
    storage::{
        Catalog,
        admin::{BucketDefinition, Origin, Publication, PublishMode},
        sync_state::{
            authority::ControlAuthority,
            conflicts::{CaptureRequest, Conflict, Usage},
            root::Activation,
        },
    },
    test_support::TestDir,
};
use std::{fs, io::Read};

const BODY: &[u8] = b"external\0\xff";

mod faults;
mod native;
mod snapshots;

struct Page {
    items: Vec<Conflict>,
    usage: Usage,
}

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
    async fn new(bytes: u64, entries: u64) -> Self {
        let data = TestDir::new();
        let tree = TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let bucket = BucketName::new("test-bucket").unwrap();
        catalog
            .admin(Origin::Api)
            .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        fs::write(data.0.join("api"), b"api winner").unwrap();
        let record = catalog
            .admin(Origin::Api)
            .publish_from_file(
                &bucket,
                &ObjectName::new("object").unwrap(),
                Publication {
                    content_type: "text/custom".into(),
                    metadata: Default::default(),
                    expected: Default::default(),
                    condition: None,
                    max_bytes: 4096,
                    mode: PublishMode::Replace,
                },
                File::open(data.0.join("api")).unwrap(),
            )
            .await
            .unwrap();
        catalog.enable_change_journal().await.unwrap();
        let lease = catalog
            .sync_state()
            .activate_root(&tree.0, Activation::Open)
            .await
            .unwrap();
        let writeback = lease
            .writeback(Limits::new(bytes, entries).unwrap())
            .await
            .unwrap();
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
    async fn protected(&self, body: &[u8]) -> Pending {
        let item = self
            .writeback
            .prepare(
                &self.bucket,
                &ObjectName::new("object").unwrap(),
                Generation::new(self.record.generation).unwrap(),
                Kind::Publish,
                self.catalog
                    .read_changes(None, None, 1)
                    .await
                    .unwrap()
                    .fence,
            )
            .await
            .unwrap();
        let item = self.writeback.arm(item.token).await.unwrap();
        fs::write(
            self.tree.0.join(mapping::WORKSPACE).join(item.token.id()),
            body,
        )
        .unwrap();
        self.writeback.inspect(item.token).await.unwrap()
    }
    async fn page(&self) -> Page {
        let page = self
            .lease
            .conflict_page(None, 0, 256, ControlAuthority::Permissive)
            .await
            .unwrap();
        Page {
            items: page.items,
            usage: page.usage,
        }
    }
    async fn assert_api(&self) {
        let (record, reader) = self
            .catalog
            .admin(Origin::Api)
            .open_object(&self.bucket, &ObjectName::new("object").unwrap(), None)
            .await
            .unwrap();
        assert_eq!(record, self.record);
        let mut file = reader.into_file().await.unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"api winner");
    }
    async fn close(self) {
        drop(self.writeback);
        drop(self.lease);
        self.catalog.close().await.unwrap();
    }
    async fn restart(self) -> Self {
        let limits = self.writeback.status().await.unwrap().limits;
        let Self {
            data,
            tree,
            catalog,
            lease,
            writeback,
            bucket,
            record,
        } = self;
        drop(writeback);
        drop(lease);
        catalog.close().await.unwrap();
        let catalog = Catalog::open(&data.0).await.unwrap();
        catalog.enable_change_journal().await.unwrap();
        let lease = catalog
            .sync_state()
            .activate_root(&tree.0, Activation::Open)
            .await
            .unwrap();
        let writeback = lease.writeback(limits).await.unwrap();
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
    async fn preserved(&self) -> (LiveExport, String) {
        fs::create_dir_all(self.tree.0.join("test-bucket")).unwrap();
        if !self.tree.0.join("test-bucket/object").exists() {
            fs::write(self.tree.0.join("test-bucket/object"), BODY).unwrap();
        }
        let binding = self
            .catalog
            .sync_state()
            .binding(&self.bucket)
            .await
            .unwrap();
        let captured = self
            .lease
            .capture_path(CaptureRequest {
                binding: binding.clone(),
                name: ObjectName::new("object").unwrap(),
                expected_generation: Some(Generation::new(self.record.generation).unwrap()),
                source_op: None,
            })
            .await
            .unwrap();
        let id = captured.id().to_owned();
        let (_, reader) = self
            .catalog
            .admin(Origin::Api)
            .open_object(&self.bucket, &ObjectName::new("object").unwrap(), None)
            .await
            .unwrap();
        let operation = self
            .writeback
            .begin_live_export(
                binding,
                ObjectName::new("object").unwrap(),
                Generation::new(self.record.generation).unwrap(),
                None,
                self.catalog
                    .read_changes(None, None, 1)
                    .await
                    .unwrap()
                    .fence,
                LiveSource::Preserved {
                    source: Some(reader.into_file().await.unwrap()),
                    conflict: Box::new(captured),
                },
            )
            .await
            .unwrap();
        (operation, id)
    }
}

#[tokio::test]
async fn protected_transfer_preserves_binary_and_empty_without_changing_api() {
    for body in [BODY, b""] {
        let fixture = Fixture::new(4096, 1000).await;
        let item = fixture.protected(body).await;
        let usage = fixture.writeback.preserve_protected().await.unwrap();
        assert_eq!(
            (usage.entries, usage.retained_bytes, usage.stop),
            (0, 0, Stop::Ready)
        );
        let page = fixture.page().await;
        assert_eq!(page.items.len(), 1);
        assert_eq!(
            (page.usage.entries, page.usage.retained_bytes),
            (1, body.len() as u128)
        );
        assert_eq!(
            fs::read(
                fixture
                    .data
                    .0
                    .join("sync-conflicts")
                    .join(&page.items[0].conflict_id)
            )
            .unwrap(),
            body
        );
        assert!(
            !fixture
                .tree
                .0
                .join(mapping::WORKSPACE)
                .join(item.token.id())
                .exists()
        );
        assert_eq!(
            fixture
                .writeback
                .preserve_protected()
                .await
                .unwrap()
                .entries,
            0
        );
        fixture.assert_api().await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn protected_transfer_requires_temporary_capacity_for_both_actual_copies() {
    let fixture = Fixture::new(BODY.len() as u64 * 2 - 1, 1000).await;
    let item = fixture.protected(BODY).await;
    let usage = fixture.writeback.preserve_protected().await.unwrap();
    assert_eq!(
        (usage.entries, usage.retained_bytes, usage.stop),
        (1, BODY.len() as u128, Stop::ProtectedBody)
    );
    assert!(fixture.page().await.items.is_empty());
    assert_eq!(
        fs::read(
            fixture
                .tree
                .0
                .join(mapping::WORKSPACE)
                .join(item.token.id())
        )
        .unwrap(),
        BODY
    );
    fixture.assert_api().await;
    fixture.close().await;
}

#[tokio::test]
async fn protected_transfer_reuses_registered_copy_and_retains_changed_body_copy() {
    use crate::storage::catalog::fault::{Fault, Step};
    for changed in [false, true] {
        let fixture = Fixture::new(4096, 1000).await;
        let item = fixture.protected(BODY).await;
        fixture.catalog.inject(
            Step::TransferAccepted,
            0,
            Fault::Io(io::ErrorKind::StorageFull),
        );
        assert!(fixture.writeback.preserve_protected().await.is_err());
        assert_eq!(fixture.catalog.pending_faults(), 0);
        let first = fixture.page().await;
        assert_eq!(
            (
                first.items.len(),
                first.usage.entries,
                first.usage.retained_bytes
            ),
            (1, 2, BODY.len() as u128 * 2)
        );
        if changed {
            fs::write(
                fixture
                    .tree
                    .0
                    .join(mapping::WORKSPACE)
                    .join(item.token.id()),
                b"changed\0body",
            )
            .unwrap();
        }
        fixture.writeback.preserve_protected().await.unwrap();
        let page = fixture.page().await;
        assert_eq!(page.items.len(), if changed { 2 } else { 1 });
        assert_eq!(page.items[0].conflict_id, first.items[0].conflict_id);
        assert_eq!(
            fs::read(
                fixture
                    .data
                    .0
                    .join("sync-conflicts")
                    .join(&page.items[0].conflict_id)
            )
            .unwrap(),
            BODY
        );
        fixture.assert_api().await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn preserved_export_uses_exact_byte_and_entry_budget_once() {
    let fixture = Fixture::new(BODY.len() as u64, 1).await;
    fs::create_dir(fixture.tree.0.join("test-bucket")).unwrap();
    fs::write(fixture.tree.0.join("test-bucket/object"), BODY).unwrap();
    let binding = fixture
        .catalog
        .sync_state()
        .binding(&fixture.bucket)
        .await
        .unwrap();
    let captured = fixture
        .lease
        .capture_path(CaptureRequest {
            binding: binding.clone(),
            name: ObjectName::new("object").unwrap(),
            expected_generation: Some(Generation::new(fixture.record.generation).unwrap()),
            source_op: None,
        })
        .await
        .unwrap();
    let (_, reader) = fixture
        .catalog
        .admin(Origin::Api)
        .open_object(&fixture.bucket, &ObjectName::new("object").unwrap(), None)
        .await
        .unwrap();
    let operation = fixture
        .writeback
        .begin_live_export(
            binding,
            ObjectName::new("object").unwrap(),
            Generation::new(fixture.record.generation).unwrap(),
            None,
            fixture
                .catalog
                .read_changes(None, None, 1)
                .await
                .unwrap()
                .fence,
            LiveSource::Preserved {
                source: Some(reader.into_file().await.unwrap()),
                conflict: Box::new(captured),
            },
        )
        .await
        .unwrap();
    assert_eq!(fixture.writeback.status().await.unwrap().reserved_bytes, 0);
    loop {
        if fixture
            .writeback
            .advance_live_export(operation.clone())
            .await
            .unwrap()
            != LiveResult::Pending
        {
            break;
        }
    }
    assert_eq!(
        fs::read(fixture.tree.0.join("test-bucket/object")).unwrap(),
        b"api winner"
    );
    assert_eq!(
        (
            fixture.page().await.usage.entries,
            fixture.page().await.usage.retained_bytes
        ),
        (1, BODY.len() as u128)
    );
    fixture.assert_api().await;
    drop(operation);
    fixture.close().await;
}
