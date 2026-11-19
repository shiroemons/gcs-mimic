//! A rejected deletion set writes its files back from their objects. The rejection is durable
//! and its write-back resumes after a failure, a stop or a power loss, so no inventory in
//! between takes the files not yet written back for files the user deleted: one file back in
//! the directory and the other still missing looks like a single deletion, below every
//! threshold.
#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::baseline::exists;
use super::missing_bucket::{
    BODY, BUCKET, Live, TRACKED, reject_control, released, tracked_pair, wait_control,
};
use super::*;
use crate::storage::fault::{Fault, Step};
use crate::storage::sync_state::deletions::DeletionSet;
use crate::sync::control::WaitReason;
use crate::sync::outstanding::{Holdup, Outstanding};
use crate::sync::test_support::restore_tree;

/// What a user writes at a member's path behind the write-back.
const USER: &[u8] = b"the user's own bytes";

/// Stops the mirror before it writes back each member of a rejected set.
fn member_hook() -> std::sync::Arc<Hook> {
    std::sync::Arc::new(Hook {
        phase: "rejected-member",
        entered: Default::default(),
        release: Default::default(),
    })
}

async fn reach_member(hook: &Hook) {
    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
        .await
        .unwrap();
}

/// Starts the mirror over the pair, with `hook` when there is one, removes the bucket
/// directory and rescans, which takes both files to approval.
async fn awaiting_pair(hook: Option<std::sync::Arc<Hook>>) -> (TestDir, TestDir, BucketName, Live) {
    let (data, tree, bucket) = tracked_pair().await;
    let live = Live::start_with(&data.0, &tree.0, hook).await;
    fs::remove_dir_all(tree.0.join(BUCKET)).unwrap();
    rescan_control(&live.commands).await;
    let set = live.root.deletion_set().await.unwrap().unwrap();
    assert_eq!((set.candidate_count, set.guarded), (2, true));
    (data, tree, bucket, live)
}

/// Both objects are there, and the tree holds the file of each.
async fn written_back(catalog: &Catalog, tree: &Path, bucket: &BucketName) {
    for name in TRACKED {
        assert!(exists(catalog, bucket, name).await, "{name}");
        assert_eq!(
            fs::read(tree.join(BUCKET).join(name)).unwrap(),
            BODY,
            "{name}"
        );
    }
}

/// The mirror as the server runs it, which pauses after a failure until a rescan, unlike
/// [`Live`], whose run ends with the failure.
struct Running {
    catalog: Catalog,
    task: Task,
}

impl Running {
    /// Starts the mirror over the pair, removes the bucket directory and rescans, which takes
    /// both files to approval.
    async fn awaiting(data: &Path, tree: &Path) -> Self {
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
        let task = start(catalog.clone(), root, &config).unwrap();
        fs::remove_dir_all(tree.join(BUCKET)).unwrap();
        task.rescan().await.unwrap();
        let running = Self { catalog, task };
        let set = running.root().deletion_set().await.unwrap().unwrap();
        assert_eq!((set.candidate_count, set.guarded), (2, true));
        running
    }

    fn root(&self) -> std::sync::Arc<RootLease> {
        self.catalog.sync_controls().unwrap().unwrap().1
    }

    /// Rejects the set, whose write-back fails at the member `skip` files in, every attempt
    /// the retries make included, which pauses the mirror.
    async fn reject_failing_at(&self, skip: usize) -> DeletionSet {
        let root = self.root();
        crate::storage::sync_state::writeback::inject_live_export_failure_after(
            root.status().root_id,
            "install",
            skip,
            3,
        );
        let rejected = reject_control(&self.task.rescan, &root).await;
        mirror_reaches(&self.task, MirrorState::Paused).await;
        rejected
    }

    async fn finish(self) {
        self.task.finish().await.unwrap();
        self.catalog.close().await.unwrap();
    }
}

/// Every attempt at the second member's file fails, so the write-back ends with the first
/// file back and the second still missing, and the mirror pauses. The rescan the pause asks
/// for resumes the write-back instead of inventorying that state, and deletes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_back_that_fails_part_way_resumes_on_rescan_and_deletes_nothing() {
    let (data, tree, bucket) = tracked_pair().await;
    let running = Running::awaiting(&data.0, &tree.0).await;
    running.reject_failing_at(1).await;
    for name in TRACKED {
        assert!(exists(&running.catalog, &bucket, name).await, "{name}");
    }
    assert_eq!(fs::read(tree.0.join(BUCKET).join("first")).unwrap(), BODY);
    assert!(!tree.0.join(BUCKET).join("second").exists());
    running.task.rescan().await.unwrap();
    written_back(&running.catalog, &tree.0, &bucket).await;
    assert_eq!(
        wait_control(&running.task.rescan).await.reason,
        WaitReason::Ready
    );
    written_back(&running.catalog, &tree.0, &bucket).await;
    running.finish().await;
}

