#![expect(
    clippy::unwrap_used,
    reason = "test fixtures fail at the failing operation"
)]

use super::*;
use crate::{
    clock::SystemClock,
    storage::{
        Catalog,
        admin::{BucketDefinition, BucketName, Origin, Publication, PublishMode},
        sync_state::{root::Activation, writeback},
    },
    test_support::TestDir,
};
use std::{fs, io::Read};

mod faults;
mod path_reuse;
mod recovery;

struct Fixture {
    data: TestDir,
    tree: TestDir,
    catalog: Catalog,
    lease: RootLease,
    bucket: BucketName,
    winner: ObjectRecord,
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
        fs::write(data.0.join("input"), b"api winner").unwrap();
        let winner = catalog
            .admin(Origin::SyncIngest)
            .publish_from_file(
                &bucket,
                &ObjectName::new("object.txt").unwrap(),
                Publication {
                    content_type: "text/custom".into(),
                    metadata: Default::default(),
                    expected: Default::default(),
                    condition: None,
                    max_bytes: 4096,
                    mode: PublishMode::Replace,
                },
                File::open(data.0.join("input")).unwrap(),
            )
            .await
            .unwrap();
        let lease = catalog
            .sync_state()
            .activate_root(&tree.0, Activation::Open)
            .await
            .unwrap();
        fs::write(tree.0.join("external"), b"external\0\xff").unwrap();
        Self {
            data,
            tree,
            catalog,
            lease,
            bucket,
            winner,
        }
    }
    async fn request(&self) -> CaptureRequest {
        CaptureRequest {
            binding: self
                .catalog
                .sync_state()
                .binding(&self.bucket)
                .await
                .unwrap(),
            name: ObjectName::new("object.txt").unwrap(),
            expected_generation: Some(Generation::new(self.winner.generation).unwrap()),
            source_op: None,
        }
    }
    async fn capture(&self) -> CapturedConflict {
        self.lease
            .capture_conflict(
                self.request().await,
                File::open(self.tree.0.join("external")).unwrap(),
            )
            .await
            .unwrap()
    }
    async fn page(&self) -> ConflictPage {
        self.lease
            .conflict_page(None, 0, 256, ControlAuthority::Permissive)
            .await
            .unwrap()
    }
    async fn resolve(
        &self,
        captured: &CapturedConflict,
        action: Action,
        operation: &str,
    ) -> Result<ResolveOutcome, Error> {
        self.lease
            .resolve_conflict(
                ResolveRequest {
                    operation_id: operation.into(),
                    action,
                    target: ResolveTarget::Conflict {
                        conflict_id: captured.id.clone(),
                        revision: captured.revision.clone(),
                        expected_generation: Some(self.winner.generation),
                    },
                },
                ControlAuthority::Permissive,
                4096,
            )
            .await
    }
    async fn api_bytes(&self) -> Vec<u8> {
        let (_, reader) = self
            .catalog
            .admin(Origin::Api)
            .open_object(&self.bucket, &ObjectName::new("object.txt").unwrap(), None)
            .await
            .unwrap();
        let mut bytes = Vec::new();
        reader
            .into_file()
            .await
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }
    async fn close(self) {
        drop(self.lease);
        self.catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn conflict_capture_owns_binary_bytes_and_discard_is_exact_and_idempotent() {
    let fixture = Fixture::new().await;
    let captured = fixture.capture().await;
    let page = fixture.page().await;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.usage.retained_bytes, 10);
    assert_eq!(page.usage.entries, 1);
    let token = captured.clone();
    let binding = fixture.request().await.binding;
    let expected = Some(Generation::new(fixture.winner.generation).unwrap());
    let root = fixture.lease.status().root_id.as_bytes();
    fixture
        .lease
        .conflict_db(move |connection| {
            let tx = connection.transaction()?;
            assert!(matches!(
                token.validate(
                    &tx,
                    [0; 16],
                    &binding,
                    &ObjectName::new("object.txt")?,
                    expected
                ),
                Err(ObjectError::PreconditionFailed)
            ));
            let proof = token.validate(
                &tx,
                root,
                &binding,
                &ObjectName::new("object.txt")?,
                expected,
            )?;
            assert_eq!(proof.fingerprint.size, 10);
            assert_ne!(proof.device, 0);
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        fs::read(fixture.data.0.join("sync-conflicts").join(captured.id())).unwrap(),
        b"external\0\xff"
    );
    fs::write(fixture.tree.0.join("external"), b"later external edit").unwrap();
    let mut stale = captured.clone();
    stale.revision = "00000000000000000000000000000000".into();
    assert!(matches!(
        fixture.resolve(&stale, Action::Discard, "stale").await,
        Err(Error::RevisionMismatch)
    ));
    assert_eq!(fixture.page().await.usage.entries, 1);
    let outcome = fixture
        .resolve(&captured, Action::Discard, "discard")
        .await
        .unwrap();
    assert!(outcome.completed);
    assert!(
        fixture
            .resolve(&captured, Action::Discard, "discard")
            .await
            .unwrap()
            .completed
    );
    assert!(matches!(
        fixture.resolve(&captured, Action::Restore, "discard").await,
        Err(Error::OperationIdReused)
    ));
    assert_eq!(fixture.page().await.usage.entries, 0);
    assert_eq!(fixture.api_bytes().await, b"api winner");
    assert_eq!(
        fs::read(fixture.tree.0.join("external")).unwrap(),
        b"later external edit"
    );
    fixture.close().await;
}

#[tokio::test]
async fn conflict_restore_uses_shared_publication_and_replays_one_generation() {
    let fixture = Fixture::new().await;
    let (_, old_reader) = fixture
        .catalog
        .admin(Origin::Api)
        .open_object(
            &fixture.bucket,
            &ObjectName::new("object.txt").unwrap(),
            None,
        )
        .await
        .unwrap();
    let captured = fixture.capture().await;
    let outcome = fixture
        .resolve(&captured, Action::Restore, "restore")
        .await
        .unwrap();
    assert!(outcome.result_generation.unwrap() > fixture.winner.generation);
    assert_eq!(
        fixture
            .resolve(&captured, Action::Restore, "restore")
            .await
            .unwrap()
            .result_generation,
        outcome.result_generation
    );
    assert_eq!(fixture.api_bytes().await, b"external\0\xff");
    let record = fixture
        .catalog
        .admin(Origin::Api)
        .get_object(
            &fixture.bucket,
            &ObjectName::new("object.txt").unwrap(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(record.content_type, "text/custom");
    assert_eq!(fixture.page().await.usage.entries, 0);
    let mut old_bytes = Vec::new();
    old_reader
        .into_file()
        .await
        .unwrap()
        .read_to_end(&mut old_bytes)
        .unwrap();
    assert_eq!(old_bytes, b"api winner");
    fixture.close().await;
}

#[tokio::test]
async fn conflict_path_capture_rejects_symlink_redirects_and_owns_the_descriptor() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new().await;
    let directory = fixture.tree.0.join("test-bucket");
    fs::create_dir(&directory).unwrap();
    let target = directory.join("object.txt");
    symlink(fixture.tree.0.join("external"), &target).unwrap();
    assert!(
        fixture
            .lease
            .capture_path(fixture.request().await)
            .await
            .is_err()
    );
    assert_eq!(fixture.page().await.usage.entries, 0);
    fs::remove_file(&target).unwrap();
    fs::write(&target, b"path bytes").unwrap();
    let captured = fixture
        .lease
        .capture_path(fixture.request().await)
        .await
        .unwrap();
    assert_eq!(
        fs::read(fixture.data.0.join("sync-conflicts").join(captured.id())).unwrap(),
        b"path bytes"
    );
    fs::remove_file(&target).unwrap();
    assert_eq!(fixture.page().await.usage.entries, 1);
    fixture.close().await;
}

#[tokio::test]
async fn conflict_restore_stale_generation_and_holds_preserve_quota_and_bytes() {
    let fixture = Fixture::new().await;
    let captured = fixture.capture().await;
    fixture
        .catalog
        .update_object_metadata(
            "test-bucket",
            "object.txt",
            None,
            Default::default(),
            crate::object::ObjectMetadataUpdate {
                temporary_hold: Some(true),
                ..Default::default()
            },
            |_| Ok(()),
        )
        .await
        .unwrap();
    assert!(matches!(
        fixture.resolve(&captured, Action::Restore, "held").await,
        Err(Error::Admin(AdminError::Hold))
    ));
    assert_eq!(fixture.page().await.usage.entries, 1);
    assert_eq!(fixture.api_bytes().await, b"api winner");
    let stale = ResolveRequest {
        operation_id: "stale-generation".into(),
        action: Action::Restore,
        target: ResolveTarget::Conflict {
            conflict_id: captured.id.clone(),
            revision: captured.revision.clone(),
            expected_generation: Some(0),
        },
    };
    assert!(matches!(
        fixture
            .lease
            .resolve_conflict(stale, ControlAuthority::Permissive, 4096)
            .await,
        Err(Error::TargetChanged)
    ));
    assert!(
        fixture
            .resolve(&captured, Action::Discard, "discard-held-body")
            .await
            .unwrap()
            .completed
    );
    assert!(matches!(
        fixture.resolve(&captured, Action::Restore, "held").await,
        Err(Error::TargetChanged)
    ));
    fixture.close().await;
}

fn strict_authority(projects: &[&str]) -> ControlAuthority {
    let roles = projects
        .iter()
        .map(|project| {
            (
                project.to_string(),
                serde_json::json!(["roles/storage.admin"]),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let bytes=serde_json::to_vec(&serde_json::json!({"identities":[{"id":"operator","tokens":["token"],"project_roles":roles}]})).unwrap();
    let auth = crate::auth::Auth::strict(crate::auth::Registry::parse(&bytes).unwrap());
    let mut headers = hyper::HeaderMap::new();
    headers.insert(
        hyper::header::AUTHORIZATION,
        "Bearer token".parse().unwrap(),
    );
    ControlAuthority::Strict {
        seed: "seed".into(),
        identity: auth
            .authenticate(&headers, None)
            .unwrap()
            .unwrap()
            .principal()
            .unwrap()
            .clone(),
    }
}

#[tokio::test]
async fn conflict_original_owner_survives_bucket_recreation_and_completed_retry() {
    let fixture = Fixture::new().await;
    let captured = fixture.capture().await;
    let denied = strict_authority(&["seed", "new-owner"]);
    let authorized = strict_authority(&["seed", "owner", "new-owner"]);
    assert!(matches!(
        fixture
            .lease
            .conflict_page(None, 0, 256, denied.clone())
            .await,
        Err(Error::Admin(AdminError::Forbidden))
    ));
    fixture
        .catalog
        .admin(Origin::Api)
        .delete_object(
            &fixture.bucket,
            &ObjectName::new("object.txt").unwrap(),
            crate::storage::admin::GenerationCondition::Is(
                Generation::new(fixture.winner.generation).unwrap(),
            ),
        )
        .await
        .unwrap();
    fixture
        .catalog
        .delete_bucket("test-bucket", None, None)
        .await
        .unwrap();
    fixture
        .catalog
        .admin(Origin::Api)
        .ensure_bucket(
            BucketDefinition::default_for(&fixture.bucket, "new-owner", &SystemClock).unwrap(),
        )
        .await
        .unwrap();
    let request = ResolveRequest {
        operation_id: "original-owner".into(),
        action: Action::Restore,
        target: ResolveTarget::Conflict {
            conflict_id: captured.id.clone(),
            revision: captured.revision.clone(),
            expected_generation: Some(0),
        },
    };
    assert!(matches!(
        fixture
            .lease
            .resolve_conflict(request.clone(), denied.clone(), 4096)
            .await,
        Err(Error::Admin(AdminError::Forbidden))
    ));
    assert!(matches!(
        fixture
            .lease
            .resolve_conflict(request.clone(), authorized.clone(), 4096)
            .await,
        Err(Error::Admin(AdminError::Precondition))
    ));
    let discard = ResolveRequest {
        operation_id: "authorized-discard".into(),
        action: Action::Discard,
        ..request
    };
    assert!(
        fixture
            .lease
            .resolve_conflict(discard.clone(), authorized, 4096)
            .await
            .unwrap()
            .completed
    );
    assert!(
        fixture
            .lease
            .conflict_page(None, 0, 256, denied.clone())
            .await
            .unwrap()
            .items
            .is_empty()
    );
    assert!(matches!(
        fixture.lease.resolve_conflict(discard, denied, 4096).await,
        Err(Error::Admin(AdminError::Forbidden))
    ));
    assert!(
        !fixture
            .data
            .0
            .join("sync-conflicts")
            .join(captured.id())
            .try_exists()
            .unwrap()
    );
    fixture.close().await;
}

#[tokio::test]
async fn conflict_workspace_copy_charges_both_bodies_and_refuses_overflow() {
    let fixture = Fixture::new().await;
    let (writeback, id) = protected_workspace(&fixture, 16).await;
    let mut request = fixture.request().await;
    request.source_op = Some(id.clone());
    fixture
        .lease
        .capture_conflict(
            request,
            File::open(
                fixture
                    .tree
                    .0
                    .join(crate::sync::mapping::WORKSPACE)
                    .join(&id),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let usage = fixture.page().await.usage;
    assert_eq!(usage.retained_bytes, 32);
    assert_eq!(usage.entries, 2);
    let root = fixture.lease.status().root_id.as_bytes();
    fixture
        .lease
        .conflict_db(move |connection| {
            connection
                .prepare_cached("UPDATE sync_writeback_roots SET retained=?2 WHERE root_id=?1")?
                .execute(params![root, u128::from(MAX_BYTES).to_be_bytes()])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        fixture
            .lease
            .capture_conflict(
                fixture.request().await,
                File::open(fixture.tree.0.join("external")).unwrap()
            )
            .await,
        Err(Error::Capacity)
    ));
    assert!(
        fixture
            .tree
            .0
            .join(crate::sync::mapping::WORKSPACE)
            .join(id)
            .exists()
    );
    drop(writeback);
    fixture.close().await;
}

#[tokio::test]
async fn conflict_capturing_copy_without_registration_has_an_explicit_discard_remedy() {
    let fixture = Fixture::new().await;
    fixture.catalog.inject(
        crate::storage::catalog::fault::Step::ConflictRegister,
        0,
        crate::storage::catalog::fault::Fault::Io(io::ErrorKind::StorageFull),
    );
    assert!(
        fixture
            .lease
            .capture_conflict(
                fixture.request().await,
                File::open(fixture.tree.0.join("external")).unwrap()
            )
            .await
            .is_err()
    );
    let page = fixture.page().await;
    assert_eq!(page.items[0].phase, "capturing");
    let request = ResolveRequest {
        operation_id: "partial-discard".into(),
        action: Action::Discard,
        target: ResolveTarget::Conflict {
            conflict_id: page.items[0].conflict_id.clone(),
            revision: page.items[0].revision.clone(),
            expected_generation: None,
        },
    };
    assert!(
        fixture
            .lease
            .resolve_conflict(request, ControlAuthority::Permissive, 4096)
            .await
            .unwrap()
            .completed
    );
    assert_eq!(fixture.page().await.usage.entries, 0);
    assert_eq!(fixture.api_bytes().await, b"api winner");
    fixture.close().await;
}

#[tokio::test]
async fn conflict_quota_includes_reservations_zero_entries_and_sparse_overflow() {
    let fixture = Fixture::new().await;
    let empty = File::create(fixture.tree.0.join("empty")).unwrap();
    let first = fixture
        .lease
        .capture_conflict(fixture.request().await, empty)
        .await
        .unwrap();
    assert_eq!(fixture.page().await.usage.entries, 1);
    let root = fixture.lease.status().root_id.as_bytes();
    fixture
        .lease
        .conflict_db(move |connection| {
            connection
                .prepare_cached("UPDATE sync_conflicts SET state=0,size=?2 WHERE root_id=?1")?
                .execute(params![root, MAX_BYTES as i64])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        fixture
            .lease
            .capture_conflict(
                fixture.request().await,
                File::open(fixture.tree.0.join("external")).unwrap()
            )
            .await,
        Err(Error::Capacity)
    ));
    assert_eq!(
        fixture.page().await.usage.reserved_bytes,
        u128::from(MAX_BYTES)
    );
    let oversized = File::create(fixture.tree.0.join("oversized")).unwrap();
    oversized.set_len(MAX_BYTES + 1).unwrap();
    assert!(matches!(
        fixture
            .lease
            .capture_conflict(fixture.request().await, oversized)
            .await,
        Err(Error::Capacity)
    ));
    assert!(
        fixture
            .data
            .0
            .join("sync-conflicts")
            .join(first.id())
            .exists()
    );
    assert_eq!(fixture.api_bytes().await, b"api winner");
    fixture.close().await;
}

#[tokio::test]
async fn conflict_pages_reject_changed_revision_and_bound_serialized_size() {
    let fixture = Fixture::new().await;
    fixture.capture().await;
    let page = fixture.page().await;
    fixture.capture().await;
    assert!(matches!(
        fixture
            .lease
            .conflict_page(Some(page.revision), 0, 256, ControlAuthority::Permissive)
            .await,
        Err(Error::RevisionMismatch)
    ));
    assert!(serde_json::to_vec(&fixture.page().await).unwrap().len() < 256 * 1024);
    assert!(
        fixture
            .lease
            .conflict_page(None, 0, 257, ControlAuthority::Permissive)
            .await
            .is_err()
    );
    fixture.close().await;
}

async fn protected_workspace(fixture: &Fixture, size: u64) -> (writeback::Writeback, String) {
    fixture.catalog.enable_change_journal().await.unwrap();
    let writeback = fixture
        .lease
        .writeback(writeback::Limits::new(MAX_BYTES, MAX_ENTRIES).unwrap())
        .await
        .unwrap();
    let fence = fixture
        .catalog
        .read_changes(None, None, 1)
        .await
        .unwrap()
        .fence;
    let pending = writeback
        .prepare(
            &fixture.bucket,
            &ObjectName::new("object.txt").unwrap(),
            Generation::new(fixture.winner.generation).unwrap(),
            writeback::Kind::Publish,
            fence,
        )
        .await
        .unwrap();
    let pending = writeback.arm(pending.token).await.unwrap();
    let id = pending.workspace_entry.name().to_owned();
    let name = id.clone();
    writeback
        .file_job(pending.token.clone(), move |_, workspace| {
            let file = fsutil::create_regular_at(workspace, name.as_ref())?;
            file.set_len(size)?;
            Ok(())
        })
        .await
        .unwrap();
    writeback.inspect(pending.token).await.unwrap();
    (writeback, id)
}

#[tokio::test]
async fn recovery_sparse_over_limit_requires_exact_identity_and_confirmation() {
    let fixture = Fixture::new().await;
    let (writeback, id) = protected_workspace(&fixture, (1 << 40) + 1).await;
    let recovery = fixture
        .lease
        .recovery_page(0, 128, ControlAuthority::Permissive)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(recovery.retained_size, (1 << 40) + 1);
    assert!(recovery.over_limit);
    assert!(recovery.hashes.is_none());
    let mut request = ResolveRequest {
        operation_id: "recovery".into(),
        action: Action::Discard,
        target: ResolveTarget::Recovery {
            recovery_id: id.clone(),
            revision: recovery.recovery_revision.clone(),
            target_identity: recovery.target_identity.clone(),
            confirmation: None,
        },
    };
    assert!(matches!(
        fixture
            .lease
            .resolve_conflict(request.clone(), ControlAuthority::Permissive, 4096)
            .await,
        Err(Error::ConfirmationRequired)
    ));
    if let ResolveTarget::Recovery {
        confirmation,
        target_identity,
        ..
    } = &mut request.target
    {
        *confirmation = Some(Confirmation::DiscardWithoutBackup);
        *target_identity = "00000000000000000000000000000000".into();
    }
    assert!(matches!(
        fixture
            .lease
            .resolve_conflict(request.clone(), ControlAuthority::Permissive, 4096)
            .await,
        Err(Error::TargetChanged)
    ));
    assert_eq!(
        fs::metadata(
            fixture
                .tree
                .0
                .join(crate::sync::mapping::WORKSPACE)
                .join(&id)
        )
        .unwrap()
        .len(),
        (1 << 40) + 1
    );
    if let ResolveTarget::Recovery {
        target_identity, ..
    } = &mut request.target
    {
        *target_identity = recovery.target_identity;
    }
    assert!(
        fixture
            .lease
            .resolve_conflict(request.clone(), ControlAuthority::Permissive, 4096)
            .await
            .unwrap()
            .completed
    );
    assert!(
        fixture
            .lease
            .resolve_conflict(request, ControlAuthority::Permissive, 4096)
            .await
            .unwrap()
            .completed
    );
    assert_eq!(writeback.status().await.unwrap().entries, 0);
    assert_eq!(fixture.api_bytes().await, b"api winner");
    drop(writeback);
    fixture.close().await;
}
