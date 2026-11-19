//! Exports into a bucket whose whole directory is missing while the tree tracks files in it.
//! Creating the directory for one export would turn the missing files into deletions counted one
//! by one, which the guard lets through below its threshold, so every export into the bucket
//! waits until the deletion is approved or rejected, or the directory is back. A bucket emptied
//! while the server was stopped waits for the same decision.
#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::baseline::exists;
use super::ingest_durability::Started;
use super::*;
use crate::storage::sync_state::deletions::DeletionSet;
use crate::sync::control::{WaitReason, WaitResult};
use crate::sync::outstanding::{Holdup, Outstanding};

pub(super) const BODY: &[u8] = b"tracked bytes";
pub(super) const BUCKET: &str = "test-bucket";
pub(super) const TRACKED: [&str; 2] = ["first", "second"];
const ALL: [&str; 3] = ["first", "second", "third"];

/// A root whose bucket directory holds `first` and `second`, which a start tracked and whose
/// baseline it completed.
pub(super) async fn tracked_pair() -> (TestDir, TestDir, BucketName) {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    drop(root);
    catalog.close().await.unwrap();
    write_pair(&tree.0);
    Started::start(&data.0, &tree.0).await.close().await;
    (data, tree, bucket)
}

/// Puts the bucket directory with `first` and `second` in place, as a user restores it.
fn write_pair(tree: &Path) {
    fs::create_dir(tree.join(BUCKET)).unwrap();
    for name in TRACKED {
        fs::write(tree.join(BUCKET).join(name), BODY).unwrap();
    }
}

async fn all_exist(catalog: &Catalog, bucket: &BucketName) {
    for name in ALL {
        assert!(exists(catalog, bucket, name).await, "{name}");
    }
}

/// The directory goes away while the server is stopped, and an object is written into its
/// bucket through the API meanwhile. The startup reconciliation does not export the new object,
/// so the directory stays missing and both tracked files wait for approval. Putting the
/// directory back releases the export.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_object_written_while_stopped_waits_with_its_missing_bucket_directory() {
    let (data, tree, bucket) = tracked_pair().await;
    fs::remove_dir_all(tree.0.join(BUCKET)).unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    publish_named(&catalog, &bucket, "third", BODY, Origin::Api).await;
    catalog.close().await.unwrap();

    let started = Started::start(&data.0, &tree.0).await;
    assert!(!tree.0.join(BUCKET).exists());
    let set = started.root.deletion_set().await.unwrap().unwrap();
    assert_eq!((set.candidate_count, set.guarded), (2, true));
    assert_eq!(set.completed_count, 0);
    all_exist(&started.catalog, &bucket).await;
    started.close().await;

    write_pair(&tree.0);
    let started = Started::start(&data.0, &tree.0).await;
    assert_eq!(fs::read(tree.0.join(BUCKET).join("third")).unwrap(), BODY);
    let set = started.root.deletion_set().await.unwrap();
    assert_eq!(set.map_or(0, |set| set.candidate_count), 0);
    all_exist(&started.catalog, &bucket).await;
    started.close().await;
}

