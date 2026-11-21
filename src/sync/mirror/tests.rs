#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::*;
use crate::storage::sync_state::conflicts::ConflictTarget;
use crate::storage::sync_state::findings::FindingKind;
use crate::storage::test_fixtures::{PERMISSIVE, live};
use crate::sync::export::{
    reconcile::{external_files, reconcile},
    restore::{Restored, restore_from_catalog},
    retry::{EXPORT_BACKOFF, EXPORT_RETRIES, retryable, with_retry},
};

use crate::sync::test_support::{Retrack, hooked_worker, retrack};
use crate::{
    clock::SystemClock,
    object::{Patch, UploadChunk},
    storage::{
        admin::{BucketDefinition, GenerationCondition, Publication, PublishMode},
        fault,
    },
    test_support::{HANG_GUARD, TestDir, eventually_async},
};
use crate::{
    fsutil,
    object::ObjectRecord,
    storage::{
        admin::Generation,
        change_journal::JournalFence,
        sync_state::writeback::{self, LiveResult, LiveSource, Stop},
    },
    sync::{
        EntryReportSender,
        mapping::{self, Reason},
    },
};
use bytes::Bytes;
use std::fs;
use std::fs::File;

mod acknowledgement;
mod alias;
mod baseline;
mod decision_race;
mod empty_directory;
mod ingest_durability;
mod install_faults;
mod install_race;
mod missing_bucket;
mod reevaluation;
mod rejection;
mod root_files;
mod several_roots;
mod sparse_displacement;
pub(super) mod state;
mod wait;

