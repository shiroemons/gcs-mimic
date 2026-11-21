//! A decision whose inventory an API change or a stop cuts short. The inventory before a
//! decision compares the set's members with the tree at the current journal fence; an API write
//! during it, or a stop, leaves the set uncompared, so the decision is refused without being
//! recorded, and the refusal says what to do instead. An update of the members' bucket replaces
//! the set at that inventory, which then ends with no set waiting until a pass finishes the new
//! one.
#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::baseline::exists;
use super::missing_bucket::{
    ApiWrite, BUCKET, Live, TRACKED, decide, released, tracked_pair, wait_control,
};
use super::*;
use crate::storage::sync_state::{
    OpaqueId,
    deletions::{DeletionAuthorization, DeletionSet, Refusal},
};
use crate::sync::WaitReason;

type Answer = tokio::sync::oneshot::Receiver<Result<DeletionSet, ControlError>>;

/// Stops the mirror at each member an inventory compares.
fn candidate_hook() -> std::sync::Arc<Hook> {
    std::sync::Arc::new(Hook {
        phase: "deletion-candidate",
        entered: Default::default(),
        release: Default::default(),
    })
}

/// A mirror over the tracked pair whose bucket directory went, with the set that waits for a
/// decision on both files.
struct Waiting {
    data: TestDir,
    tree: TestDir,
    bucket: BucketName,
    hook: std::sync::Arc<Hook>,
    live: Live,
    revision: OpaqueId,
}

impl Waiting {
    async fn start() -> Self {
        let (data, tree, bucket) = tracked_pair().await;
        fs::remove_dir_all(tree.0.join(BUCKET)).unwrap();
        let hook = candidate_hook();
        let live = Live::start_with(&data.0, &tree.0, Some(hook.clone())).await;
        let revision = Self::awaited(&live, &hook).await.revision;
        Self {
            data,
            tree,
            bucket,
            hook,
            live,
            revision,
        }
    }

    /// The set a wait ends on, which waits for a decision.
    async fn awaited(live: &Live, hook: &Hook) -> DeletionSet {
        let waited = released(hook, wait_control(&live.commands)).await;
        assert_eq!(waited.reason, WaitReason::DeletionApproval);
        let set = live.root.deletion_set().await.unwrap().unwrap();
        assert_eq!((set.candidate_count, set.guarded), (2, true));
        set
    }

    /// Sends `decision` on `revision`, and holds the mirror at the first member the inventory
    /// before it compares.
    async fn held(&self, decision: Decision, revision: OpaqueId) -> Answer {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.live
            .commands
            .send(decide(decision, revision, reply))
            .await
            .unwrap();
        tokio::time::timeout(HANG_GUARD, self.hook.entered.notified())
            .await
            .unwrap();
        answer
    }

    /// Lets the held mirror go on, and returns its answer.
    async fn answered(&self, answer: Answer) -> Result<DeletionSet, ControlError> {
        self.hook.release.notify_one();
        released(&self.hook, async {
            tokio::time::timeout(HANG_GUARD, answer).await.unwrap()
        })
        .await
        .unwrap()
    }

    /// Sends `decision` on `revision` and returns the answer, letting the mirror past every
    /// member.
    async fn decided(
        &self,
        decision: Decision,
        revision: OpaqueId,
    ) -> Result<DeletionSet, ControlError> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.live
            .commands
            .send(decide(decision, revision, reply))
            .await
            .unwrap();
        released(&self.hook, async {
            tokio::time::timeout(HANG_GUARD, answer).await.unwrap()
        })
        .await
        .unwrap()
    }

    async fn every_object_remains(&self) {
        for name in TRACKED {
            assert!(
                exists(&self.live.catalog, &self.bucket, name).await,
                "{name}"
            );
        }
    }

    async fn stop(self) -> (TestDir, TestDir, BucketName, std::sync::Arc<Hook>) {
        released(&self.hook, self.live.stop()).await;
        (self.data, self.tree, self.bucket, self.hook)
    }
}

/// The refusal a stale decision answered with, and its text.
fn stale(answer: Result<DeletionSet, ControlError>, decision: Decision) -> (Refusal, String) {
    match answer {
        Err(
            error @ ControlError::Stale {
                decision: refused,
                refusal,
                ..
            },
        ) if refused == decision => (refusal, error.to_string()),
        answer => panic!("{decision:?}: {answer:?}"),
    }
}