pub(super) async fn wait_control(commands: &tokio::sync::mpsc::Sender<Command>) -> WaitResult {
    let (reply, result) = tokio::sync::oneshot::channel();
    commands
        .send(Command::Wait {
            deadline: crate::test_support::hang_deadline(),
            settling: Default::default(),
            reply,
            identity: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(HANG_GUARD, result)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

async fn approve_control(commands: &tokio::sync::mpsc::Sender<Command>, root: &RootLease) {
    let revision = root.deletion_set().await.unwrap().unwrap().revision;
    let (reply, approved) = tokio::sync::oneshot::channel();
    commands
        .send(Command::Approve {
            revision,
            reply,
            identity: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(HANG_GUARD, approved)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// Rejects the root's pending set, and returns the set the rejection answered with.
pub(super) async fn reject_control(
    commands: &tokio::sync::mpsc::Sender<Command>,
    root: &RootLease,
) -> DeletionSet {
    let revision = root.deletion_set().await.unwrap().unwrap().revision;
    let (reply, rejected) = tokio::sync::oneshot::channel();
    commands
        .send(Command::Reject {
            revision,
            reply,
            identity: None,
        })
        .await
        .unwrap();
    let rejected = tokio::time::timeout(HANG_GUARD, rejected)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(rejected.revision, revision);
    rejected
}

/// A mirror that runs over the tree and answers the controls sent to `commands`, as the server's
/// control routes hand them on.
pub(super) struct Live {
    pub(super) catalog: Catalog,
    pub(super) root: RootLease,
    pub(super) commands: tokio::sync::mpsc::Sender<Command>,
    pub(super) stop: tokio::sync::watch::Sender<bool>,
    worker: tokio::task::JoinHandle<Result<(), SyncError>>,
}

impl Live {
    pub(super) async fn start(data: &Path, tree: &Path) -> Self {
        Self::start_with(data, tree, None).await
    }

    /// Starts the mirror, stopped by `hook`, when there is one, at each checkpoint it names.
    pub(super) async fn start_with(
        data: &Path,
        tree: &Path,
        hook: Option<std::sync::Arc<Hook>>,
    ) -> Self {
        let catalog = Catalog::open(data).await.unwrap();
        let config = Config {
            data_dir: data.to_owned(),
            sync_dir: Some(tree.to_owned()),
            ..Default::default()
        };
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let (commands, mut requests) = tokio::sync::mpsc::channel(1);
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let run = async move {
            run_live_controlled(&worker_catalog, &worker_root, &mut stopping, &mut requests).await
        };
        let worker = match hook {
            Some(hook) => tokio::spawn(EXPORT_HOOK.scope(hook, run)),
            None => tokio::spawn(run),
        };
        Self {
            catalog,
            root,
            commands,
            stop,
            worker,
        }
    }

    pub(super) async fn stop(self) {
        self.stop.send_replace(true);
        self.worker.await.unwrap().unwrap();
        drop(self.root);
        self.catalog.close().await.unwrap();
    }
}

/// The directory goes away while the server runs, and an API write into its bucket follows. The
/// replayed export waits instead of recreating the directory, a wait reports the pending
/// approval, and approving the set deletes the tracked files and then runs the export.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_api_write_into_a_missing_bucket_directory_waits_for_the_approval() {
    let (data, tree, bucket) = tracked_pair().await;
    let live = Live::start(&data.0, &tree.0).await;
    let (catalog, root, commands) = (&live.catalog, &live.root, &live.commands);
    fs::remove_dir_all(tree.0.join(BUCKET)).unwrap();
    rescan_control(commands).await;
    let set = root.deletion_set().await.unwrap().unwrap();
    assert_eq!((set.candidate_count, set.guarded), (2, true));

    publish_named(catalog, &bucket, "third", BODY, Origin::Api).await;
    // The held export stays queued for the retry after the approval; every wait, not only the
    // first after the write, reads it as the pending approval, and so does the status.
    for _ in 0..2 {
        let waited = wait_control(commands).await;
        assert_eq!(waited.reason, WaitReason::DeletionApproval);
    }
    let status = Outstanding::of_status(
        &root.observation().await.unwrap(),
        root.deletion_set().await.unwrap().as_ref(),
    );
    assert_eq!(status.first(), Holdup::DeletionApproval);
    assert!(!tree.0.join(BUCKET).exists());
    // The API write moved the journal, and the inventory found the same members, so the set
    // keeps the revision the user may already be about to approve.
    let held = root.deletion_set().await.unwrap().unwrap();
    assert_eq!((held.candidate_count, held.guarded), (2, true));
    assert_eq!(held.revision, set.revision);
    assert_eq!(root.automatic_deletion_count().await.unwrap(), 0);
    all_exist(catalog, &bucket).await;

    approve_control(commands, root).await;
    eventually_async("the held export to run", || async {
        tree.0.join(BUCKET).join("third").is_file()
    })
    .await;
    assert_eq!(fs::read(tree.0.join(BUCKET).join("third")).unwrap(), BODY);
    for name in TRACKED {
        assert!(!exists(catalog, &bucket, name).await, "{name}");
    }
    assert!(exists(catalog, &bucket, "third").await);
    live.stop().await;
}

/// An API write into a bucket emptied while the server was stopped reaches the tree, as any
/// write does. The file it adds was not there at the start, so it does not end the wait of the
/// files that went while the server was stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_api_write_into_a_bucket_emptied_while_stopped_keeps_the_rest_waiting() {
    let (data, tree, bucket) = tracked_pair().await;
    for name in TRACKED {
        fs::remove_file(tree.0.join(BUCKET).join(name)).unwrap();
    }
    let live = Live::start(&data.0, &tree.0).await;
    let (catalog, root, commands) = (&live.catalog, &live.root, &live.commands);
    rescan_control(commands).await;
    let set = root.deletion_set().await.unwrap().unwrap();
    assert_eq!((set.candidate_count, set.guarded), (2, true));

    publish_named(catalog, &bucket, "third", BODY, Origin::Api).await;
    eventually_async("the API write to reach the tree", || async {
        tree.0.join(BUCKET).join("third").is_file()
    })
    .await;
    // The rescan inventories again with `third` tracked in the bucket.
    rescan_control(commands).await;
    let waited = wait_control(commands).await;
    assert_eq!(waited.reason, WaitReason::DeletionApproval);
    let held = root.deletion_set().await.unwrap().unwrap();
    assert_eq!((held.candidate_count, held.guarded), (2, true));
    assert_eq!(held.completed_count, 0);
    assert_eq!(fs::read(tree.0.join(BUCKET).join("third")).unwrap(), BODY);
    all_exist(catalog, &bucket).await;
    live.stop().await;
}

/// Inventories `root` again, as a later pass of the same run does, and returns its set.
async fn inventory_again(started: &Started, data: &Path, tree: &Path) -> DeletionSet {
    let config = Config {
        data_dir: data.to_owned(),
        sync_dir: Some(tree.to_owned()),
        ..Default::default()
    };
    let pass = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    crate::sync::deletion::inventory(&started.root, &config, pass.gate(), &stopping)
        .await
        .unwrap()
        .unwrap()
}

/// Every tracked file of a bucket goes while the server is stopped and its directory stays, as
/// when a bind mount of the bucket comes up empty. The start takes the files to approval however
/// few they are, and a later pass of the run keeps them there while the bucket stays empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bucket_emptied_while_stopped_waits_for_approval() {
    let (data, tree, bucket) = tracked_pair().await;
    for name in TRACKED {
        fs::remove_file(tree.0.join(BUCKET).join(name)).unwrap();
    }
    let started = Started::start(&data.0, &tree.0).await;
    let set = started.root.deletion_set().await.unwrap().unwrap();
    assert_eq!((set.candidate_count, set.guarded), (2, true));
    assert_eq!(set.completed_count, 0);
    let again = inventory_again(&started, &data.0, &tree.0).await;
    assert_eq!((again.candidate_count, again.guarded), (2, true));
    for name in TRACKED {
        assert!(exists(&started.catalog, &bucket, name).await, "{name}");
    }
    assert!(tree.0.join(BUCKET).is_dir());
    started.close().await;
}

/// One of the files a bucket lost while the server was stopped comes back, so the bucket is no
/// longer emptied, and the other file is left to the counts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_back_in_a_bucket_emptied_while_stopped_leaves_the_rest_to_the_counts() {
    let (data, tree, _) = tracked_pair().await;
    for name in TRACKED {
        fs::remove_file(tree.0.join(BUCKET).join(name)).unwrap();
    }
    let started = Started::start(&data.0, &tree.0).await;
    let set = started.root.deletion_set().await.unwrap().unwrap();
    assert_eq!((set.candidate_count, set.guarded), (2, true));
    fs::write(tree.0.join(BUCKET).join("first"), BODY).unwrap();
    let again = inventory_again(&started, &data.0, &tree.0).await;
    assert_eq!((again.candidate_count, again.guarded), (1, false));
    started.close().await;
}

/// A bucket that keeps one of its tracked files is not emptied: the other file, deleted while
/// the server was stopped, deletes its object at the start without approval, as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_file_deleted_while_stopped_still_deletes_its_object() {
    let (data, tree, bucket) = tracked_pair().await;
    fs::remove_file(tree.0.join(BUCKET).join("first")).unwrap();
    let started = Started::start(&data.0, &tree.0).await;
    assert!(!exists(&started.catalog, &bucket, "first").await);
    assert!(exists(&started.catalog, &bucket, "second").await);
    started.close().await;
}

/// Emptying a bucket while the mirror runs is a deletion the user made in front of it, which the
/// counts alone take to approval, as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bucket_emptied_while_running_needs_no_approval() {
    let (data, tree, bucket) = tracked_pair().await;
    let started = Started::start(&data.0, &tree.0).await;
    for name in TRACKED {
        fs::remove_file(tree.0.join(BUCKET).join(name)).unwrap();
    }
    let set = inventory_again(&started, &data.0, &tree.0).await;
    assert_eq!((set.candidate_count, set.guarded), (2, false));
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    assert_eq!(
        crate::sync::deletion::execute(&started.catalog, &started.root, set, &stopping)
            .await
            .unwrap(),
        2
    );
    for name in TRACKED {
        assert!(!exists(&started.catalog, &bucket, name).await, "{name}");
    }
    started.close().await;
}