async fn run_live(
    catalog: &Catalog,
    root: &RootLease,
    stopping: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<(), SyncError> {
    let (_sender, mut receiver) = tokio::sync::mpsc::channel(1);
    run_live_controlled(catalog, root, stopping, &mut receiver).await
}

/// Runs the live mirror like `run_live`, answering the controls that `requests` delivers.
async fn run_live_controlled(
    catalog: &Catalog,
    root: &RootLease,
    stopping: &mut tokio::sync::watch::Receiver<bool>,
    requests: &mut tokio::sync::mpsc::Receiver<Command>,
) -> Result<(), SyncError> {
    let status = Lifecycle::default();
    let config = Config::default();
    run_live_config(
        LiveRun {
            catalog,
            root,
            config: &config,
            status: &status,
            resume: &mut None,
        },
        stopping,
        requests,
        &mut observation::Controls::default(),
    )
    .await
}

async fn publish(catalog: &Catalog, bucket: &BucketName, body: &[u8]) -> ObjectRecord {
    publish_origin(catalog, bucket, body, Origin::SyncIngest).await
}

async fn publish_origin(
    catalog: &Catalog,
    bucket: &BucketName,
    body: &[u8],
    origin: Origin,
) -> ObjectRecord {
    publish_named(catalog, bucket, "object", body, origin).await
}

async fn publish_named(
    catalog: &Catalog,
    bucket: &BucketName,
    name: &str,
    body: &[u8],
    origin: Origin,
) -> ObjectRecord {
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    tx.try_send(Ok(UploadChunk::Data(Bytes::copy_from_slice(body))))
        .unwrap();
    tx.try_send(Ok(UploadChunk::End)).unwrap();
    catalog
        .admin(origin)
        .publish_object(
            bucket,
            &ObjectName::new(name).unwrap(),
            Publication {
                content_type: "application/octet-stream".into(),
                metadata: Default::default(),
                expected: Default::default(),
                condition: None,
                max_bytes: 4096,
                mode: PublishMode::Replace,
            },
            rx,
        )
        .await
        .unwrap()
}

async fn live_fixture() -> (TestDir, TestDir, Catalog, BucketName, RootLease) {
    live_fixture_with("test-bucket").await
}

async fn live_fixture_with(name: &str) -> (TestDir, TestDir, Catalog, BucketName, RootLease) {
    let bucket = BucketName::new(name).unwrap();
    let (data, tree, catalog, root, _) = crate::sync::test_support::fixture(Some(&bucket)).await;
    (data, tree, catalog, bucket, root)
}

#[tokio::test]
async fn external_inventory_defers_unreadable_children_without_stopping_other_files() {
    use std::os::unix::fs::PermissionsExt;
    let (_data, tree, catalog, _bucket, root) = live_fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    let blocked = tree.0.join("test-bucket/blocked");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("hidden"), b"hidden").unwrap();
    fs::write(tree.0.join("test-bucket/working"), b"working").unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let mut counts = Reconciliation::default();
    let counted = external_files(
        &root,
        &root.sync_admin(),
        Interrupt::new(&stopping),
        &mut counts,
    )
    .await;
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    counted.unwrap();
    assert_eq!(
        (counts.external_only, counts.ignored, counts.unreadable()),
        (1, 0, 1)
    );
    assert_eq!(
        counts.entries().entries(),
        [crate::sync::entries::UnrepresentableEntry {
            path: "test-bucket/blocked".into(),
            problem: crate::sync::entries::EntryProblem::Unreadable,
        }]
    );
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn quiet_native_observation_keeps_split_bulk_deletions_pending_and_answers_controls() {
    let (_data, tree, catalog, _bucket, root) = live_fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    for index in 0..100 {
        fs::write(tree.0.join(format!("test-bucket/{index:03}")), b"body").unwrap();
    }
    let (stop, mut stopping) = tokio::sync::watch::channel(false);
    let (commands, mut requests) = tokio::sync::mpsc::channel(2);
    let worker_root = root.clone();
    let worker_catalog = catalog.clone();
    let hook = std::sync::Arc::new(Hook {
        phase: "idle,observation-invalidated",
        entered: Default::default(),
        release: Default::default(),
    });
    let worker = tokio::spawn(EXPORT_HOOK.scope(hook.clone(), async move {
        let mut controls = observation::Controls::default();
        let result = run_live_config(
            LiveRun {
                catalog: &worker_catalog,
                root: &worker_root,
                config: &Config::default(),
                status: &Lifecycle::default(),
                resume: &mut None,
            },
            &mut stopping,
            &mut requests,
            &mut controls,
        )
        .await;
        controls.stop();
        result
    }));
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
    for index in 0..12 {
        fs::remove_file(tree.0.join(format!("test-bucket/{index:03}"))).unwrap();
    }
    hook.release.notify_one();
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
    let observed = tokio::time::timeout(Duration::from_millis(500), root.observation())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed.tracked_objects, 100);
    let (reply, mut waiting) = tokio::sync::oneshot::channel();
    commands
        .send(Command::Wait {
            deadline: tokio::time::Instant::now() + Duration::from_secs(30),
            settling: Default::default(),
            reply,
            identity: None,
        })
        .await
        .unwrap();
    let (reply, approved) = tokio::sync::oneshot::channel();
    commands
        .send(Command::Decide {
            decision: crate::sync::Decision::Approve,
            revision: "00000000000000000000000000000000".parse().unwrap(),
            reply,
            identity: None,
        })
        .await
        .unwrap();
    hook.release.notify_one();
    let error = tokio::time::timeout(Duration::from_millis(500), approved)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(crate::error::causes(&error).any(|cause| matches!(
        cause.downcast_ref::<AdminError>(),
        Some(AdminError::Precondition)
    )));
    assert!(matches!(
        waiting.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
    assert_eq!(root.observation().await.unwrap().tracked_objects, 100);
    assert_eq!(root.automatic_deletion_count().await.unwrap(), 0);
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
    hook.release.notify_one();
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
    let set = root.deletion_set().await.unwrap().unwrap();
    assert!(set.guarded);
    assert_eq!(set.candidate_count, 100);
    assert_eq!(root.observation().await.unwrap().tracked_objects, 100);
    assert_eq!(root.automatic_deletion_count().await.unwrap(), 0);
    stop.send(true).unwrap();
    hook.release.notify_one();
    worker.await.unwrap().unwrap();
    assert!(matches!(
        waiting.await.unwrap().unwrap().reason,
        crate::sync::WaitReason::Stopped | crate::sync::WaitReason::DeletionApproval
    ));
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn delayed_native_event_retains_the_completed_deletion_observation_until_its_scan_finishes() {
    let (_data, tree, catalog, _bucket, root) = live_fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    for index in 0..100 {
        fs::write(tree.0.join(format!("test-bucket/{index:03}")), b"body").unwrap();
    }
    let (stop, mut stopping) = tokio::sync::watch::channel(false);
    let worker_root = root.clone();
    let worker_catalog = catalog.clone();
    let hook = std::sync::Arc::new(Hook {
        phase: "observation-ready,observation-invalidated",
        entered: Default::default(),
        release: Default::default(),
    });
    let worker = tokio::spawn(EXPORT_HOOK.scope(hook.clone(), async move {
        run_live(&worker_catalog, &worker_root, &mut stopping).await
    }));
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
    fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
    let writeback = root.default_writeback().await.unwrap();
    let mut baseline = BaselineProgress {
        gate: BaselineGate::Allowed,
        complete: true,
    };
    ingest_external(
        LiveScope {
            catalog: &catalog,
            root: &root,
            config: &Config::default(),
            writeback: &writeback,
            stopping: &stop.subscribe(),
            entries: &EntryReportSender::default(),
        },
        &mut baseline,
        &mut SkippedExports::default(),
    )
    .await
    .unwrap();
    let before = root.deletion_set().await.unwrap().unwrap();
    assert!(before.guarded);
    assert_eq!(before.candidate_count, 100);

    // The native deletion event was queued while the explicit scan completed.
    hook.release.notify_one();
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
    assert_eq!(
        root.deletion_set().await.unwrap().unwrap().revision,
        before.revision
    );
    let (summary, during) = root
        .published_observation_for(
            crate::storage::sync_state::authority::ControlAuthority::Permissive,
        )
        .await
        .unwrap();
    assert!(summary.baseline_complete);
    let during = during.unwrap();
    assert_eq!(during.revision, before.revision);
    assert_eq!(during.candidate_count, 100);
    stop.send(true).unwrap();
    hook.release.notify_one();
    worker.await.unwrap().unwrap();
    drop(writeback);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn api_priority_preserves_external_conflicts_and_keeps_protected_api_metadata_unchanged() {
    for (held, tracked) in [(false, false), (false, true), (true, false), (true, true)] {
        let (data, tree, catalog, bucket, root) = live_fixture().await;
        publish_origin(&catalog, &bucket, b"old API", Origin::Api).await;
        if tracked {
            assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
        } else {
            fs::create_dir(tree.0.join("test-bucket")).unwrap();
        }
        fs::write(tree.0.join("test-bucket/object"), b"external saved body").unwrap();
        publish_origin(&catalog, &bucket, b"API winner", Origin::Api).await;
        if held {
            catalog
                .update_object_metadata(
                    live(bucket.as_str(), "object"),
                    Default::default(),
                    crate::object::ObjectMetadataUpdate {
                        temporary_hold: Some(true),
                        ..Default::default()
                    },
                    |_| Ok(()),
                    PERMISSIVE,
                )
                .await
                .unwrap();
        }
        let name = ObjectName::new("object").unwrap();
        let incarnation = catalog
            .admin(Origin::Api)
            .list_all_buckets(None, 1)
            .await
            .unwrap()
            .items[0]
            .incarnation;
        let winner = catalog
            .admin(Origin::Api)
            .get_object(&bucket, &name, None)
            .await
            .unwrap();
        root.record_export_conflict(
            root.sync_binding(&bucket).await.unwrap(),
            name.clone(),
            Generation::new(winner.generation).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(root.observation().await.unwrap().mapping_findings, 1);
        let summary = reconcile(&catalog, &root).await.unwrap();
        assert_eq!(summary.conflicting, 1);
        assert_eq!(summary.exported, 1);
        assert_eq!(root.observation().await.unwrap().mapping_findings, 0);
        assert_eq!(
            fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"API winner"
        );
        assert_eq!(
            catalog
                .admin(Origin::Api)
                .get_object(&bucket, &name, None)
                .await
                .unwrap(),
            winner
        );
        let page = root
            .conflict_page(
                crate::storage::sync_state::conflicts::ConflictPosition::Start,
                crate::storage::sync_state::PageLimit::LARGEST,
                crate::storage::sync_state::authority::ControlAuthority::Permissive,
            )
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].owner_project, "owner");
        assert_eq!(page.items[0].generation, Some(winner.generation));
        assert_eq!(page.items[0].bucket, bucket.as_str());
        assert_eq!(page.items[0].bucket_incarnation, incarnation);
        assert_eq!(page.usage.entries, 1);
        assert_eq!(
            fs::read(
                data.0
                    .join("sync-conflicts")
                    .join(page.items[0].conflict_id.to_string())
            )
            .unwrap(),
            b"external saved body"
        );
        assert_eq!(reconcile(&catalog, &root).await.unwrap().identical, 1);
        assert_eq!(
            root.conflict_page(
                crate::storage::sync_state::conflicts::ConflictPosition::Start,
                crate::storage::sync_state::PageLimit::LARGEST,
                crate::storage::sync_state::authority::ControlAuthority::Permissive
            )
            .await
            .unwrap()
            .items
            .len(),
            1
        );
        root.resolve_conflict(
            crate::storage::sync_state::conflicts::ResolveRequest {
                operation_id: "discard-preserved-revision".into(),
                action: crate::storage::sync_state::conflicts::Action::Discard,
                target: crate::storage::sync_state::conflicts::ResolveTarget::Conflict(
                    ConflictTarget {
                        conflict_id: page.items[0].conflict_id,
                        revision: page.items[0].revision,
                        expected_generation: winner.generation,
                    },
                ),
            },
            crate::storage::sync_state::authority::ControlAuthority::Permissive,
            4096,
        )
        .await
        .unwrap();
        assert_eq!(
            catalog
                .admin(Origin::Api)
                .get_object(&bucket, &name, None)
                .await
                .unwrap(),
            winner
        );
        assert_eq!(root.observation().await.unwrap().unresolved_conflicts, 0);
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn api_deletion_preserves_divergent_external_bytes_for_an_explicit_restore() {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    let original = publish_origin(&catalog, &bucket, b"old API", Origin::Api).await;
    reconcile(&catalog, &root).await.unwrap();
    fs::write(
        tree.0.join("test-bucket/object"),
        b"retained external bytes",
    )
    .unwrap();
    let name = ObjectName::new("object").unwrap();
    root.record_export_conflict(
        root.sync_binding(&bucket).await.unwrap(),
        name.clone(),
        Generation::new(original.generation).unwrap(),
    )
    .await
    .unwrap();
    catalog
        .admin(Origin::Api)
        .delete_object(
            &bucket,
            &name,
            GenerationCondition::Is(Generation::new(original.generation).unwrap()),
        )
        .await
        .unwrap();
    let summary = reconcile(&catalog, &root).await.unwrap();
    assert_eq!(summary.conflicting, 1);
    assert_eq!(root.observation().await.unwrap().mapping_findings, 0);
    assert!(!tree.0.join("test-bucket/object").exists());
    assert!(matches!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket, &name, None)
            .await,
        Err(AdminError::NotFound)
    ));
    let page = root
        .conflict_page(
            crate::storage::sync_state::conflicts::ConflictPosition::Start,
            crate::storage::sync_state::PageLimit::LARGEST,
            crate::storage::sync_state::authority::ControlAuthority::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(
        fs::read(
            data.0
                .join("sync-conflicts")
                .join(page.items[0].conflict_id.to_string())
        )
        .unwrap(),
        b"retained external bytes"
    );
    let restored = root
        .resolve_conflict(
            crate::storage::sync_state::conflicts::ResolveRequest {
                operation_id: "restore-after-api-deletion".into(),
                action: crate::storage::sync_state::conflicts::Action::Restore,
                target: crate::storage::sync_state::conflicts::ResolveTarget::Conflict(
                    ConflictTarget {
                        conflict_id: page.items[0].conflict_id,
                        revision: page.items[0].revision,
                        expected_generation: 0,
                    },
                ),
            },
            crate::storage::sync_state::authority::ControlAuthority::Permissive,
            4096,
        )
        .await
        .unwrap();
    assert!(restored.result_generation.unwrap() > original.generation);
    let record = catalog
        .admin(Origin::Api)
        .get_object(&bucket, &name, None)
        .await
        .unwrap();
    assert_eq!(record.size, b"retained external bytes".len() as u64);
    assert_eq!(record.crc32c, crc32c::crc32c(b"retained external bytes"));
    assert_eq!(root.observation().await.unwrap().unresolved_conflicts, 0);
    drop(root);
    catalog.close().await.unwrap();
}

/// An API write that commits while the initial reconciliation runs leaves the baseline
/// incomplete. API deletions still reach the tree: the unchanged file goes, the locally edited
/// one is kept as a conflict, and the baseline then completes. Before, both deletions waited for
/// a baseline that their own pending removals kept from completing, through rescans and restarts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn api_deletions_reach_the_tree_while_the_initial_baseline_is_incomplete() {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    // An existing object makes the initial reconciliation stop at its export checkpoint.
    publish_named(&catalog, &bucket, "seed", b"seed", Origin::Api).await;
    let (stop, mut stopping) = tokio::sync::watch::channel(false);
    let worker_catalog = catalog.clone();
    let worker_root = root.clone();
    let (hook, worker) = hooked_worker(&EXPORT_HOOK, "before-binding", async move {
        run_live(&worker_catalog, &worker_root, &mut stopping).await
    })
    .await;
    // Committed after the reconciliation took its start fence, so its end fence differs.
    let clean = publish_named(&catalog, &bucket, "clean", b"clean", Origin::Api).await;
    let edited = publish_named(&catalog, &bucket, "edited", b"api", Origin::Api).await;
    let releaser = tokio::spawn(async move {
        loop {
            hook.release.notify_one();
            hook.entered.notified().await;
        }
    });
    let binding = catalog.sync_state().binding(&bucket).await.unwrap();
    for (path, generation) in [
        ("test-bucket/clean", clean.generation),
        ("test-bucket/edited", edited.generation),
    ] {
        eventually_async("the export to record the generation", || async {
            catalog
                .sync_state()
                .get_path(binding.clone(), path.into())
                .await
                .unwrap()
                .is_some_and(|record| record.generation.get() == generation)
        })
        .await;
    }
    assert!(!root.observation().await.unwrap().baseline_complete);
    let edited_path = tree.0.join("test-bucket/edited");
    fs::write(&edited_path, b"local edit").unwrap();
    for (name, record) in [("clean", &clean), ("edited", &edited)] {
        catalog
            .admin(Origin::Api)
            .delete_object(
                &bucket,
                &ObjectName::new(name).unwrap(),
                GenerationCondition::Is(Generation::new(record.generation).unwrap()),
            )
            .await
            .unwrap();
    }
    for path in [tree.0.join("test-bucket/clean"), edited_path] {
        eventually_async("the API deletion to reach the tree", || {
            std::future::ready(!path.exists())
        })
        .await;
    }
    let retained = fs::read_dir(data.0.join("sync-conflicts"))
        .unwrap()
        .map(|entry| fs::read(entry.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(retained, [b"local edit".to_vec()]);
    eventually_async("the baseline to complete", || async {
        root.observation().await.unwrap().baseline_complete
    })
    .await;
    stop.send_replace(true);
    worker.await.unwrap().unwrap();
    releaser.abort();
    drop(root);
    catalog.close().await.unwrap();
}

/// Holds the next conflict capture at `step` until the returned sender releases it:
/// `ConflictReserved` after it reserved its entry and before it copies the file,
/// `ConflictRegistered` after it registered the copy and before the export replaces the file.
fn hold_conflict_capture(
    catalog: &Catalog,
    step: fault::Step,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (entered, held) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    catalog.inject(
        step,
        0,
        fault::Fault::Block {
            entered: std::sync::Arc::new(std::sync::Mutex::new(Some(entered))),
            release: std::sync::Arc::new(std::sync::Mutex::new(released)),
        },
    );
    (held, release)
}

/// The bytes of every retained conflict.
fn retained_conflicts(data: &TestDir) -> Vec<Vec<u8>> {
    fs::read_dir(data.0.join("sync-conflicts"))
        .unwrap()
        .map(|entry| fs::read(entry.unwrap().path()).unwrap())
        .collect()
}

/// Runs the explicit rescan control through `commands`. The mirror answers it between two
/// exports, so an export in progress when it is sent has ended.
async fn rescan_control(commands: &tokio::sync::mpsc::Sender<Command>) -> Reconciliation {
    let (reply, summary) = tokio::sync::oneshot::channel();
    commands
        .send(Command::Rescan {
            reply,
            identity: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(HANG_GUARD, summary)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

/// The generation the tree tracks for `object` in `bucket`, if it tracks one.
async fn tracked_generation(
    catalog: &Catalog,
    root: &RootLease,
    bucket: &BucketName,
) -> Option<i64> {
    let binding = root.sync_binding(bucket).await.unwrap();
    catalog
        .sync_state()
        .get_path(binding, "test-bucket/object".into())
        .await
        .unwrap()
        .map(|record| record.generation.get())
}

/// The bytes of every retained conflict, sorted.
fn sorted_conflicts(data: &TestDir) -> Vec<Vec<u8>> {
    let mut retained = retained_conflicts(data);
    retained.sort();
    retained
}

/// A live mirror past its initial export of `object`, held at a conflict capture `step` while
/// it preserves a local edit of that file before replaying the API write that replaces it.
struct HeldPreservation {
    data: TestDir,
    tree: TestDir,
    catalog: Catalog,
    bucket: BucketName,
    root: RootLease,
    /// The generation the held export brings to the tree.
    api: ObjectRecord,
    stop: tokio::sync::watch::Sender<bool>,
    commands: tokio::sync::mpsc::Sender<Command>,
    worker: tokio::task::JoinHandle<Result<(), SyncError>>,
    releaser: tokio::task::JoinHandle<()>,
    release: std::sync::mpsc::Sender<()>,
}

impl HeldPreservation {
    async fn start(step: fault::Step) -> Self {
        let (data, tree, catalog, bucket, root) = live_fixture().await;
        publish_origin(&catalog, &bucket, b"first", Origin::Api).await;
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let (commands, mut requests) = tokio::sync::mpsc::channel(1);
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        // The first idle follows the initial export, which completed the baseline.
        let (hook, worker) = hooked_worker(&EXPORT_HOOK, "idle", async move {
            run_live_controlled(&worker_catalog, &worker_root, &mut stopping, &mut requests).await
        })
        .await;
        assert!(root.observation().await.unwrap().baseline_complete);
        let path = tree.0.join("test-bucket/object");
        assert_eq!(fs::read(&path).unwrap(), b"first");
        fs::write(&path, b"external edit").unwrap();
        let (held, release) = hold_conflict_capture(&catalog, step);
        let api = publish_origin(&catalog, &bucket, b"second", Origin::Api).await;
        let releaser = tokio::spawn(async move {
            loop {
                hook.release.notify_one();
                hook.entered.notified().await;
            }
        });
        tokio::time::timeout(HANG_GUARD, held)
            .await
            .unwrap()
            .unwrap();
        Self {
            data,
            tree,
            catalog,
            bucket,
            root,
            api,
            stop,
            commands,
            worker,
            releaser,
            release,
        }
    }

    fn path(&self) -> PathBuf {
        self.tree.0.join("test-bucket/object")
    }

    fn release(&self) {
        self.release.send(()).unwrap();
    }

    /// Waits until the tree tracks `api` (`None`: nothing), then checks the file holds `body`.
    async fn await_export(&self, api: Option<&ObjectRecord>, body: Option<&[u8]>) {
        let binding = self.root.sync_binding(&self.bucket).await.unwrap();
        let expected = api.map(|record| record.generation);
        eventually_async("the export to track the API generation", || async {
            self.catalog
                .sync_state()
                .get_path(binding.clone(), "test-bucket/object".into())
                .await
                .unwrap()
                .map(|record| record.generation.get())
                == expected
        })
        .await;
        match body {
            Some(body) => assert_eq!(fs::read(self.path()).unwrap(), body),
            None => assert!(!self.path().exists()),
        }
    }

    async fn finish(self) {
        self.stop.send_replace(true);
        self.worker.await.unwrap().unwrap();
        self.releaser.abort();
        drop(self.root);
        self.catalog.close().await.unwrap();
    }
}

/// An API write that commits while a live export preserves the local edit it replaces leaves
/// that export skipped. The write is a journal change of its own, so its replay preserves the
/// edit once and brings the newer API state to the tree.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_api_write_during_conflict_preservation_is_replayed_from_the_journal() {
    for deleted in [false, true] {
        let held = HeldPreservation::start(fault::Step::ConflictReserved).await;
        let name = ObjectName::new("object").unwrap();
        let newer = if deleted {
            held.catalog
                .admin(Origin::Api)
                .delete_object(
                    &held.bucket,
                    &name,
                    GenerationCondition::Is(Generation::new(held.api.generation).unwrap()),
                )
                .await
                .unwrap();
            None
        } else {
            Some(publish_origin(&held.catalog, &held.bucket, b"third", Origin::Api).await)
        };
        held.release();
        held.await_export(newer.as_ref(), newer.as_ref().map(|_| &b"third"[..]))
            .await;
        assert_eq!(retained_conflicts(&held.data), [b"external edit".to_vec()]);
        held.finish().await;
    }
}

/// A local edit that changes again, or goes away, while a live export preserves it leaves that
/// export skipped after the journal cursor moved past its API write. Once the tree settles, the
/// export runs again: the API generation reaches the tree, and the newer edit is preserved as
/// a conflict. Before, nothing exported it again until a restart.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_live_export_skipped_by_a_changing_edit_runs_again_once_the_tree_settles() {
    for removed in [false, true] {
        let held = HeldPreservation::start(fault::Step::ConflictReserved).await;
        fs::write(held.path(), b"newer external edit").unwrap();
        if removed {
            fs::remove_file(held.path()).unwrap();
        }
        held.release();
        held.await_export(Some(&held.api), Some(b"second")).await;
        let retained: &[&[u8]] = if removed {
            &[]
        } else {
            &[b"newer external edit"]
        };
        assert_eq!(retained_conflicts(&held.data), retained);
        let observed = held.root.observation().await.unwrap();
        assert_eq!(observed.mapping_findings, 0);
        assert_eq!(observed.unresolved_conflicts, retained.len() as u64);
        held.finish().await;
    }
}

/// A local edit that changes again after a live export preserved it makes that export record an
/// export conflict and log the remedy: stop the writer and rescan. The export now runs again
/// after the next ingestion pass, so once the writer stops, the rescan answers with the API
/// generation in the tree and the newer edit preserved next to the first. Before, every later
/// pass, the rescan's included, reported the edit as a conflict and exported nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rescan_exports_an_api_write_whose_preserved_edit_changed_again() {
    let held = HeldPreservation::start(fault::Step::ConflictRegistered).await;
    fs::write(held.path(), b"newer external edit").unwrap();
    held.release();
    assert!(rescan_control(&held.commands).await.baseline_complete);
    assert_eq!(
        tracked_generation(&held.catalog, &held.root, &held.bucket).await,
        Some(held.api.generation)
    );
    assert_eq!(fs::read(held.path()).unwrap(), b"second");
    assert_eq!(
        sorted_conflicts(&held.data),
        [b"external edit".to_vec(), b"newer external edit".to_vec()]
    );
    let observed = held.root.observation().await.unwrap();
    assert!(observed.baseline_complete);
    assert_eq!(observed.mapping_findings, 0);
    assert_eq!(observed.unresolved_conflicts, 2);
    held.finish().await;
}

/// The same newer edit during the initial reconciliation also kept the baseline from
/// completing, since every later pass reported it as a conflict. After the rescan, the API
/// generation is in the tree, and the pass that next finds it there completes the baseline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rescan_lets_the_initial_baseline_complete_after_a_preserved_edit_changed_again() {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    let api = publish_origin(&catalog, &bucket, b"api", Origin::Api).await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    let path = tree.0.join("test-bucket/object");
    fs::write(&path, b"external body").unwrap();
    let (held, release) = hold_conflict_capture(&catalog, fault::Step::ConflictRegistered);
    let (stop, mut stopping) = tokio::sync::watch::channel(false);
    let (commands, mut requests) = tokio::sync::mpsc::channel(1);
    let worker_catalog = catalog.clone();
    let worker_root = root.clone();
    let worker = tokio::spawn(async move {
        run_live_controlled(&worker_catalog, &worker_root, &mut stopping, &mut requests).await
    });
    tokio::time::timeout(HANG_GUARD, held)
        .await
        .unwrap()
        .unwrap();
    fs::write(&path, b"newer external body").unwrap();
    release.send(()).unwrap();
    rescan_control(&commands).await;
    assert_eq!(
        tracked_generation(&catalog, &root, &bucket).await,
        Some(api.generation)
    );
    assert_eq!(fs::read(&path).unwrap(), b"api");
    assert_eq!(
        sorted_conflicts(&data),
        [b"external body".to_vec(), b"newer external body".to_vec()]
    );
    eventually_async("the baseline to complete", || async {
        root.observation().await.unwrap().baseline_complete
    })
    .await;
    assert_eq!(root.observation().await.unwrap().mapping_findings, 0);
    stop.send_replace(true);
    worker.await.unwrap().unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

/// The same skip during the initial reconciliation kept the baseline from completing: the
/// ingestion pass that follows reports the newer edit as a conflict, so it reconciles nothing.
/// The export now runs again after that pass, and the baseline completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_skipped_conflict_preservation_lets_the_initial_baseline_complete() {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    let api = publish_origin(&catalog, &bucket, b"api", Origin::Api).await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    let path = tree.0.join("test-bucket/object");
    fs::write(&path, b"external body").unwrap();
    let (held, release) = hold_conflict_capture(&catalog, fault::Step::ConflictReserved);
    let (stop, mut stopping) = tokio::sync::watch::channel(false);
    let worker_catalog = catalog.clone();
    let worker_root = root.clone();
    let worker =
        tokio::spawn(async move { run_live(&worker_catalog, &worker_root, &mut stopping).await });
    tokio::time::timeout(HANG_GUARD, held)
        .await
        .unwrap()
        .unwrap();
    fs::write(&path, b"newer external body").unwrap();
    release.send(()).unwrap();
    eventually_async("the baseline to complete", || async {
        root.observation().await.unwrap().baseline_complete
    })
    .await;
    let binding = root.sync_binding(&bucket).await.unwrap();
    let tracked = catalog
        .sync_state()
        .get_path(binding, "test-bucket/object".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(tracked.generation.get(), api.generation);
    assert_eq!(fs::read(&path).unwrap(), b"api");
    assert_eq!(retained_conflicts(&data), [b"newer external body".to_vec()]);
    stop.send_replace(true);
    worker.await.unwrap().unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn live_export_retries_identical_observation_at_prepare_arm_durable_and_ack() {
    use std::time::Duration;
    for phase in ["prepare", "arm", "durable", "ack"] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        publish_origin(&catalog, &bucket, b"same", Origin::Api).await;
        let other = BucketName::new("zz-other").unwrap();
        catalog
            .admin(Origin::Api)
            .ensure_bucket(BucketDefinition::default_for(&other, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let (hook, worker) = hooked_worker(&EXPORT_HOOK, "idle", async move {
            run_live(&worker_catalog, &worker_root, &mut stopping).await
        })
        .await;
        crate::storage::sync_state::writeback::inject_live_export_failure(
            root.status().root_id,
            phase,
            1,
        );
        let record = publish_origin(&catalog, &bucket, b"same", Origin::Api).await;
        publish_origin(&catalog, &other, b"other", Origin::Api).await;
        let started = tokio::time::Instant::now();
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "{phase} must retry"
        );
        let writeback = root
            .writeback(Limits::new(4096, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(writeback.status().await.unwrap().stop(), Stop::Ready);
        assert!(writeback.page(0, 256).await.unwrap().is_empty());
        assert_eq!(
            std::fs::read(tree.0.join("zz-other/object")).unwrap(),
            b"other"
        );
        let binding = catalog.sync_state().binding(&bucket).await.unwrap();
        assert_eq!(
            catalog
                .sync_state()
                .get_path(binding, "test-bucket/object".into())
                .await
                .unwrap()
                .unwrap()
                .generation
                .get(),
            record.generation
        );
        crate::storage::sync_state::writeback::inject_live_export_failure(
            root.status().root_id,
            phase,
            1,
        );
        let patched = catalog
            .update_object_metadata(
                live(bucket.as_str(), "object"),
                Default::default(),
                crate::object::ObjectMetadataUpdate {
                    content_type: Patch::Set("text/plain".into()),
                    ..Default::default()
                },
                |_| Ok(()),
                PERMISSIVE,
            )
            .await
            .unwrap();
        let before = std::fs::metadata(tree.0.join("test-bucket/object")).unwrap();
        let started = tokio::time::Instant::now();
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        assert!(
            started.elapsed() >= Duration::from_secs(1),
            "metadata {phase} must retry"
        );
        let binding = catalog.sync_state().binding(&bucket).await.unwrap();
        let tracked = catalog
            .sync_state()
            .get_path(binding, "test-bucket/object".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tracked.generation.get(), patched.generation);
        assert_eq!(
            catalog
                .admin(Origin::Api)
                .get_object(&bucket, &ObjectName::new("object").unwrap(), None)
                .await
                .unwrap(),
            patched
        );
        assert_eq!(
            std::fs::metadata(tree.0.join("test-bucket/object"))
                .unwrap()
                .modified()
                .unwrap(),
            before.modified().unwrap()
        );
        assert_eq!(writeback.status().await.unwrap().stop(), Stop::Ready);
        publish_origin(&catalog, &bucket, b"later", Origin::Api).await;
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"later"
        );
        stop.send_replace(true);
        hook.release.notify_one();
        worker.await.unwrap().unwrap();
        drop(writeback);
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn live_export_identical_observation_preserves_edits_before_durable_and_ack() {
    for durable in [false, true] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        let record = publish_origin(&catalog, &bucket, b"same", Origin::Api).await;
        catalog.enable_change_journal().await.unwrap();
        let fence = catalog.current_fence().await.unwrap();
        let binding = catalog.sync_state().binding(&bucket).await.unwrap();
        let writeback = root
            .writeback(Limits::new(4096, 10).unwrap())
            .await
            .unwrap();
        std::fs::create_dir(tree.0.join("test-bucket")).unwrap();
        let path = tree.0.join("test-bucket/object");
        std::fs::write(&path, b"same").unwrap();
        let operation = writeback
            .begin_live_export(
                binding,
                ObjectName::new("object").unwrap(),
                Generation::new(record.generation).unwrap(),
                None,
                fence,
                LiveSource::Observe,
            )
            .await
            .unwrap();
        assert_eq!(
            writeback
                .advance_live_export(operation.clone())
                .await
                .unwrap(),
            LiveResult::Pending
        );
        if durable {
            assert_eq!(
                writeback
                    .advance_live_export(operation.clone())
                    .await
                    .unwrap(),
                LiveResult::Pending
            );
        }
        std::fs::write(&path, b"external").unwrap();
        assert_eq!(
            writeback.advance_live_export(operation).await.unwrap(),
            LiveResult::Conflict
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"external");
        assert_eq!(writeback.status().await.unwrap().stop(), Stop::Ready);
        assert!(writeback.page(0, 256).await.unwrap().is_empty());
        drop(writeback);
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn live_export_identical_parent_disappearance_cancels_and_keeps_other_buckets_progressing() {
    for moved in [false, true] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        let other = BucketName::new("zz-other").unwrap();
        catalog
            .admin(Origin::Api)
            .ensure_bucket(BucketDefinition::default_for(&other, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let (hook, worker) = hooked_worker(&EXPORT_HOOK, "idle,after-prepare", async move {
            run_live(&worker_catalog, &worker_root, &mut stopping).await
        })
        .await;
        let path = tree.0.join("test-bucket/object");
        std::fs::write(&path, b"same").unwrap();
        publish_origin(&catalog, &bucket, b"same", Origin::Api).await;
        publish_origin(&catalog, &other, b"other", Origin::Api).await;
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        if moved {
            std::fs::rename(tree.0.join("test-bucket"), tree.0.join("moved")).unwrap();
        } else {
            std::fs::remove_dir_all(tree.0.join("test-bucket")).unwrap();
        }
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(tree.0.join("zz-other/object")).unwrap(),
            b"other"
        );
        assert!(!path.exists());
        if moved {
            assert_eq!(std::fs::read(tree.0.join("moved/object")).unwrap(), b"same");
        }
        let writeback = root
            .writeback(Limits::new(4096, 10).unwrap())
            .await
            .unwrap();
        assert_eq!(writeback.status().await.unwrap().stop(), Stop::Ready);
        assert!(writeback.page(0, 256).await.unwrap().is_empty());
        stop.send_replace(true);
        hook.release.notify_one();
        worker.await.unwrap().unwrap();
        drop(writeback);
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn live_export_retries_file_and_parent_permissions_without_advancing_the_cursor() {
    use std::os::unix::fs::PermissionsExt;
    for parent in [false, true] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        publish_origin(&catalog, &bucket, b"before", Origin::Api).await;
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let (hook, worker) = hooked_worker(&EXPORT_HOOK, "idle,retry", async move {
            run_live(&worker_catalog, &worker_root, &mut stopping).await
        })
        .await;
        let path = tree.0.join(if parent {
            "test-bucket"
        } else {
            "test-bucket/object"
        });
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0)).unwrap();
        publish_origin(&catalog, &bucket, b"after", Origin::Api).await;
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        std::fs::set_permissions(
            &path,
            std::fs::Permissions::from_mode(if parent { 0o700 } else { 0o600 }),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"before"
        );
        assert_eq!(
            root.writeback(Limits::new(4096, 10).unwrap())
                .await
                .unwrap()
                .status()
                .await
                .unwrap()
                .protected_entries,
            0
        );
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"after"
        );
        stop.send_replace(true);
        hook.release.notify_one();
        worker.await.unwrap().unwrap();
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn live_export_bucket_incarnation_races_keep_other_buckets_progressing() {
    for phase in ["before-binding", "after-binding", "after-get-path"] {
        for recreate in [false, true] {
            let (_data, tree, catalog, bucket, root) = live_fixture().await;
            let other = BucketName::new("zz-other").unwrap();
            catalog
                .admin(Origin::Api)
                .ensure_bucket(
                    BucketDefinition::default_for(&other, "owner", &SystemClock).unwrap(),
                )
                .await
                .unwrap();
            let (stop, mut stopping) = tokio::sync::watch::channel(false);
            let phases = match phase {
                "before-binding" => "idle,before-binding",
                "after-binding" => "idle,after-binding",
                _ => "idle,after-get-path",
            };
            let worker_catalog = catalog.clone();
            let worker_root = root.clone();
            let (hook, worker) = hooked_worker(&EXPORT_HOOK, phases, async move {
                run_live(&worker_catalog, &worker_root, &mut stopping).await
            })
            .await;
            let record = publish_origin(&catalog, &bucket, b"obsolete", Origin::Api).await;
            publish_origin(&catalog, &other, b"other", Origin::Api).await;
            hook.release.notify_one();
            tokio::time::timeout(HANG_GUARD, hook.entered.notified())
                .await
                .unwrap();
            catalog
                .admin(Origin::Api)
                .delete_object(
                    &bucket,
                    &ObjectName::new("object").unwrap(),
                    GenerationCondition::Is(Generation::new(record.generation).unwrap()),
                )
                .await
                .unwrap();
            catalog
                .delete_bucket(bucket.as_str(), Default::default(), PERMISSIVE)
                .await
                .unwrap();
            if recreate {
                catalog
                    .admin(Origin::Api)
                    .ensure_bucket(
                        BucketDefinition::default_for(&bucket, "new-owner", &SystemClock).unwrap(),
                    )
                    .await
                    .unwrap();
            }
            hook.release.notify_one();
            // The next key belongs to another bucket in the same committed page.
            tokio::time::timeout(HANG_GUARD, hook.entered.notified())
                .await
                .unwrap();
            hook.release.notify_one();
            tokio::time::timeout(HANG_GUARD, hook.entered.notified())
                .await
                .unwrap();
            assert_eq!(
                std::fs::read(tree.0.join("zz-other/object")).unwrap(),
                b"other",
                "{phase} recreate={recreate}"
            );
            stop.send_replace(true);
            hook.release.notify_one();
            worker.await.unwrap().unwrap();
            assert!(!tree.0.join("test-bucket/object").exists());
            drop(root);
            catalog.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn live_export_coalesces_pages_recovers_retention_gaps_and_sleeps_without_polling() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;
    for gap in [false, true] {
        let (data, tree, catalog, bucket, root) = live_fixture().await;
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let count = Arc::new(AtomicUsize::new(0));
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let (hook, worker) = hooked_worker(
            &EXPORT_HOOK,
            "idle",
            LIVE_EXPORTS.scope(count.clone(), async move {
                run_live(&worker_catalog, &worker_root, &mut stopping).await
            }),
        )
        .await;
        for n in 0..300 {
            publish_origin(&catalog, &bucket, n.to_string().as_bytes(), Origin::Api).await;
        }
        if gap {
            let mut connection =
                rusqlite::Connection::open(data.0.join("catalog.sqlite3")).unwrap();
            let tx = connection.transaction().unwrap();
            tx.execute("DELETE FROM change_journal", []).unwrap();
            tx.execute("UPDATE change_journal_state SET floor=sequence", [])
                .unwrap();
            tx.commit().unwrap();
        }
        hook.release.notify_one();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(
            std::fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"299"
        );
        // Native observation can enqueue our export's own event before becoming quiet.
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                hook.release.notify_one();
                if tokio::time::timeout(Duration::from_millis(100), hook.entered.notified())
                    .await
                    .is_err()
                {
                    break;
                }
                assert_eq!(count.load(Ordering::SeqCst), 1);
            }
        })
        .await
        .unwrap();
        stop.send_replace(true);
        hook.release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(root);
        catalog.close().await.unwrap();
    }
}

/// A local edit that ingestion cannot take in stays a candidate of every pass: one that conflicts
/// with a newer API generation no journal replay brings to the tree, or one over an object under
/// a temporary hold. Each pass stages it, and on macOS that staging clones the file, which
/// FSEvents reports on the file itself. That report alone ran the pass again about every two
/// seconds while nothing changed. Now a pass runs again only after the tree changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_edit_ingestion_cannot_take_in_is_staged_again_only_after_the_tree_changes() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    for held in [false, true] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        publish_origin(&catalog, &bucket, b"first", Origin::Api).await;
        if held {
            catalog
                .update_object_metadata(
                    live(bucket.as_str(), "object"),
                    Default::default(),
                    crate::object::ObjectMetadataUpdate {
                        temporary_hold: Some(true),
                        ..Default::default()
                    },
                    |_| Ok(()),
                    PERMISSIVE,
                )
                .await
                .unwrap();
        }
        let stages = Arc::new(AtomicUsize::new(0));
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        // The first idle follows the initial export.
        let (hook, worker) = hooked_worker(
            &EXPORT_HOOK,
            "idle",
            crate::sync::ingest::STAGES.scope(stages.clone(), async move {
                run_live(&worker_catalog, &worker_root, &mut stopping).await
            }),
        )
        .await;
        let path = tree.0.join("test-bucket/object");
        assert_eq!(fs::read(&path).unwrap(), b"first");
        if !held {
            // Ingestion's own origin is never replayed, so the tree keeps tracking the first one.
            publish(&catalog, &bucket, b"second").await;
        }
        fs::write(&path, b"external edit").unwrap();
        let releaser = tokio::spawn(async move {
            loop {
                hook.release.notify_one();
                hook.entered.notified().await;
            }
        });
        eventually_async("a pass to stage the edit", || {
            std::future::ready(stages.load(Ordering::SeqCst) == 1)
        })
        .await;
        // A pass repeated without a change stages the edit again after one quiet interval and
        // its own settle interval.
        let repeated = tokio::time::timeout(
            Duration::from_millis(3 * crate::sync::SETTLE_MS),
            eventually_async("a repeated pass to stage the edit", || {
                std::future::ready(stages.load(Ordering::SeqCst) > 1)
            }),
        )
        .await;
        assert!(repeated.is_err(), "a pass ran again without a change");
        let observed = root.observation().await.unwrap();
        assert_eq!(observed.mapping_findings, u64::from(!held));
        fs::write(&path, b"newer external edit").unwrap();
        eventually_async("a pass to stage the newer edit", || {
            std::future::ready(stages.load(Ordering::SeqCst) == 2)
        })
        .await;
        assert_eq!(fs::read(&path).unwrap(), b"newer external edit");
        stop.send_replace(true);
        worker.await.unwrap().unwrap();
        releaser.abort();
        drop(root);
        catalog.close().await.unwrap();
    }
}

/// The metadata-only API change that meets an edit a hold deferred.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HeldEditChange {
    /// The hold is released.
    Release,
    /// Another field changes while the hold stays.
    PatchWhileHeld,
}

/// A temporary or event-based hold keeps ingestion from publishing a local edit, so the pass
/// defers it and leaves the file as edited. The hold's release is a metadata-only API change,
/// and its export meets the divergent file like any API change: it preserves the edit as a
/// conflict and returns the file to the API bytes, which do not change. A metadata-only change
/// while the hold stays does the same. The edit reaches the API only through an explicit restore
/// of the preserved revision once the hold is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_metadata_change_preserves_an_edit_a_hold_deferred_as_a_conflict() {
    use crate::object::{HoldKind, ObjectMetadataUpdate};
    use crate::storage::sync_state::{
        PageLimit,
        authority::ControlAuthority,
        conflicts::{Action, ConflictPosition, ResolveRequest, ResolveTarget},
        observation::ScanStop,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let hold = |kind: HoldKind, held: bool| match kind {
        HoldKind::Temporary => ObjectMetadataUpdate {
            temporary_hold: Some(held),
            ..Default::default()
        },
        HoldKind::EventBased => ObjectMetadataUpdate {
            event_based_hold: Some(held),
            ..Default::default()
        },
    };
    for (kind, change) in [
        (HoldKind::Temporary, HeldEditChange::Release),
        (HoldKind::EventBased, HeldEditChange::Release),
        (HoldKind::Temporary, HeldEditChange::PatchWhileHeld),
    ] {
        let (data, tree, catalog, bucket, root) = live_fixture().await;
        let name = ObjectName::new("object").unwrap();
        let api = publish_origin(&catalog, &bucket, b"first", Origin::Api).await;
        let update = |update| {
            catalog.update_object_metadata(
                live(bucket.as_str(), "object"),
                Default::default(),
                update,
                |_| Ok(()),
                PERMISSIVE,
            )
        };
        let api_object = || async {
            catalog
                .admin(Origin::Api)
                .get_object(&bucket, &name, None)
                .await
                .unwrap()
        };
        update(hold(kind, true)).await.unwrap();
        let stages = Arc::new(AtomicUsize::new(0));
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        // The first idle follows the initial export.
        let (hook, worker) = hooked_worker(
            &EXPORT_HOOK,
            "idle",
            crate::sync::ingest::STAGES.scope(stages.clone(), async move {
                run_live(&worker_catalog, &worker_root, &mut stopping).await
            }),
        )
        .await;
        // The mirror reaches its idle checkpoint only after the journal page or the ingestion
        // pass it started has finished, so each step ends with that work done.
        let step = || async {
            hook.release.notify_one();
            tokio::time::timeout(HANG_GUARD, hook.entered.notified())
                .await
                .unwrap();
        };
        let path = tree.0.join("test-bucket/object");
        assert_eq!(fs::read(&path).unwrap(), b"first");
        fs::write(&path, b"external edit").unwrap();
        while stages.load(Ordering::SeqCst) == 0 {
            step().await;
        }
        let deferred = stages.load(Ordering::SeqCst);
        let held = api_object().await;
        assert_eq!((held.generation, held.md5), (api.generation, api.md5));
        assert!(held.has_hold());
        assert_eq!(fs::read(&path).unwrap(), b"external edit");
        let observed = root.observation().await.unwrap();
        assert_eq!(observed.scan_stop, Some(ScanStop::SourceChanging));
        assert_eq!(observed.mapping_findings, 0);
        assert_eq!(observed.unresolved_conflicts, 0);

        update(match change {
            HeldEditChange::Release => hold(kind, false),
            HeldEditChange::PatchWhileHeld => ObjectMetadataUpdate {
                content_type: Patch::Set("text/plain".into()),
                ..Default::default()
            },
        })
        .await
        .unwrap();
        step().await;
        assert_eq!(
            stages.load(Ordering::SeqCst),
            deferred,
            "an ingestion pass met the edit before the export"
        );
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert_eq!(retained_conflicts(&data), [b"external edit".to_vec()]);
        // The export's own file change starts the pass that finds the API bytes.
        while root.observation().await.unwrap().scan_stop.is_some() {
            step().await;
        }
        let observed = root.observation().await.unwrap();
        assert!(observed.baseline_complete);
        assert_eq!(observed.mapping_findings, 0);
        assert_eq!(observed.path_stops, 0);
        assert_eq!(observed.unresolved_conflicts, 1);
        let exported = api_object().await;
        assert_eq!(
            (exported.generation, exported.md5),
            (api.generation, api.md5)
        );
        assert_eq!(
            exported.has_hold(),
            change == HeldEditChange::PatchWhileHeld
        );
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert_eq!(
            tracked_generation(&catalog, &root, &bucket).await,
            Some(api.generation)
        );

        if change == HeldEditChange::Release {
            let page = root
                .conflict_page(
                    ConflictPosition::Start,
                    PageLimit::LARGEST,
                    ControlAuthority::Permissive,
                )
                .await
                .unwrap();
            let restored = root
                .resolve_conflict(
                    ResolveRequest {
                        operation_id: "restore-the-released-edit".into(),
                        action: Action::Restore,
                        target: ResolveTarget::Conflict(ConflictTarget {
                            conflict_id: page.items[0].conflict_id,
                            revision: page.items[0].revision,
                            expected_generation: api.generation,
                        }),
                    },
                    ControlAuthority::Permissive,
                    4096,
                )
                .await
                .unwrap();
            let generation = restored.result_generation.unwrap();
            let published = api_object().await;
            assert_eq!(published.generation, generation);
            assert_eq!(published.crc32c, crc32c::crc32c(b"external edit"));
            // The restore is an API write, which a journal page brings back to the tree.
            while fs::read(&path).unwrap() != b"external edit" {
                step().await;
            }
            assert_eq!(
                tracked_generation(&catalog, &root, &bucket).await,
                Some(generation)
            );
            assert_eq!(root.observation().await.unwrap().unresolved_conflicts, 0);
        }
        stop.send_replace(true);
        hook.release.notify_one();
        worker.await.unwrap().unwrap();
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn live_export_retries_write_failures_at_copy_install_sync_and_catalog_ack() {
    use std::time::Duration;
    for (phase, remove) in [
        ("copy", false),
        ("install", false),
        ("installed", false),
        ("ack", false),
        ("install", true),
        ("installed", true),
        ("ack", true),
        ("cancel", false),
    ] {
        let data = TestDir::new();
        let tree = TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let bucket = BucketName::new("test-bucket").unwrap();
        catalog
            .admin(Origin::Api)
            .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        publish_origin(&catalog, &bucket, b"before", Origin::Api).await;
        let config = Config {
            data_dir: data.0.clone(),
            sync_dir: Some(tree.0.clone()),
            ..Default::default()
        };
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let (hook, mut worker) = hooked_worker(&EXPORT_HOOK, "idle", async move {
            run_live(&worker_catalog, &worker_root, &mut stopping).await
        })
        .await;
        crate::storage::sync_state::writeback::inject_live_export_failure(
            root.status().root_id,
            phase,
            1,
        );
        if phase == "cancel" {
            std::fs::write(tree.0.join("test-bucket/object"), b"external").unwrap();
        }
        let record = if remove {
            let record = catalog
                .admin(Origin::Api)
                .get_object(&bucket, &ObjectName::new("object").unwrap(), None)
                .await
                .unwrap();
            catalog
                .admin(Origin::Api)
                .delete_object(
                    &bucket,
                    &ObjectName::new("object").unwrap(),
                    GenerationCondition::Is(Generation::new(record.generation).unwrap()),
                )
                .await
                .unwrap();
            record
        } else {
            publish_origin(&catalog, &bucket, b"after\0\xff", Origin::Api).await
        };
        hook.release.notify_one();
        if phase == "installed" {
            assert!(
                tokio::time::timeout(Duration::from_secs(3), worker)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
            let recovering = root
                .writeback(
                    Limits::new(
                        super::super::CONFLICT_MAX_BYTES,
                        super::super::CONFLICT_MAX_ENTRIES,
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            let usage = recovering.status().await.unwrap();
            assert_eq!(usage.stop(), Stop::IoFailure);
            assert_eq!(usage.protected_entries, 1);
            assert_eq!(usage.retained_bytes, b"before".len() as u128);
            let pending = recovering.page(0, 256).await.unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(
                fs::read(
                    tree.0
                        .join(mapping::WORKSPACE)
                        .join(pending[0].token.id().to_string())
                )
                .unwrap(),
                b"before"
            );
            recovering.recover_pending().await.unwrap();
            assert_eq!(
                recovering.preserve_protected().await.unwrap().stop(),
                Stop::Ready
            );
            let copies = root
                .conflict_page(
                    crate::storage::sync_state::conflicts::ConflictPosition::Start,
                    crate::storage::sync_state::PageLimit::LARGEST,
                    crate::storage::sync_state::authority::ControlAuthority::Permissive,
                )
                .await
                .unwrap();
            assert_eq!(copies.items.len(), 1);
            assert_eq!(copies.items[0].owner_project, "owner");
            assert_eq!(
                copies.items[0].generation,
                Some(pending[0].generation.get())
            );
            assert_eq!(copies.items[0].bucket_incarnation, pending[0].incarnation);
            assert_eq!(
                fs::read(
                    data.0
                        .join("sync-conflicts")
                        .join(copies.items[0].conflict_id.to_string())
                )
                .unwrap(),
                b"before"
            );
            let worker_catalog = catalog.clone();
            let worker_root = root.clone();
            let mut stopping = stop.subscribe();
            worker = tokio::spawn(EXPORT_HOOK.scope(hook.clone(), async move {
                run_live(&worker_catalog, &worker_root, &mut stopping).await
            }));
        }
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap_or_else(|error| panic!("phase {phase}, remove={remove}: {error}"));
        if remove {
            assert!(!tree.0.join("test-bucket/object").exists());
        } else {
            assert_eq!(
                std::fs::read(tree.0.join("test-bucket/object")).unwrap(),
                b"after\0\xff",
                "{phase}"
            );
        }
        let binding = catalog.sync_state().binding(&bucket).await.unwrap();
        let tracked = catalog
            .sync_state()
            .get_path(binding.clone(), "test-bucket/object".into())
            .await
            .unwrap();
        if remove {
            assert!(tracked.is_none());
        } else {
            assert_eq!(tracked.unwrap().generation.get(), record.generation);
        }
        if phase == "cancel" {
            let page = root
                .conflict_page(
                    crate::storage::sync_state::conflicts::ConflictPosition::Start,
                    crate::storage::sync_state::PageLimit::LARGEST,
                    crate::storage::sync_state::authority::ControlAuthority::Permissive,
                )
                .await
                .unwrap();
            assert_eq!(page.items.len(), 1);
            let conflict = &page.items[0];
            assert_eq!(conflict.owner_project, "owner");
            assert_eq!(conflict.generation, Some(record.generation));
            assert_eq!(conflict.bucket, bucket.as_str());
            assert_eq!(conflict.bucket_incarnation.to_hex().len(), 32);
            assert_eq!(
                fs::read(
                    data.0
                        .join("sync-conflicts")
                        .join(conflict.conflict_id.to_string())
                )
                .unwrap(),
                b"external"
            );
        }
        let writeback = root
            .writeback(
                Limits::new(
                    super::super::CONFLICT_MAX_BYTES,
                    super::super::CONFLICT_MAX_ENTRIES,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(writeback.status().await.unwrap().stop(), Stop::Ready);
        // The naming probe keeps its directory; no operation may leave anything else.
        assert_eq!(
            std::fs::read_dir(tree.0.join(mapping::WORKSPACE))
                .unwrap()
                .filter(|entry| {
                    entry.as_ref().unwrap().file_name() != crate::sync::naming::PROBE_DIRECTORY
                })
                .count(),
            0
        );
        let opened = catalog
            .admin(Origin::Api)
            .open_object(&bucket, &ObjectName::new("object").unwrap(), None)
            .await;
        if remove {
            assert!(matches!(opened, Err(AdminError::NotFound)));
        } else {
            let (_, reader) = opened.unwrap();
            let file = reader.into_file().unwrap();
            let mut body = [0; 8];
            assert_eq!(fsutil::read_at(&file, &mut body, 0).unwrap(), 7);
            assert_eq!(&body[..7], b"after\0\xff");
        }
        stop.send_replace(true);
        hook.release.notify_one();
        worker.await.unwrap().unwrap();
        drop(writeback);
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn export_concurrent_api_and_external_changes_never_strand_unpublished_operations() {
    for (phase, action) in [
        ("before-open", "overwrite"),
        ("before-open", "delete"),
        ("before-arm", "overwrite"),
        ("before-arm", "delete"),
        ("before-publish", "external-create"),
        ("before-durable", "external-edit"),
        ("before-durable", "overwrite"),
        ("before-durable", "delete"),
    ] {
        let data = TestDir::new();
        let tree = TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let bucket = BucketName::new("test-bucket").unwrap();
        let admin = catalog.admin(Origin::SyncIngest);
        admin
            .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
            .await
            .unwrap();
        let record = publish(&catalog, &bucket, b"api").await;
        let config = Config {
            data_dir: data.0.clone(),
            sync_dir: Some(tree.0.clone()),
            ..Default::default()
        };
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        let root = crate::sync::test_support::probed(root).await;
        catalog.enable_change_journal().await.unwrap();
        let fence = catalog.current_fence().await.unwrap();
        let writeback = root
            .writeback(Limits::new(4096, 10).unwrap())
            .await
            .unwrap();
        let path = tree.0.join("test-bucket/object");
        std::fs::create_dir(tree.0.join("test-bucket")).unwrap();
        let identical = action == "external-edit";
        if identical {
            std::fs::write(&path, b"api").unwrap();
        }
        let hook = std::sync::Arc::new(Hook {
            phase,
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let name = ObjectName::new("object").unwrap();
        let mut summary = Reconciliation::default();
        let (result, ()) = tokio::join!(
            EXPORT_HOOK.scope(
                hook.clone(),
                reconcile_live_object(
                    &catalog,
                    &root,
                    &writeback,
                    &bucket,
                    &name,
                    fence,
                    &mut summary
                )
            ),
            async {
                tokio::time::timeout(HANG_GUARD, hook.entered.notified())
                    .await
                    .unwrap();
                match action {
                    "overwrite" => {
                        publish(&catalog, &bucket, b"new-api").await;
                    }
                    "delete" => {
                        admin
                            .delete_object(
                                &bucket,
                                &name,
                                GenerationCondition::Is(
                                    Generation::new(record.generation).unwrap(),
                                ),
                            )
                            .await
                            .unwrap();
                    }
                    _ => std::fs::write(&path, b"external").unwrap(),
                }
                hook.release.notify_one();
            }
        );
        result.unwrap();
        assert!(
            summary.deferred > 0 || summary.conflicting > 0,
            "{phase} {action}"
        );
        assert_eq!(
            writeback.status().await.unwrap().stop(),
            Stop::Ready,
            "{phase} {action}"
        );
        assert!(writeback.page(0, 256).await.unwrap().is_empty());
        if action.starts_with("external") {
            let conflicts = root
                .conflict_page(
                    crate::storage::sync_state::conflicts::ConflictPosition::Start,
                    crate::storage::sync_state::PageLimit::LARGEST,
                    crate::storage::sync_state::authority::ControlAuthority::Permissive,
                )
                .await
                .unwrap();
            assert_eq!(conflicts.items.len(), 1, "{phase} {action}");
            assert_eq!(
                std::fs::read(
                    data.0
                        .join("sync-conflicts")
                        .join(conflicts.items[0].conflict_id.to_string())
                )
                .unwrap(),
                b"external"
            );
            assert_eq!(std::fs::read(&path).unwrap(), b"api");
        }
        if action == "overwrite" {
            let (_, reader) = admin.open_object(&bucket, &name, None).await.unwrap();
            let mut file = reader.into_file().unwrap();
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
            assert_eq!(bytes, b"new-api");
        }
        drop(writeback);
        drop(root);
        catalog.close().await.unwrap();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        reconcile(&catalog, &root).await.unwrap();
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[derive(Clone, Copy)]
enum LiveFailure {
    Error,
    Panic,
    /// Catalog admission timed out, as under contention.
    Busy,
}

/// Failures armed per working tree, since tests run in parallel and the mirror runs on its
/// own thread, out of reach of a task-local hook.
static LIVE_FAILURES: std::sync::Mutex<Vec<(PathBuf, LiveFailure)>> =
    std::sync::Mutex::new(Vec::new());

/// A mirror run held before it starts, armed per working tree like `LIVE_FAILURES`.
struct LiveHold {
    entered: tokio::sync::oneshot::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}

static LIVE_HOLDS: std::sync::Mutex<Vec<(PathBuf, LiveHold)>> = std::sync::Mutex::new(Vec::new());

/// Holds the next run of the mirror on `tree` before it starts. The first receiver fires once
/// the run is held; the run continues when the sender fires or drops, so a failed test cannot
/// leave the mirror thread blocked.
fn hold_next_run(
    tree: &Path,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    std::sync::mpsc::Sender<()>,
) {
    let (entered, held) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    LIVE_HOLDS.lock().unwrap().push((
        tree.to_owned(),
        LiveHold {
            entered,
            release: released,
        },
    ));
    (held, release)
}

pub(super) fn injected_live_failure(config: &Config) -> Result<(), SyncError> {
    let hold = {
        let mut holds = LIVE_HOLDS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        holds
            .iter()
            .position(|(tree, _)| config.sync_dir.as_ref() == Some(tree))
            .map(|index| holds.remove(index).1)
    };
    if let Some(hold) = hold {
        let _ = hold.entered.send(());
        let _ = hold.release.recv();
    }
    let mut armed = LIVE_FAILURES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(index) = armed
        .iter()
        .position(|(tree, _)| config.sync_dir.as_ref() == Some(tree))
    else {
        return Ok(());
    };
    let (_, failure) = armed.remove(index);
    drop(armed);
    match failure {
        LiveFailure::Error => Err(SyncError::Io(io::Error::other(
            "injected live sync failure",
        ))),
        LiveFailure::Panic => panic!("injected live sync panic"),
        LiveFailure::Busy => Err(SyncError::Admin(AdminError::Busy {
            retry_after: std::time::Duration::from_secs(1),
        })),
    }
}

async fn started_mirror(failure: Option<LiveFailure>) -> (TestDir, TestDir, Catalog, Task) {
    started_mirror_failing(failure.as_slice()).await
}

/// Starts the mirror with `failures` armed for its first runs, in order.
async fn started_mirror_failing(failures: &[LiveFailure]) -> (TestDir, TestDir, Catalog, Task) {
    let (data, tree, catalog, _, root) = live_fixture().await;
    let config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    LIVE_FAILURES
        .lock()
        .unwrap()
        .extend(failures.iter().map(|failure| (tree.0.clone(), *failure)));
    let task = start(catalog.clone(), root, &config).unwrap();
    (data, tree, catalog, task)
}

/// Whether the Storage API reports ready, as `/_internal/ready` decides it.
fn api_ready(catalog: &Catalog) -> bool {
    crate::server::State::for_test(catalog.clone(), crate::auth::Auth::permissive()).is_ready()
}

/// Waits until `task`'s mirror is in `state`.
async fn mirror_reaches(task: &Task, state: MirrorState) {
    let reached = tokio::time::timeout(HANG_GUARD, task.status().reached(state))
        .await
        .unwrap_or(false);
    assert!(reached, "the mirror never became {state}");
}

/// Trees whose mirror thread panics once it has paused, outside any supervised run.
static PAUSE_PANICS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

pub(super) fn injected_pause_panic(config: &Config) {
    let mut armed = PAUSE_PANICS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(index) = armed
        .iter()
        .position(|tree| config.sync_dir.as_ref() == Some(tree))
    else {
        return;
    };
    armed.remove(index);
    drop(armed);
    panic!("injected paused mirror panic");
}

/// A panic outside a supervised run, here while the mirror pauses, still reports the mirror
/// stopped and withdraws its controls, so nothing reads as resumable that cannot resume; the
/// Storage API stays ready.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mirror_thread_that_panics_outside_a_run_reports_stopped_and_withdraws_its_controls() {
    use MirrorState::{Paused, Starting, Stopped};
    let (data, tree, catalog, _, root) = live_fixture().await;
    let config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    LIVE_FAILURES
        .lock()
        .unwrap()
        .push((tree.0.clone(), LiveFailure::Error));
    PAUSE_PANICS.lock().unwrap().push(tree.0.clone());
    let task = start(catalog.clone(), root, &config).unwrap();
    mirror_reaches(&task, Stopped).await;
    assert!(catalog.sync_controls().unwrap().is_none());
    assert!(api_ready(&catalog));
    let status = task.status();
    assert!(
        task.finish().await.is_err(),
        "the thread ended without its orderly shutdown"
    );
    // The thread reports its panic only once it has unwound, after the guard reported it
    // stopped and dropped the sender that `finish` waits for.
    eventually_async("the mirror thread to report its panic", || async {
        crate::task::tests::reported_panics("sync-mirror")
            .iter()
            .any(|panic| panic == "injected paused mirror panic")
    })
    .await;
    assert_eq!(status.entered(), [Starting, Paused, Stopped]);
    // The unwound thread released the root, so the directory can be owned again.
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, crate::storage::sync_state::root::Activation::Open)
        .await
        .unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

/// Queues a wait on `task`'s mirror; the receiver answers it.
async fn queue_wait(
    task: &Task,
    timeout: Duration,
) -> tokio::sync::oneshot::Receiver<
    Result<crate::sync::report::ending::WaitResult, crate::sync::control::ControlError>,
> {
    let (reply, response) = tokio::sync::oneshot::channel();
    task.rescan
        .send(Command::Wait {
            deadline: tokio::time::Instant::now() + timeout,
            settling: Default::default(),
            reply,
            identity: None,
        })
        .await
        .unwrap();
    response
}

/// An error or panic pauses synchronization until an explicit rescan succeeds; the Storage API
/// stays ready throughout, and after shutdown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopped_mirror_pauses_synchronization_and_keeps_the_api_ready() {
    use MirrorState::{Paused, Starting, Stopped};
    for failure in [LiveFailure::Error, LiveFailure::Panic] {
        let (_data, _tree, catalog, task) = started_mirror(Some(failure)).await;
        mirror_reaches(&task, Paused).await;
        assert!(
            catalog.is_healthy(),
            "a stopped mirror does not poison the catalog"
        );
        assert!(api_ready(&catalog), "a paused mirror leaves the API ready");
        let (control, root) = catalog.sync_controls().unwrap().unwrap();
        await_paused(&root).await;
        assert_eq!(
            control.status.state(),
            Paused,
            "status reads the same state"
        );
        assert_eq!(
            task.status().state(),
            Paused,
            "inspection cannot resume a paused observer"
        );
        drop((control, root));
        let status = task.status();
        task.finish().await.unwrap();
        assert_eq!(status.state(), Stopped, "the stop is permanent");
        assert_eq!(status.entered(), [Starting, Paused, Stopped]);
        assert!(api_ready(&catalog), "a stopped mirror leaves the API ready");
        catalog.close().await.unwrap();
    }
    // Other tests' mirror threads report their own panics under the same name.
    let panics = crate::task::tests::reported_panics("sync-mirror");
    assert_eq!(
        panics
            .iter()
            .filter(|panic| *panic == "injected live sync panic")
            .count(),
        1,
        "{panics:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_rescan_resumes_a_paused_mirror_and_shutdown_releases_ownership() {
    use MirrorState::{Paused, Running, Starting, Stopped};
    let (_data, tree, catalog, task) = started_mirror(Some(LiveFailure::Error)).await;
    mirror_reaches(&task, Paused).await;
    assert!(api_ready(&catalog));
    assert!(matches!(
        catalog
            .sync_state()
            .activate_root(&tree.0, crate::storage::sync_state::root::Activation::Open)
            .await,
        Err(RootError::InUse)
    ));
    let (_, root) = catalog.sync_controls().unwrap().unwrap();
    await_paused(&root).await;
    fs::create_dir_all(tree.0.join("test-bucket")).unwrap();
    fs::write(
        tree.0.join("test-bucket/operator-note"),
        b"preserve while paused",
    )
    .unwrap();
    assert_eq!(
        task.status().state(),
        Paused,
        "inspection cannot resume a paused observer"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(10), task.rescan())
            .await
            .unwrap()
            .unwrap()
            .baseline_complete
    );
    assert!(root.observation().await.unwrap().paused_reason.is_none());
    assert_eq!(
        task.status().state(),
        Running,
        "the rescan's reply follows the resumed state"
    );
    assert!(api_ready(&catalog));
    drop(root);
    let status = task.status();
    task.finish().await.unwrap();
    assert_eq!(status.entered(), [Starting, Paused, Running, Stopped]);
    assert!(api_ready(&catalog));
    assert!(catalog.sync_controls().unwrap().is_none());
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, crate::storage::sync_state::root::Activation::Open)
        .await
        .unwrap();
    assert_eq!(
        fs::read(tree.0.join("test-bucket/operator-note")).unwrap(),
        b"preserve while paused"
    );
    drop(root);
    catalog.close().await.unwrap();
}

/// A rescan that would resume a paused mirror and fails on contention leaves it paused under
/// its recorded reason and answers with the failure; the mirror does not resume on its own once
/// a retry delay passes, which only an explicit rescan that succeeds may do.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resume_that_fails_transiently_stays_paused_until_a_rescan_succeeds() {
    use MirrorState::{Paused, Running, Starting, Stopped};
    let (_data, tree, catalog, task) = started_mirror(Some(LiveFailure::Error)).await;
    mirror_reaches(&task, Paused).await;
    let (_, root) = catalog.sync_controls().unwrap().unwrap();
    await_paused(&root).await;
    let recorded = root.observation().await.unwrap().paused_reason;
    LIVE_FAILURES
        .lock()
        .unwrap()
        .push((tree.0.clone(), LiveFailure::Busy));
    let failed = tokio::time::timeout(HANG_GUARD, task.rescan())
        .await
        .unwrap()
        .unwrap_err();
    // The rescan hears its own cause and the reason the pause keeps.
    assert!(
        matches!(
            &failed,
            ControlError::RescanFailed {
                reason,
                change: crate::sync::control::PauseChange::Kept,
                error: SyncError::Admin(AdminError::Busy { .. }),
            } if Some(*reason) == recorded
        ),
        "{failed:?}"
    );
    // A wait answers `Paused` only from the pause, so the mirror went back to it instead of
    // retrying on its own.
    let waited = tokio::time::timeout(HANG_GUARD, queue_wait(&task, HANG_GUARD).await)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(waited.reason, crate::sync::WaitReason::Paused);
    assert_eq!(task.status().state(), Paused);
    assert_eq!(root.observation().await.unwrap().paused_reason, recorded);
    assert!(
        tokio::time::timeout(HANG_GUARD, task.rescan())
            .await
            .unwrap()
            .unwrap()
            .baseline_complete
    );
    drop(root);
    let status = task.status();
    task.finish().await.unwrap();
    assert_eq!(status.entered(), [Starting, Paused, Running, Stopped]);
    catalog.close().await.unwrap();
}

async fn await_paused(root: &RootLease) {
    eventually_async("the observer to pause", || async {
        root.observation().await.unwrap().paused_reason.is_some()
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_disk_full_pauses_sync_for_controls_keeps_the_api_ready_and_shutdown_releases_the_root()
 {
    let (_data, tree, catalog, task) = started_mirror(None).await;
    task.rescan().await.unwrap();
    let (_, root) = catalog.sync_controls().unwrap().unwrap();
    crate::storage::sync_state::writeback::inject_live_export_failure(
        root.status().root_id,
        "copy",
        100,
    );
    let bucket = BucketName::new("test-bucket").unwrap();
    let record = publish_origin(&catalog, &bucket, b"API winner survives", Origin::Api).await;
    await_paused(&root).await;
    // The mirror enters the state before it records the pause.
    assert_eq!(task.status().state(), MirrorState::Paused);
    assert!(
        api_ready(&catalog),
        "a full disk pauses only synchronization"
    );
    assert!(catalog.sync_controls().unwrap().is_some());
    let page = root
        .conflict_page(
            crate::storage::sync_state::conflicts::ConflictPosition::Start,
            crate::storage::sync_state::PageLimit::LARGEST,
            crate::storage::sync_state::authority::ControlAuthority::Permissive,
        )
        .await
        .unwrap();
    assert!(page.items.is_empty());
    let (reply, receive) = tokio::sync::oneshot::channel();
    let (control, _) = catalog.sync_controls().unwrap().unwrap();
    control
        .sender
        .try_send(Command::Resolve {
            request: crate::storage::sync_state::conflicts::ResolveRequest {
                operation_id: "missing-but-observed".into(),
                action: crate::storage::sync_state::conflicts::Action::Discard,
                target: crate::storage::sync_state::conflicts::ResolveTarget::Conflict(
                    ConflictTarget {
                        conflict_id: "0".repeat(32).parse().unwrap(),
                        revision: "0".repeat(32).parse().unwrap(),
                        expected_generation: record.generation,
                    },
                ),
            },
            reply,
            identity: None,
        })
        .unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), receive)
            .await
            .unwrap()
            .unwrap(),
        Err(crate::storage::sync_state::conflicts::Error::RevisionMismatch)
    ));
    assert_eq!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket, &ObjectName::new("object").unwrap(), None)
            .await
            .unwrap(),
        record
    );
    drop((control, root));
    tokio::time::timeout(Duration::from_secs(2), task.finish())
        .await
        .unwrap()
        .unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, crate::storage::sync_state::root::Activation::Open)
        .await
        .unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_interrupts_a_persistent_io_retry_after_its_owned_transition() {
    let (_data, tree, catalog, task) = started_mirror(None).await;
    task.rescan().await.unwrap();
    let (_, root) = catalog.sync_controls().unwrap().unwrap();
    crate::storage::sync_state::writeback::inject_live_export_failure(
        root.status().root_id,
        "copy",
        100,
    );
    publish_origin(
        &catalog,
        &BucketName::new("test-bucket").unwrap(),
        b"API body",
        Origin::Api,
    )
    .await;
    eventually_async("a pending writeback", || async {
        root.observation().await.unwrap().pending_writeback != 0
    })
    .await;
    drop(root);
    tokio::time::timeout(Duration::from_secs(2), task.finish())
        .await
        .unwrap()
        .unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, crate::storage::sync_state::root::Activation::Open)
        .await
        .unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn failed_explicit_rescan_hands_its_reply_to_the_pause() {
    use crate::storage::sync_state::observation::PauseReason;
    let (_data, tree, catalog, _bucket, root) = live_fixture().await;
    reconcile(&catalog, &root).await.unwrap();
    let writeback = root.default_writeback().await.unwrap();
    let workspace = tree.0.join(mapping::WORKSPACE);
    fs::rename(&workspace, tree.0.join("retained-workspace")).unwrap();
    fs::create_dir(&workspace).unwrap();
    let config = Config::default();
    let mut state = LiveState {
        cursor: catalog.current_fence().await.unwrap(),
        baseline: BaselineProgress {
            gate: BaselineGate::Allowed,
            complete: true,
        },
        skipped: SkippedExports::default(),
    };
    let (_sender, stopping) = tokio::sync::watch::channel(false);
    let mut control = LiveControl {
        scope: LiveScope {
            catalog: &catalog,
            root: &root,
            config: &config,
            writeback: &writeback,
            stopping: &stopping,
            entries: &EntryReportSender::default(),
        },
        state: &mut state,
    };
    let (reply, mut result) = tokio::sync::oneshot::channel();
    let error = control
        .handle_authorized(Command::Rescan {
            reply,
            identity: None,
        })
        .await
        .unwrap_err();
    assert_eq!(error.pause_reason(), PauseReason::RecoveryRequired);
    // The mirror pauses, records the pause and only then answers, from outside the run.
    assert!(matches!(error, SyncError::RescanStopped(_)), "{error:?}");
    assert!(matches!(
        result.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(root.observation().await.unwrap().paused_reason, None);
    drop(writeback);
    drop(root);
    catalog.close().await.unwrap();
}

/// Each retry waits twice as long as the one before, up to the cap, and a reset starts over.
/// Export steps wait 1 s and then 2 s before a third failure pauses synchronization.
#[test]
fn retry_backoff_doubles_up_to_its_cap_and_starts_over_after_a_reset() {
    let mut backoff = Backoff::new(Duration::from_millis(10), Duration::from_millis(70));
    for expected in [10, 20, 40, 70, 70] {
        assert_eq!(backoff.next_delay(), Duration::from_millis(expected));
    }
    assert_eq!(backoff.retries(), 5);
    backoff.reset();
    assert_eq!(backoff.retries(), 0);
    assert_eq!(backoff.next_delay(), Duration::from_millis(10));

    let mut export = EXPORT_BACKOFF;
    assert_eq!(export.next_delay(), Duration::from_secs(1));
    assert_eq!(export.next_delay(), Duration::from_secs(2));
    assert_eq!(export.retries(), EXPORT_RETRIES);
}

#[test]
fn only_typed_transient_io_failures_are_retried() {
    assert!(!retryable(&writeback::Error::Admin(AdminError::Io(
        io::Error::other("permanent corruption").into()
    ))));
    for errno in [libc::EMFILE, libc::ENFILE, libc::EAGAIN] {
        let error = writeback::Error::Admin(io::Error::from_raw_os_error(errno).into());
        assert!(retryable(&error));
    }
}

#[tokio::test]
async fn wait_deadline_interrupts_settling_without_publishing_an_incomplete_scan() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    reconcile(&catalog, &root).await.unwrap();
    fs::write(tree.0.join("test-bucket/new-source"), b"unsettled").unwrap();
    let config = Config::default();
    let mut state = LiveState {
        cursor: catalog.current_fence().await.unwrap(),
        baseline: BaselineProgress {
            gate: BaselineGate::Allowed,
            complete: true,
        },
        skipped: SkippedExports::default(),
    };
    let (_sender, stopping) = tokio::sync::watch::channel(false);
    let writeback = root.default_writeback().await.unwrap();
    let mut control = LiveControl {
        scope: LiveScope {
            catalog: &catalog,
            root: &root,
            config: &config,
            writeback: &writeback,
            stopping: &stopping,
            entries: &EntryReportSender::default(),
        },
        state: &mut state,
    };
    let result = tokio::time::timeout(
        Duration::from_millis(500),
        control.wait(tokio::time::Instant::now() + Duration::from_millis(50)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.reason, crate::sync::WaitReason::Timeout);
    assert!(result.reconciliation.unwrap().interrupted);
    assert!(matches!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket, &ObjectName::new("new-source").unwrap(), None)
            .await,
        Err(AdminError::NotFound)
    ));
    assert_eq!(state.baseline.gate, BaselineGate::Deferred);
    drop(writeback);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn wait_captures_a_fixed_post_scan_fence_and_does_not_chase_later_writes() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    catalog.enable_change_journal().await.unwrap();
    let config = Config::default();
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    let cursor = catalog.current_fence().await.unwrap();
    publish_origin(&catalog, &bucket, b"captured", Origin::Api).await;
    let mut state = LiveState {
        cursor,
        baseline: BaselineProgress {
            gate: BaselineGate::Allowed,
            complete: true,
        },
        skipped: SkippedExports::default(),
    };
    let (_sender, stopping) = tokio::sync::watch::channel(false);
    let writeback = root.default_writeback().await.unwrap();
    let mut control = LiveControl {
        scope: LiveScope {
            catalog: &catalog,
            root: &root,
            config: &config,
            writeback: &writeback,
            stopping: &stopping,
            entries: &EntryReportSender::default(),
        },
        state: &mut state,
    };
    assert!(
        control
            .wait(tokio::time::Instant::now())
            .await
            .unwrap()
            .timed_out
    );
    let hook = std::sync::Arc::new(Hook {
        phase: "wait-fence",
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let result = EXPORT_HOOK
        .scope(hook.clone(), async {
            let work=control.wait(tokio::time::Instant::now() + Duration::from_secs(10));
            tokio::pin!(work);
            tokio::select! {
                result=&mut work=>panic!("wait finished before its fence checkpoint: {:?}",result.map(|value|value.reason)),
                result=tokio::time::timeout(Duration::from_secs(5),hook.entered.notified())=>{result.unwrap();}
            }
                    let other = BucketName::new("later-bucket").unwrap();
                    catalog
                        .admin(Origin::Api)
                        .ensure_bucket(
                            BucketDefinition::default_for(&other, "owner", &SystemClock).unwrap(),
                        )
                        .await
                        .unwrap();
                    publish_origin(&catalog, &other, b"after capture", Origin::Api).await;
            hook.release.notify_one();
            work.await
        })
        .await;
    let result = result.unwrap();
    assert!(result.completed, "{:?}", result.reason);
    assert!(!result.timed_out);
    assert!(
        result.journal_fence.unwrap().sequence
            < catalog.current_fence().await.unwrap().sequence.get()
    );
    assert_eq!(
        fs::read(tree.0.join("test-bucket/object")).unwrap(),
        b"captured"
    );
    assert!(!tree.0.join("later-bucket/object").exists());
    drop(writeback);
    drop(root);
    catalog.close().await.unwrap();
}

/// Contention that outlasts the catalog wait makes the mirror retry. Before its first pass has
/// finished, the mirror stays starting through those retries, so a healthcheck keeps waiting
/// for that pass; the Storage API stays ready throughout.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_transient_mirror_failure_is_retried_until_it_clears() {
    use MirrorState::{Running, Starting, Stopped};
    let (_data, tree, catalog, task) = started_mirror_failing(&[LiveFailure::Busy; 4]).await;
    // A served rescan shows a retried run got past its start.
    tokio::time::timeout(std::time::Duration::from_secs(10), task.rescan())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task.status().state(), Running);
    assert!(api_ready(&catalog));
    assert!(
        LIVE_FAILURES
            .lock()
            .unwrap()
            .iter()
            .all(|(armed, _)| *armed != tree.0),
        "every armed failure was retried"
    );
    let status = task.status();
    task.finish().await.unwrap();
    assert_eq!(status.entered(), [Starting, Running, Stopped]);
    assert!(api_ready(&catalog));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn retry_repeats_transient_errors_and_keeps_permanent_causes() {
    let (_data, _tree, catalog, _bucket, root) = live_fixture().await;
    let mut attempts = 0;
    let result = with_retry(&root, || {
        attempts += 1;
        std::future::ready(if attempts == 1 {
            Err(writeback::Error::Admin(AdminError::Io(
                io::ErrorKind::Interrupted.into(),
            )))
        } else {
            Ok(7)
        })
    })
    .await
    .unwrap();
    assert_eq!(result, 7);
    assert_eq!(attempts, 2);
    attempts = 0;
    let error = with_retry(&root, || {
        attempts += 1;
        std::future::ready(Err::<(), _>(writeback::Error::Admin(AdminError::Forbidden)))
    })
    .await
    .unwrap_err();
    assert_eq!(attempts, 1);
    assert!(matches!(
        error,
        SyncError::Writeback(writeback::Error::Admin(AdminError::Forbidden))
    ));
    drop(root);
    catalog.close().await.unwrap();
}

#[test]
fn writeback_retries_resource_pressure_without_retrying_unclassified_failures() {
    for error in [
        io::Error::from_raw_os_error(libc::EMFILE),
        io::Error::from_raw_os_error(libc::ENFILE),
        io::ErrorKind::TimedOut.into(),
    ] {
        assert!(retryable(&writeback::Error::Admin(error.into())));
    }
    assert!(!retryable(&writeback::Error::Admin(AdminError::Io(
        io::Error::other("invalid state").into()
    ))));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mirror_stopped_by_shutdown_stays_ready() {
    let (_data, _tree, catalog, task) = started_mirror(None).await;
    // A served rescan shows the mirror is past its start and running.
    tokio::time::timeout(std::time::Duration::from_secs(10), task.rescan())
        .await
        .unwrap()
        .unwrap();
    assert!(api_ready(&catalog));
    let status = task.status();
    task.finish().await.unwrap();
    assert_eq!(status.state(), MirrorState::Stopped);
    assert!(api_ready(&catalog));
    catalog.close().await.unwrap();
}

/// A wait sent before the first reconciliation of this process waits for it, even though an
/// earlier process already completed the baseline that the catalog keeps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wait_sent_while_starting_succeeds_only_after_the_first_reconciliation() {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    reconcile(&catalog, &root).await.unwrap();
    assert!(root.observation().await.unwrap().baseline_complete);
    publish_origin(&catalog, &bucket, b"exported at start", Origin::Api).await;
    let exported = tree.0.join("test-bucket/object");
    let config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    let (held, release) = hold_next_run(&tree.0);
    let task = start(catalog.clone(), root, &config).unwrap();
    tokio::time::timeout(HANG_GUARD, held)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task.status().state(), MirrorState::Starting);
    assert!(api_ready(&catalog), "the API is ready while sync starts");
    let mut response = queue_wait(&task, HANG_GUARD).await;
    assert!(matches!(
        response.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert!(!exported.exists());
    release.send(()).unwrap();
    let waited = tokio::time::timeout(HANG_GUARD, response)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(waited.reason, crate::sync::WaitReason::Ready);
    assert_eq!(
        fs::read(&exported).unwrap(),
        b"exported at start",
        "the wait answered after the first reconciliation exported the object"
    );
    assert_eq!(task.status().state(), MirrorState::Running);
    task.finish().await.unwrap();
    catalog.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn native_watch_permission_retry_reports_retrying_keeps_the_api_ready_and_shutdown_releases_the_root()
 {
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    let (_data, tree, catalog, task) = started_mirror(None).await;
    // Drain startup exports before attributing a retrying observer to the permission change.
    let response = queue_wait(&task, Duration::from_secs(10)).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), response)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .reason,
        crate::sync::WaitReason::Ready
    );
    let bucket = tree.0.join("test-bucket");
    std::fs::set_permissions(&bucket, std::fs::Permissions::from_mode(0o0)).unwrap();
    // Retries that keep failing pause the mirror; the rebuild reported retrying before that.
    mirror_reaches(&task, MirrorState::Paused).await;
    let entered = task.status().entered();
    let retrying = entered
        .iter()
        .position(|state| *state == MirrorState::Retrying);
    let paused = entered
        .iter()
        .position(|state| *state == MirrorState::Paused);
    assert!(retrying.is_some() && retrying < paused, "{entered:?}");
    assert!(api_ready(&catalog), "an unwatched subtree pauses only sync");
    tokio::time::timeout(Duration::from_secs(5), task.finish())
        .await
        .unwrap()
        .unwrap();
    std::fs::set_permissions(&bucket, std::fs::Permissions::from_mode(0o700)).unwrap();
    catalog.close().await.unwrap();
}

/// Answers one `sync wait` that replays the journal from `cursor`.
async fn wait_from(
    catalog: &Catalog,
    root: &RootLease,
    cursor: JournalFence,
) -> crate::sync::report::ending::WaitResult {
    try_wait_from(catalog, root, cursor).await.unwrap()
}

/// Like [`wait_from`], returning the error of a pass that failed.
async fn try_wait_from(
    catalog: &Catalog,
    root: &RootLease,
    cursor: JournalFence,
) -> Result<crate::sync::report::ending::WaitResult, SyncError> {
    let config = Config::default();
    let mut state = LiveState {
        cursor,
        baseline: BaselineProgress {
            gate: BaselineGate::Allowed,
            complete: true,
        },
        skipped: SkippedExports::default(),
    };
    let (_sender, stopping) = tokio::sync::watch::channel(false);
    let writeback = root.default_writeback().await.unwrap();
    let mut control = LiveControl {
        scope: LiveScope {
            catalog,
            root,
            config: &config,
            writeback: &writeback,
            stopping: &stopping,
            entries: &EntryReportSender::default(),
        },
        state: &mut state,
    };
    control.wait(crate::test_support::hang_deadline()).await
}

/// Exports `object` with `body` and tracks it as `record` describes; returns the journal fence
/// after the export.
async fn tracked_export(
    catalog: &Catalog,
    bucket: &BucketName,
    root: &RootLease,
    tree: &std::path::Path,
    record: Retrack,
) -> JournalFence {
    catalog.enable_change_journal().await.unwrap();
    publish_origin(catalog, bucket, b"before", Origin::Api).await;
    reconcile(catalog, root).await.unwrap();
    let cursor = catalog.current_fence().await.unwrap();
    retrack(catalog, root, tree, "test-bucket/object", record).await;
    cursor
}

/// A tracked file that still holds its tracked bytes is no external edit while a newer API
/// generation awaits export, whether the scan skips it or must hash it because its fingerprint
/// names another inode number or is racy. The wait's scan counts it as nothing, the export
/// replaces it with the newer generation, and the wait is ready without a conflict.
#[tokio::test]
async fn wait_exports_over_an_unchanged_file_whatever_its_fingerprint_needs() {
    use crate::sync::WaitReason;
    for record in [Retrack::Stable, Retrack::Renumbered, Retrack::Racy] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        let cursor = tracked_export(&catalog, &bucket, &root, &tree.0, record).await;
        publish_origin(&catalog, &bucket, b"newer", Origin::Api).await;
        let result = wait_from(&catalog, &root, cursor).await;
        let summary = result.reconciliation.unwrap();
        assert_eq!(
            (
                result.reason,
                summary.exported,
                summary.conflicting,
                summary.deferred
            ),
            (WaitReason::Ready, 1, 0, 0),
            "{record:?}"
        );
        assert_eq!(
            fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"newer",
            "{record:?}"
        );
        assert!(root.findings_page(0, 256).await.unwrap().is_empty());
        assert_eq!(root.observation().await.unwrap().unresolved_conflicts, 0);
        drop(root);
        catalog.close().await.unwrap();
    }
}

/// When a name collision holds back the export of the newer generation, the unchanged renumbered
/// file still counts as nothing, so the wait ends at once instead of reporting a source that
/// keeps changing until its deadline. The object stays reachable through the API, so the wait
/// succeeds and reports the finding in its count.
#[tokio::test]
async fn wait_reports_the_finding_that_holds_back_the_export_over_a_renumbered_file() {
    use crate::sync::WaitReason;
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    let cursor = tracked_export(&catalog, &bucket, &root, &tree.0, Retrack::Renumbered).await;
    publish_origin(&catalog, &bucket, b"newer", Origin::Api).await;
    publish_named(&catalog, &bucket, "object/child", b"child", Origin::Api).await;
    let result = wait_from(&catalog, &root, cursor).await;
    let summary = result.reconciliation.unwrap();
    assert_eq!(
        (result.reason, summary.conflicting, summary.deferred),
        (WaitReason::Ready, 0, 0)
    );
    let findings = root.findings_page(0, 256).await.unwrap();
    assert!(
        findings.iter().any(|finding| finding.name == "object"
            && finding.reason == FindingKind::Unmapped(Reason::Collision)),
        "{findings:?}"
    );
    assert_eq!(
        result.findings.map(|counted| counted.unmapped),
        Some(findings.len() as u64)
    );
    assert_eq!(
        fs::read(tree.0.join("test-bucket/object")).unwrap(),
        b"before"
    );
    assert_eq!(root.observation().await.unwrap().unresolved_conflicts, 0);
    drop(root);
    catalog.close().await.unwrap();
}

/// A collision carried over from schema 57 does not say whether it was a name collision or an
/// export conflict. A reconciliation that meets the name collision again records it as one.
#[tokio::test]
async fn reconciliation_classifies_a_carried_over_collision_that_is_a_name_collision() {
    let (_data, _tree, catalog, bucket, root) = live_fixture().await;
    publish_origin(&catalog, &bucket, b"file", Origin::Api).await;
    publish_named(&catalog, &bucket, "object/child", b"child", Origin::Api).await;
    reconcile(&catalog, &root).await.unwrap();
    let reason = || {
        let root = root.clone();
        async move {
            root.findings_page(0, 256)
                .await
                .unwrap()
                .into_iter()
                .find(|finding| finding.name == "object")
                .map(|finding| finding.reason)
        }
    };
    assert_eq!(
        reason().await,
        Some(FindingKind::Unmapped(Reason::Collision))
    );
    assert!(
        root.unclassify_finding("test-bucket", "object")
            .await
            .unwrap()
    );
    assert_eq!(reason().await, Some(FindingKind::UnclassifiedCollision));
    reconcile(&catalog, &root).await.unwrap();
    assert_eq!(
        reason().await,
        Some(FindingKind::Unmapped(Reason::Collision))
    );
    drop(root);
    catalog.close().await.unwrap();
}

/// Different bytes behind a renumbered fingerprint, with the same size and times, are still an
/// external edit: against a newer API generation the wait reports a conflict.
#[tokio::test]
async fn wait_reports_different_bytes_behind_a_renumbered_fingerprint_as_a_conflict() {
    use crate::sync::WaitReason;
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    catalog.enable_change_journal().await.unwrap();
    publish_origin(&catalog, &bucket, b"before", Origin::Api).await;
    reconcile(&catalog, &root).await.unwrap();
    let cursor = catalog.current_fence().await.unwrap();
    let path = tree.0.join("test-bucket/object");
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    fs::write(&path, b"BEFORE").unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    retrack(
        &catalog,
        &root,
        &tree.0,
        "test-bucket/object",
        Retrack::Renumbered,
    )
    .await;
    publish_origin(&catalog, &bucket, b"newer", Origin::Api).await;
    let result = wait_from(&catalog, &root, cursor).await;
    let summary = result.reconciliation.unwrap();
    // The scan records the conflict. The export of the newer generation meets the edit too,
    // keeps it as a conflict revision and installs the API bytes.
    assert_eq!(
        (result.reason, summary.conflicting, summary.exported),
        (WaitReason::FindingsPending, 2, 1)
    );
    assert_eq!(root.observation().await.unwrap().unresolved_conflicts, 1);
    assert_eq!(fs::read(&path).unwrap(), b"newer");
    drop(root);
    catalog.close().await.unwrap();
}

/// After the API deletes the object, a tracked file that still holds its tracked bytes is
/// removed the same way whether the scan skips it or must hash it: the scan counts it as
/// nothing, and the removal export deletes it.
#[tokio::test]
async fn wait_removes_an_unchanged_file_deleted_through_the_api_whatever_its_fingerprint_needs() {
    use crate::sync::WaitReason;
    for record in [Retrack::Stable, Retrack::Renumbered, Retrack::Racy] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        let cursor = tracked_export(&catalog, &bucket, &root, &tree.0, record).await;
        let name = ObjectName::new("object").unwrap();
        let live = catalog
            .admin(Origin::Api)
            .get_object(&bucket, &name, None)
            .await
            .unwrap();
        catalog
            .admin(Origin::Api)
            .delete_object(
                &bucket,
                &name,
                GenerationCondition::Is(Generation::new(live.generation).unwrap()),
            )
            .await
            .unwrap();
        let result = wait_from(&catalog, &root, cursor).await;
        let summary = result.reconciliation.unwrap();
        assert_eq!(
            (result.reason, summary.conflicting, summary.deferred),
            (WaitReason::Ready, 0, 0),
            "{record:?}"
        );
        assert!(!tree.0.join("test-bucket/object").exists(), "{record:?}");
        assert_eq!(root.observation().await.unwrap().unresolved_conflicts, 0);
        drop(root);
        catalog.close().await.unwrap();
    }
}