/// An API write that touches no member, made while the inventory before a decision compares the
/// members, refuses the decision without recording it: the set still waits under its revision,
/// no object goes, and the answer asks for the same decision again, which is then accepted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_api_write_during_the_recheck_refuses_the_decision_until_it_is_sent_again() {
    for decision in [Decision::Approve, Decision::Reject] {
        let waiting = Waiting::start().await;
        let revision = waiting.revision;
        let answer = waiting.held(decision, revision).await;
        ApiWrite::Beside
            .make(&waiting.live.catalog, &waiting.bucket)
            .await;
        let (refusal, text) = stale(waiting.answered(answer).await, decision);
        assert_eq!(refusal, Refusal::Moved, "{decision:?}");
        let again = format!(
            "send it again: gcs-mimic sync {} --deletion-set {revision} --endpoint URL",
            decision.as_str()
        );
        assert!(text.ends_with(&again), "{text}");
        let kept = waiting.live.root.deletion_set().await.unwrap().unwrap();
        assert_eq!(
            (kept.revision, kept.authorization),
            (revision, DeletionAuthorization::Withheld),
            "{decision:?}"
        );
        waiting.every_object_remains().await;

        let accepted = waiting.decided(decision, revision).await.unwrap();
        assert_eq!(accepted.revision, revision, "{decision:?}");
        let waited = released(&waiting.hook, wait_control(&waiting.live.commands)).await;
        assert_eq!(waited.reason, WaitReason::Ready, "{decision:?}");
        for name in TRACKED {
            let kept = exists(&waiting.live.catalog, &waiting.bucket, name).await;
            assert_eq!(kept, decision == Decision::Reject, "{decision:?}: {name}");
        }
        waiting.stop().await;
    }
}

/// A decision after an update of the members' bucket replaces the set at its inventory, and a
/// stop ends that inventory. The decision is refused as a shutdown, the named set is gone and
/// no set waits, so nothing can run; every object remains. The next start takes a new set,
/// and the old revision is then refused with that set's counts: a refused approval names no
/// approval, and a refused rejection names the rejection of the new set.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stop_while_a_bucket_update_replaces_the_set_leaves_nothing_to_run() {
    for decision in [Decision::Approve, Decision::Reject] {
        let waiting = Waiting::start().await;
        let revision = waiting.revision;
        ApiWrite::BucketUpdate
            .make(&waiting.live.catalog, &waiting.bucket)
            .await;
        let answer = waiting.held(decision, revision).await;
        waiting.live.stop.send_replace(true);
        let answer = waiting.answered(answer).await;
        assert!(
            matches!(answer, Err(ControlError::Shutdown)),
            "{decision:?}: {answer:?}"
        );
        assert!(waiting.live.root.deletion_set().await.unwrap().is_none());
        assert_eq!(
            waiting.live.root.refusal(revision).await.unwrap(),
            Refusal::Settled
        );
        waiting.every_object_remains().await;
        let (data, tree, bucket, hook) = waiting.stop().await;

        let live = Live::start_with(&data.0, &tree.0, Some(hook.clone())).await;
        let next = Waiting::awaited(&live, &hook).await;
        assert_ne!(next.revision, revision);
        let waiting = Waiting {
            data,
            tree,
            bucket,
            hook,
            live,
            revision: next.revision,
        };
        let (refusal, text) = stale(waiting.decided(decision, revision).await, decision);
        assert_eq!(
            refusal,
            Refusal::Replaced {
                revision: next.revision,
                missing: 2,
                tracked: next.baseline_count,
            }
        );
        let next = next.revision.to_hex();
        assert!(
            text.contains(&format!("Deletion set {next} waits instead")),
            "{text}"
        );
        assert!(!text.contains("gcs-mimic sync approve"), "{text}");
        assert_eq!(
            text.contains(&format!("gcs-mimic sync reject --deletion-set {next}")),
            decision == Decision::Reject,
            "{text}"
        );
        waiting.every_object_remains().await;
        waiting.stop().await;
    }
}

/// A decision after an update of the members' bucket replaces the set at its inventory, and an
/// API write during that inventory supersedes it. The decision is refused without being
/// recorded; the named set is gone, and the answer says no set waits yet and names the wait,
/// whose pass takes the new set. Every object remains.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_api_write_while_a_bucket_update_replaces_the_set_refuses_the_decision() {
    for decision in [Decision::Approve, Decision::Reject] {
        let waiting = Waiting::start().await;
        let revision = waiting.revision;
        ApiWrite::BucketUpdate
            .make(&waiting.live.catalog, &waiting.bucket)
            .await;
        let answer = waiting.held(decision, revision).await;
        ApiWrite::Beside
            .make(&waiting.live.catalog, &waiting.bucket)
            .await;
        let (refusal, text) = stale(waiting.answered(answer).await, decision);
        assert_eq!(refusal, Refusal::Settled, "{decision:?}");
        for part in [
            "no deletion set waits for a decision now",
            "or the inventory that replaces the set has not finished",
            "gcs-mimic sync wait --endpoint URL",
        ] {
            assert!(text.contains(part), "{text}");
        }
        assert!(waiting.live.root.deletion_set().await.unwrap().is_none());
        waiting.every_object_remains().await;

        let next = Waiting::awaited(&waiting.live, &waiting.hook).await;
        assert_ne!(next.revision, revision);
        waiting.every_object_remains().await;
        waiting.stop().await;
    }
}
