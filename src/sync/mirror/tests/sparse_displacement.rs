//! A tracked file replaced by a sparse body larger than any conflict quota after an export's last
//! check of its target and before the exchange that installs the copy. Replaced any earlier, the
//! file fails that check and stays in the tree; in this window the exchange moves it into the
//! private workspace, where recovery keeps it whole, and the root stays stopped until the
//! operator discards it with confirmation. The body is inspected by its metadata only: reading
//! 1 TiB would take the test hours.

use super::install_race::{Held, still_running, wait_racing};
use super::*;
use crate::storage::fault::Step;
use crate::storage::sync_state::authority::ControlAuthority;
use crate::storage::sync_state::conflicts::{
    Action, Confirmation, RecoveryTarget, ResolveRequest, ResolveTarget,
};
use crate::storage::sync_state::writeback::Stop;
use crate::storage::sync_state::{Ordinal, PageLimit};
use crate::sync::test_support::operation_entries;
use crate::sync::{WaitReason, control::ControlError};

/// One byte past the most the conflict quota can hold.
const SPARSE: u64 = (1 << 40) + 1;

/// The write-back entries, retained bytes and stop of `root`.
async fn usage(root: &RootLease) -> (u64, u128, Stop) {
    let writeback = root.default_writeback().await.unwrap();
    let status = writeback.status().await.unwrap();
    (status.entries, status.retained_bytes, status.stop())
}

/// The lengths of the private workspace entries.
fn workspace_lengths(tree: &Path) -> Vec<u64> {
    operation_entries(&tree.join(mapping::WORKSPACE))
        .iter()
        .map(|entry| fs::metadata(entry).unwrap().len())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_sparse_body_an_exchange_displaced_stops_the_root_until_a_confirmed_discard() {
    let (_data, tree, catalog, task) = started_mirror(None).await;
    task.rescan().await.unwrap();
    let (control, root) = catalog.sync_controls().unwrap().unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    publish_named(&catalog, &bucket, "object", b"before", Origin::Api).await;
    let reply = queue_wait(&task, HANG_GUARD).await;
    let ready = tokio::time::timeout(HANG_GUARD, reply).await.unwrap();
    assert_eq!(ready.unwrap().unwrap().reason, WaitReason::Ready);
    let target = tree.0.join("test-bucket/object");
    let held = Held::arm(&tree.0, Step::SyncCopyRename);
    publish_named(&catalog, &bucket, "object", b"after", Origin::Api).await;
    let paused = wait_racing(&task, held, || {
        let file = fs::OpenOptions::new().write(true).open(&target).unwrap();
        file.set_len(SPARSE).unwrap();
    })
    .await;
    assert_eq!(paused.reason, WaitReason::Paused);
    // The API copy took the target, and the exchange moved the whole body out of it, which
    // the quota accounts in full although no conflict could hold it.
    assert_eq!(fs::read(&target).unwrap(), b"after");
    assert_eq!(workspace_lengths(&tree.0), [SPARSE]);
    let stopped = (1, u128::from(SPARSE), Stop::ConflictCapacity);
    assert_eq!(usage(&root).await, stopped);
    assert_eq!(
        root.observation().await.unwrap().paused_reason,
        Some(PauseReason::RecoveryRequired)
    );
    let page = root
        .recovery_page(
            Ordinal::START,
            PageLimit::LARGEST,
            ControlAuthority::Permissive,
        )
        .await
        .unwrap();
    let [recovery] = page.as_slice() else {
        panic!("{page:?}");
    };
    assert_eq!(recovery.retained_size, SPARSE);
    assert!(recovery.over_limit);
    assert!(recovery.hashes.is_none());
    // Nothing but the discard frees the quota: a rescan alone stays stopped and keeps the body.
    assert!(matches!(
        task.rescan().await,
        Err(ControlError::RescanFailed {
            reason: PauseReason::ConflictCapacity,
            ..
        })
    ));
    assert_eq!(workspace_lengths(&tree.0), [SPARSE]);
    assert_eq!(usage(&root).await, stopped);
    let (reply, resolved) = tokio::sync::oneshot::channel();
    let request = ResolveRequest {
        operation_id: "discard-sparse".into(),
        action: Action::Discard,
        target: ResolveTarget::Recovery(RecoveryTarget {
            recovery_id: recovery.recovery_id,
            revision: recovery.recovery_revision,
            target_identity: recovery.target_identity,
            confirmation: Confirmation::DiscardWithoutBackup,
        }),
    };
    let identity = None;
    let resolve = Command::Resolve {
        request,
        reply,
        identity,
    };
    control.sender.try_send(resolve).unwrap();
    let outcome = tokio::time::timeout(HANG_GUARD, resolved).await.unwrap();
    assert!(outcome.unwrap().unwrap().completed);
    task.rescan().await.unwrap();
    still_running(&task, &root).await;
    assert!(workspace_lengths(&tree.0).is_empty());
    assert_eq!(usage(&root).await, (0, 0, Stop::Ready));
    assert_eq!(fs::read(&target).unwrap(), b"after");
    drop((control, root));
    task.finish().await.unwrap();
    catalog.close().await.unwrap();
}
