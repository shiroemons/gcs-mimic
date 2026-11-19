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
use crate::storage::sync_state::deletions::{DeletionAuthorization, DeletionSet};
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
/// longer emptied. The other file was part of the set that waits, so it keeps waiting instead
/// of being left to the counts, which would delete it unasked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_back_in_a_bucket_emptied_while_stopped_keeps_the_rest_waiting() {
    let (data, tree, _) = tracked_pair().await;
    for name in TRACKED {
        fs::remove_file(tree.0.join(BUCKET).join(name)).unwrap();
    }
    let started = Started::start(&data.0, &tree.0).await;
    let set = started.root.deletion_set().await.unwrap().unwrap();
    assert_eq!((set.candidate_count, set.guarded), (2, true));
    fs::write(tree.0.join(BUCKET).join("first"), BODY).unwrap();
    let again = inventory_again(&started, &data.0, &tree.0).await;
    assert_eq!((again.candidate_count, again.guarded), (1, true));
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

/// A change to the tree while the set waits, which a decision follows before any pass.
#[derive(Clone, Copy, Debug)]
enum Change {
    /// A file directly in the directory, which belongs to no bucket.
    RootFile,
    /// A late notification about the directory that the export of an object written through
    /// the API into another bucket created, after the pass that took the export in. The write
    /// moved the journal past the set, which that pass confirmed under the same revision.
    ExportedDirectory,
    /// The bucket directory and the member `first` back, so that the set no longer holds what
    /// the tree shows.
    MemberBack,
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
    for change in [Change::RootFile, Change::ExportedDirectory] {
        for decision in [Decision::Reject, Decision::Approve] {
            decide_after_a_change(change, decision).await;
        }
    }
}

/// A decision sent right after a member's file came back, before any pass looked at the tree
/// again, is refused as stale: the inventory before the decision finds the set no longer holds
/// what the tree shows and replaces it. An approval therefore deletes nothing, not even the
/// object of the member still missing, which the new set holds and keeps waiting for.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decision_right_after_a_member_came_back_is_refused_as_stale() {
    for decision in [Decision::Reject, Decision::Approve] {
        decide_after_a_change(Change::MemberBack, decision).await;
    }
}

/// A stop that ends the inventory before a decision, before it compared every member, refuses
/// the decision as a shutdown: nothing confirmed the set against the tree, so it keeps waiting
/// under its revision and every object remains. The decision was once recorded and answered
/// as accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decision_whose_inventory_a_stop_ended_is_refused() {
    for decision in [Decision::Approve, Decision::Reject] {
        let (data, tree, bucket) = tracked_pair().await;
        fs::remove_dir_all(tree.0.join(BUCKET)).unwrap();
        let hook = std::sync::Arc::new(Hook {
            phase: "deletion-candidate",
            entered: Default::default(),
            release: Default::default(),
        });
        let live = Live::start_with(&data.0, &tree.0, Some(hook.clone())).await;
        let waited = released(&hook, wait_control(&live.commands)).await;
        assert_eq!(waited.reason, WaitReason::DeletionApproval);
        let set = live.root.deletion_set().await.unwrap().unwrap();
        let (reply, answer) = tokio::sync::oneshot::channel();
        live.commands
            .send(decision.command(set.revision, reply))
            .await
            .unwrap();
        tokio::time::timeout(HANG_GUARD, hook.entered.notified())
            .await
            .unwrap();
        live.stop.send_replace(true);
        hook.release.notify_one();
        let answer = released(&hook, async {
            tokio::time::timeout(HANG_GUARD, answer).await.unwrap()
        })
        .await
        .unwrap();
        assert!(
            matches!(answer, Err(ControlError::Shutdown)),
            "{decision:?}: {answer:?}"
        );
        let kept = live.root.deletion_set().await.unwrap().unwrap();
        assert_eq!(
            (kept.revision, kept.authorization),
            (set.revision, DeletionAuthorization::Withheld),
            "{decision:?}"
        );
        for name in TRACKED {
            assert!(exists(&live.catalog, &bucket, name).await, "{decision:?}");
        }
        live.stop().await;
    }
}