/// A write-back that keeps failing stops deletion detection, so it has to show: the status
/// counts the files it has left and reads the pause it left, whose reason and remedy name what
/// to correct, and a wait ends on that pause instead of reading the root as ready. Once the
/// cause is gone the rescan the remedy names writes the rest back and the count reaches zero.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_back_that_keeps_failing_shows_in_the_status_and_holds_every_wait() {
    let (data, tree, bucket) = tracked_pair().await;
    let running = Running::awaiting(&data.0, &tree.0).await;
    running.reject_failing_at(1).await;
    // The paused mirror answers a wait once it recorded the pause, which status then reads.
    let waited = wait_control(&running.task.rescan).await;
    assert_eq!(waited.reason, WaitReason::Paused);
    let observation = running.root().observation().await.unwrap();
    assert_eq!(observation.rejected_writeback, 1);
    assert_eq!(observation.paused_reason, Some(PauseReason::IoFailure));
    let remedy = waited.remedy(Default::default()).unwrap();
    assert!(
        remedy.starts_with("Restore access to the directory or free disk space"),
        "{remedy}"
    );
    assert!(
        remedy.ends_with("gcs-mimic sync rescan --endpoint URL"),
        "{remedy}"
    );
    running.task.rescan().await.unwrap();
    written_back(&running.catalog, &tree.0, &bucket).await;
    assert_eq!(
        running
            .root()
            .observation()
            .await
            .unwrap()
            .rejected_writeback,
        0
    );
    assert_eq!(
        wait_control(&running.task.rescan).await.reason,
        WaitReason::Ready
    );
    running.finish().await;
}

/// While the files of a rejected set are being written back, the status reads the directory
/// as changing, not as ready, and counts the files left; a wait answers once all are back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_status_counts_the_files_a_write_back_has_left() {
    let hook = member_hook();
    let (_data, tree, bucket, live) = awaiting_pair(Some(hook.clone())).await;
    reject_control(&live.commands, &live.root).await;
    reach_member(&hook).await;
    let observation = live.root.observation().await.unwrap();
    assert_eq!(observation.rejected_writeback, 2);
    assert_eq!(
        Outstanding::of_status(
            &observation,
            live.root.deletion_set().await.unwrap().as_ref()
        )
        .first(),
        Holdup::SourceChanging
    );
    hook.release.notify_one();
    let waited = released(&hook, wait_control(&live.commands)).await;
    assert_eq!(waited.reason, WaitReason::Ready);
    assert_eq!(live.root.observation().await.unwrap().rejected_writeback, 0);
    written_back(&live.catalog, &tree.0, &bucket).await;
    live.stop().await;
}

/// The mirror stops between two members of the write-back. The next start resumes it before
/// any inventory, and deletes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_back_stopped_part_way_resumes_at_the_next_start() {
    let hook = member_hook();
    let (data, tree, bucket, live) = awaiting_pair(Some(hook.clone())).await;
    reject_control(&live.commands, &live.root).await;
    reach_member(&hook).await;
    hook.release.notify_one();
    reach_member(&hook).await;
    live.stop.send_replace(true);
    hook.release.notify_one();
    live.stop().await;
    assert_eq!(fs::read(tree.0.join(BUCKET).join("first")).unwrap(), BODY);
    assert!(!tree.0.join(BUCKET).join("second").exists());

    let live = Live::start(&data.0, &tree.0).await;
    assert_eq!(wait_control(&live.commands).await.reason, WaitReason::Ready);
    written_back(&live.catalog, &tree.0, &bucket).await;
    live.stop().await;
}