/// Rejecting the set that holds the files of a bucket emptied while the server was stopped
/// writes them back from their objects, deletes nothing, and leaves nothing to wait for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejecting_a_bucket_emptied_while_stopped_writes_its_files_back() {
    let (data, tree, bucket) = tracked_pair().await;
    for name in TRACKED {
        fs::remove_file(tree.0.join(BUCKET).join(name)).unwrap();
    }
    let live = Live::start(&data.0, &tree.0).await;
    let (catalog, root, commands) = (&live.catalog, &live.root, &live.commands);
    rescan_control(commands).await;
    let rejected = reject_control(commands, root).await;
    assert_eq!((rejected.candidate_count, rejected.guarded), (2, true));
    // The wait runs after the rejection's write-back, which the mirror finishes first.
    assert_eq!(wait_control(commands).await.reason, WaitReason::Ready);
    for name in TRACKED {
        assert_eq!(fs::read(tree.0.join(BUCKET).join(name)).unwrap(), BODY);
    }
    let set = root.deletion_set().await.unwrap();
    assert_eq!(set.map_or(0, |set| set.candidate_count), 0);
    for name in TRACKED {
        assert!(exists(catalog, &bucket, name).await, "{name}");
    }
    live.stop().await;
}

/// Rejecting the set that a missing bucket directory holds brings the directory back with the
/// files, and then runs the export of an API write that waited for the directory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejecting_a_set_held_for_a_missing_bucket_directory_brings_the_directory_back() {
    let (data, tree, bucket) = tracked_pair().await;
    let live = Live::start(&data.0, &tree.0).await;
    let (catalog, root, commands) = (&live.catalog, &live.root, &live.commands);
    fs::remove_dir_all(tree.0.join(BUCKET)).unwrap();
    rescan_control(commands).await;
    publish_named(catalog, &bucket, "third", BODY, Origin::Api).await;
    assert_eq!(
        wait_control(commands).await.reason,
        WaitReason::DeletionApproval
    );
    assert!(!tree.0.join(BUCKET).exists());

    let rejected = reject_control(commands, root).await;
    assert_eq!((rejected.candidate_count, rejected.guarded), (2, true));
    assert_eq!(wait_control(commands).await.reason, WaitReason::Ready);
    for name in ALL {
        assert_eq!(fs::read(tree.0.join(BUCKET).join(name)).unwrap(), BODY);
    }
    all_exist(catalog, &bucket).await;
    live.stop().await;
}