async fn decide_after_a_change(change: Change, decision: Decision) {
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

    match change {
        Change::RootFile => fs::write(tree.0.join("README.md"), b"notes").unwrap(),
        Change::MemberBack => {
            fs::create_dir(tree.0.join(BUCKET)).unwrap();
            fs::write(tree.0.join(BUCKET).join("first"), BODY).unwrap();
        }
        Change::ExportedDirectory => {
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
    let answer = tokio::time::timeout(HANG_GUARD, answer)
        .await
        .unwrap()
        .unwrap();
    let decided = Decided {
        live: &live,
        bucket: &bucket,
        tree: &tree.0,
        revision: set.revision,
        decision,
    };
    match change {
        Change::MemberBack => decided.refused_as_stale(answer, 1).await,
        Change::RootFile | Change::ExportedDirectory => {
            let accepted =
                answer.unwrap_or_else(|error| panic!("{decision:?} after {change:?}: {error:?}"));
            assert_eq!(
                accepted.revision, set.revision,
                "{decision:?} after {change:?}"
            );
            decided.applied().await;
        }
    }
    live.stop().await;
    releaser.abort();
}

/// An API change that the tree does not show, made while the set waits: one in the bucket
/// whose directory is missing writes no file and so brings no pass.
#[derive(Clone, Copy, Debug)]
enum ApiWrite {
    /// A new object beside the members.
    Beside,
    /// A new generation of the member `first`, so that its tracked file no longer tracks the
    /// live generation.
    ToMember,
    /// Versioning enabled on the members' bucket, which decides whether their deleted objects
    /// stay recoverable.
    BucketUpdate,
}

impl ApiWrite {
    async fn make(self, catalog: &Catalog, bucket: &BucketName) {
        match self {
            Self::Beside => {
                publish_named(catalog, bucket, "third", b"written", Origin::Api).await;
            }
            Self::ToMember => {
                publish_named(catalog, bucket, "first", b"written", Origin::Api).await;
            }
            Self::BucketUpdate => {
                catalog
                    .patch_bucket(
                        bucket.as_str(),
                        crate::bucket::BucketMetadataUpdate {
                            versioning: Some(crate::bucket::VersioningUpdate::Set(
                                crate::bucket::Versioning::Enabled,
                            )),
                            ..Default::default()
                        },
                        Default::default(),
                        PERMISSIVE,
                    )
                    .await
                    .unwrap();
            }
        }
    }

    /// The members the set that replaces the named one holds, when this change refuses a
    /// decision on it.
    const fn refusal(self) -> Option<u64> {
        match self {
            Self::Beside => None,
            Self::ToMember => Some(1),
            Self::BucketUpdate => Some(2),
        }
    }
}

/// A decision right after an API write that touched no member is accepted under the revision
/// the status still names. The status once lost the revision as soon as the journal moved,
/// and the decision failed with 409 until a rescan or a wait ran a pass.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decision_right_after_an_api_write_beside_the_members_is_accepted() {
    for decision in [Decision::Reject, Decision::Approve] {
        decide_after_an_api_write(ApiWrite::Beside, decision).await;
    }
}

/// A decision right after an API change to a member is refused as stale: one that replaced a
/// member's object leaves the inventory before the decision without that member, and an
/// update of the members' bucket, which may make their deletions unrecoverable, replaces the
/// set whatever the tree shows. The new set keeps waiting, and no object goes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_decision_right_after_an_api_write_to_a_member_is_refused_as_stale() {
    for write in [ApiWrite::ToMember, ApiWrite::BucketUpdate] {
        for decision in [Decision::Reject, Decision::Approve] {
            decide_after_an_api_write(write, decision).await;
        }
    }
}

async fn decide_after_an_api_write(write: ApiWrite, decision: Decision) {
    let (data, tree, bucket) = tracked_pair().await;
    fs::remove_dir_all(tree.0.join(BUCKET)).unwrap();
    let live = Live::start(&data.0, &tree.0).await;
    assert_eq!(
        wait_control(&live.commands).await.reason,
        WaitReason::DeletionApproval
    );
    let set = live.root.deletion_set().await.unwrap().unwrap();
    write.make(&live.catalog, &bucket).await;
    // The status reads the set the decision is asked for until something decides it.
    let named = live
        .root
        .deletion_set()
        .await
        .unwrap()
        .map(|set| set.revision);
    assert_eq!(named, Some(set.revision), "{decision:?} after {write:?}");
    let (reply, answer) = tokio::sync::oneshot::channel();
    live.commands
        .send(decision.command(set.revision, reply))
        .await
        .unwrap();
    let answer = tokio::time::timeout(HANG_GUARD, answer)
        .await
        .unwrap()
        .unwrap();
    let decided = Decided {
        live: &live,
        bucket: &bucket,
        tree: &tree.0,
        revision: set.revision,
        decision,
    };
    if let Some(remaining) = write.refusal() {
        decided.refused_as_stale(answer, remaining).await;
    } else {
        let accepted =
            answer.unwrap_or_else(|error| panic!("{decision:?} after {write:?}: {error:?}"));
        assert_eq!(
            accepted.revision, set.revision,
            "{decision:?} after {write:?}"
        );
        decided.applied().await;
    }
    live.stop().await;
}

/// A decision on the set `revision` that the mirror answered, and what it should have done.
struct Decided<'a> {
    live: &'a Live,
    bucket: &'a BucketName,
    tree: &'a Path,
    revision: crate::storage::sync_state::OpaqueId,
    decision: Decision,
}

impl Decided<'_> {
    /// The decision took effect: an approval deleted the members' objects, and a rejection kept
    /// them and wrote their files back.
    async fn applied(&self) {
        let (decision, catalog) = (self.decision, &self.live.catalog);
        assert_eq!(
            wait_control(&self.live.commands).await.reason,
            WaitReason::Ready
        );
        for name in TRACKED {
            let kept = exists(catalog, self.bucket, name).await;
            match decision {
                Decision::Approve => assert!(!kept, "{name}"),
                Decision::Reject => {
                    assert!(kept, "{name}");
                    assert_eq!(fs::read(self.tree.join(BUCKET).join(name)).unwrap(), BODY);
                }
            }
        }
    }

    /// The decision was refused as stale and changed nothing: a new set of `remaining` members
    /// replaced the named one and waits, every object remains, and nothing wrote the missing
    /// file back.
    async fn refused_as_stale(&self, answer: Result<DeletionSet, ControlError>, remaining: u64) {
        let decision = self.decision;
        assert!(
            matches!(
                answer,
                Err(ControlError::Failed(SyncError::Root(RootError::Admin(
                    AdminError::Precondition
                ))))
            ),
            "{decision:?}: {answer:?}"
        );
        let replaced = self.live.root.deletion_set().await.unwrap().unwrap();
        assert_ne!(replaced.revision, self.revision, "{decision:?}");
        assert_eq!(
            (replaced.candidate_count, replaced.guarded),
            (remaining, true),
            "{decision:?}"
        );
        assert_eq!(
            wait_control(&self.live.commands).await.reason,
            WaitReason::DeletionApproval
        );
        for name in TRACKED {
            let kept = exists(&self.live.catalog, self.bucket, name).await;
            assert!(kept, "{decision:?}: {name}");
        }
        assert!(!self.tree.join(BUCKET).join("second").exists());
    }
}