/// Power fails while the second member's copy is written, after the rejection committed and
/// the first file was written back. The restart resumes the write-back from what the power
/// loss kept, and deletes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_power_loss_during_the_write_back_resumes_it_at_restart() {
    let (_data, tree, bucket, live) = awaiting_pair(None).await;
    let snapshots = TestDir::new();
    let (snapshot_data, snapshot_tree) = (snapshots.0.join("data"), snapshots.0.join("tree"));
    fault::arm(
        &tree.0,
        Step::SyncCopyWrite,
        1,
        Fault::PowerLoss(snapshot_tree.clone()),
    );
    live.catalog.inject(
        Step::SyncCopyWrite,
        1,
        Fault::Snapshot(snapshot_data.clone()),
    );
    reject_control(&live.commands, &live.root).await;
    assert_eq!(wait_control(&live.commands).await.reason, WaitReason::Ready);
    assert_eq!(fault::pending(&tree.0), 0);
    assert_eq!(live.catalog.pending_faults(), 0);
    live.stop().await;

    let targets = TRACKED.map(|name| format!("{BUCKET}/{name}"));
    restore_tree(
        &snapshot_tree,
        &tree.0,
        &targets.each_ref().map(String::as_str),
    );
    assert_eq!(fs::read(tree.0.join(BUCKET).join("first")).unwrap(), BODY);
    assert!(!tree.0.join(BUCKET).join("second").exists());
    let catalog = Catalog::open(&snapshot_data).await.unwrap();
    crate::storage::sync_state::writeback::retrack_restored(&catalog, &tree.0)
        .await
        .unwrap();
    catalog.close().await.unwrap();

    let live = Live::start(&snapshot_data, &tree.0).await;
    assert_eq!(wait_control(&live.commands).await.reason, WaitReason::Ready);
    written_back(&live.catalog, &tree.0, &bucket).await;
    live.stop().await;
}

/// A user puts a file at the first member's path after the write-back found it empty, just
/// before the rename that would put the copy there. The rename does not replace the file, the
/// write-back leaves that member to it and goes on with the second, and the file is taken in
/// as any file is.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_put_behind_the_write_back_stays_and_the_rest_are_written_back() {
    let (_data, tree, bucket, live) = awaiting_pair(None).await;
    let (catalog, root, commands) = (&live.catalog, &live.root, &live.commands);
    let (entered, reached) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    catalog.inject(
        Step::SyncCopyRename,
        0,
        Fault::Block {
            entered: std::sync::Arc::new(std::sync::Mutex::new(Some(entered))),
            release: std::sync::Arc::new(std::sync::Mutex::new(blocked)),
        },
    );
    reject_control(commands, root).await;
    tokio::time::timeout(HANG_GUARD, reached)
        .await
        .unwrap()
        .unwrap();
    let first = tree.0.join(BUCKET).join("first");
    fs::write(&first, USER).unwrap();
    release.send(()).unwrap();

    assert_eq!(wait_control(commands).await.reason, WaitReason::Ready);
    assert_eq!(fs::read(&first).unwrap(), USER);
    assert_eq!(fs::read(tree.0.join(BUCKET).join("second")).unwrap(), BODY);
    let taken = catalog
        .admin(Origin::Api)
        .get_object(&bucket, &ObjectName::new("first").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(taken.size, USER.len() as u64);
    assert!(exists(catalog, &bucket, "second").await);
    live.stop().await;
}

/// The write-back of a rejected set fails at its first member, which leaves the bucket
/// directory back and empty. The rescan once inventoried that as the same two files missing,
/// in the same journal epoch, and handed the rejected revision out again, which an approval
/// then ran. The rescan resumes the write-back instead, and the rejected revision stays used.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejected_revision_is_never_approved_after_a_failed_write_back() {
    let (data, tree, bucket) = tracked_pair().await;
    let running = Running::awaiting(&data.0, &tree.0).await;
    let rejected = running.reject_failing_at(0).await;
    running.task.rescan().await.unwrap();
    let current = running.root().deletion_set().await.unwrap();
    assert_ne!(current.map(|set| set.revision), Some(rejected.revision));
    let (reply, answer) = tokio::sync::oneshot::channel();
    running
        .task
        .rescan
        .send(Command::Decide {
            decision: Decision::Approve,
            revision: rejected.revision,
            reply,
            identity: None,
        })
        .await
        .unwrap();
    let answer = tokio::time::timeout(HANG_GUARD, answer)
        .await
        .unwrap()
        .unwrap();
    assert!(answer.is_err(), "{answer:?}");
    assert_eq!(
        wait_control(&running.task.rescan).await.reason,
        WaitReason::Ready
    );
    written_back(&running.catalog, &tree.0, &bucket).await;
    running.finish().await;
}