/// What the user decides about the set that waits.
#[derive(Clone, Copy, Debug)]
enum Decision {
    Approve,
    Reject,
}

impl Decision {
    fn command(
        self,
        revision: crate::storage::sync_state::OpaqueId,
        reply: Reply<DeletionSet>,
    ) -> Command {
        match self {
            Self::Approve => Command::Approve {
                revision,
                reply,
                identity: None,
            },
            Self::Reject => Command::Reject {
                revision,
                reply,
                identity: None,
            },
        }
    }
}

/// A change to the tree that touches no member of the set that waits.
#[derive(Clone, Copy, Debug)]
enum Unrelated {
    /// A file directly in the directory, which belongs to no bucket.
    RootFile,
    /// A late notification about the directory that the export of an object written through
    /// the API into another bucket created, after the pass that took the export in. The write
    /// moved the journal past the set, which that pass confirmed under the same revision.
    ExportedDirectory,
}

/// Runs `work` while letting the mirror past every checkpoint `hook` stops it at.
pub(super) async fn released<F: std::future::Future>(hook: &Hook, work: F) -> F::Output {
    tokio::pin!(work);
    loop {
        tokio::select! {
            biased;
            output = &mut work => return output,
            () = hook.entered.notified() => hook.release.notify_one(),
        }
    }
}

/// A decision sent right after a native event about another path, before any pass looked at
/// the tree again, is accepted the first time under the revision the status named. The event
/// once withdrew the set until that pass confirmed it, so the command the guidance names failed
/// with 409 whenever a late notification arrived first. The hook stops the mirror right after the
/// event took effect, so the decision is the next thing it handles.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decision_right_after_a_change_to_another_path_is_accepted_the_first_time() {
    for unrelated in [Unrelated::RootFile, Unrelated::ExportedDirectory] {
        for decision in [Decision::Reject, Decision::Approve] {
            decide_after_a_change_to_another_path(unrelated, decision).await;
        }
    }
}

async fn decide_after_a_change_to_another_path(unrelated: Unrelated, decision: Decision) {
    let (data, tree, bucket) = tracked_pair().await;
    fs::remove_dir_all(tree.0.join(BUCKET)).unwrap();
    let hook = std::sync::Arc::new(Hook {
        phase: "observation-invalidated",
        entered: Default::default(),
        release: Default::default(),
    });
    let live = Live::start_with(&data.0, &tree.0, Some(hook.clone())).await;
    let (catalog, root, commands) = (&live.catalog, &live.root, &live.commands);
    let waited = released(&hook, wait_control(commands)).await;
    assert_eq!(waited.reason, WaitReason::DeletionApproval);
    let set = root.deletion_set().await.unwrap().unwrap();
    assert_eq!((set.candidate_count, set.guarded), (2, true));

    match unrelated {
        Unrelated::RootFile => fs::write(tree.0.join("README.md"), b"notes").unwrap(),
        Unrelated::ExportedDirectory => {
            let other = BucketName::new("other-bucket").unwrap();
            catalog
                .admin(Origin::Api)
                .ensure_bucket(
                    BucketDefinition::default_for(&other, "owner", &SystemClock).unwrap(),
                )
                .await
                .unwrap();
            publish_named(catalog, &other, "dir/file", BODY, Origin::Api).await;
            let waited = released(&hook, wait_control(commands)).await;
            assert_eq!(waited.reason, WaitReason::DeletionApproval);
            let exported = tree.0.join("other-bucket/dir");
            assert_eq!(fs::read(exported.join("file")).unwrap(), BODY);
            let confirmed = root.deletion_set().await.unwrap().unwrap();
            assert_eq!(confirmed.revision, set.revision);
            // A notification about the exported directory that leaves nothing to take in.
            fs::write(exported.join("transient"), b"gone").unwrap();
            fs::remove_file(exported.join("transient")).unwrap();
        }
    }
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
    let (reply, answer) = tokio::sync::oneshot::channel();
    commands
        .send(decision.command(set.revision, reply))
        .await
        .unwrap();
    let releasing = hook.clone();
    let releaser = tokio::spawn(async move {
        loop {
            releasing.release.notify_one();
            releasing.entered.notified().await;
        }
    });
    let decided = tokio::time::timeout(HANG_GUARD, answer)
        .await
        .unwrap()
        .unwrap()
        .unwrap_or_else(|error| panic!("{decision:?} after {unrelated:?}: {error:?}"));
    assert_eq!(
        decided.revision, set.revision,
        "{decision:?} after {unrelated:?}"
    );

    assert_eq!(wait_control(commands).await.reason, WaitReason::Ready);
    for name in TRACKED {
        let kept = exists(catalog, &bucket, name).await;
        match decision {
            Decision::Approve => assert!(!kept, "{name}"),
            Decision::Reject => {
                assert!(kept, "{name}");
                assert_eq!(fs::read(tree.0.join(BUCKET).join(name)).unwrap(), BODY);
            }
        }
    }
    live.stop().await;
    releaser.abort();
}
