//! External changes to an exported file after the rename that installed it and before the
//! export acknowledged it.

use super::install_faults::{Started, conflict_bodies};
use super::*;
use crate::storage::sync_state::authority::ControlAuthority;
use crate::storage::sync_state::observation::{ObservationCursor, PauseReason};
use crate::storage::sync_state::{Ordinal, PageLimit};
use crate::sync::WaitReason;

/// What the user does to the file an export installed.
#[derive(Clone, Copy, Debug)]
enum External {
    Delete,
    Edit,
    /// Changes only the modification time, as `touch` does.
    Touch,
}

impl External {
    fn apply(self, path: &Path) {
        match self {
            Self::Delete => fs::remove_file(path).unwrap(),
            Self::Edit => fs::write(path, b"edited").unwrap(),
            Self::Touch => fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000))
                .unwrap(),
        }
    }

    /// The conflicts retained once the export is settled: only an edit holds bytes nobody else
    /// has.
    fn retained(self) -> Vec<Vec<u8>> {
        match self {
            Self::Edit => vec![b"edited".to_vec()],
            Self::Delete | Self::Touch => Vec::new(),
        }
    }
}

/// A deletion, edit or touch of the file an export installed, before the export records it
/// durably or acknowledges it, fails the export when the export checks the file again. The
/// mirror pauses for that failure as an observer failure, whose remedy is a rescan, and the
/// operation stays armed or durable: write-back reports `RecoveryRequired` and a write-back
/// finding without recovery details, which `sync resolve --recovery` has nothing to select in and
/// nothing to do for. The run the rescan starts cancels the operation without changing the
/// target and reconciles the target again: it keeps an edit as a registered conflict and installs
/// the API body over it, exports the API body again over a deletion of a file the tree never
/// acknowledged, and tracks a touched file, while the API keeps the object it wrote. A touch
/// before the durable record changes nothing the export checks, so that export completes.
#[tokio::test]
async fn an_external_change_before_the_acknowledgement_is_settled_by_a_rescan() {
    for (checkpoint, external, left) in [
        (
            "before-durable",
            External::Delete,
            Some(writeback::Phase::Armed),
        ),
        (
            "before-durable",
            External::Edit,
            Some(writeback::Phase::Armed),
        ),
        ("before-durable", External::Touch, None),
        (
            "before-ack",
            External::Delete,
            Some(writeback::Phase::Durable),
        ),
        (
            "before-ack",
            External::Edit,
            Some(writeback::Phase::Durable),
        ),
        (
            "before-ack",
            External::Touch,
            Some(writeback::Phase::Durable),
        ),
    ] {
        for overwrite in [false, true] {
            let case = format!("{checkpoint} {external:?} overwrite={overwrite}");
            let (data, tree, catalog, bucket, _) = live_fixture().await;
            if overwrite {
                publish_origin(&catalog, &bucket, b"old", Origin::Api).await;
            }
            let mut started = Started::start(catalog, &data.0, &tree.0).await;
            let path = tree.0.join("test-bucket/object");
            let api = publish_origin(&started.catalog, &bucket, b"api", Origin::Api).await;
            let hook = std::sync::Arc::new(Hook {
                phase: checkpoint,
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            });
            let (waited, ()) =
                tokio::join!(EXPORT_HOOK.scope(hook.clone(), started.wait()), async {
                    tokio::time::timeout(HANG_GUARD, hook.entered.notified())
                        .await
                        .unwrap();
                    external.apply(&path);
                    hook.release.notify_one();
                });
            if let Some(phase) = left {
                let Err(error) = waited else {
                    panic!("{case}: the export completed");
                };
                assert_eq!(error.pause_reason(), PauseReason::ObserverFailure, "{case}");
                stops_for_recovery(&started, phase, &case).await;
                started = started.rerun().await;
                let expected = match external {
                    External::Edit => WaitReason::FindingsPending,
                    External::Delete | External::Touch => WaitReason::Ready,
                };
                assert_eq!(started.wait().await.unwrap().reason, expected, "{case}");
            } else {
                assert_eq!(waited.unwrap().reason, WaitReason::Ready, "{case}");
            }
            let writeback = started.root.default_writeback().await.unwrap();
            assert_eq!(
                writeback.status().await.unwrap().stop(),
                Stop::Ready,
                "{case}"
            );
            drop(writeback);
            assert_eq!(fs::read(&path).unwrap(), b"api", "{case}");
            assert_eq!(conflict_bodies(&data.0), external.retained(), "{case}");
            assert_eq!(
                tracked_generation(&started.catalog, &started.root, &bucket).await,
                Some(api.generation),
                "{case}"
            );
            let kept = started
                .catalog
                .admin(Origin::Api)
                .get_object(&bucket, &ObjectName::new("object").unwrap(), None)
                .await
                .unwrap();
            assert_eq!(kept, api, "{case}");
            started.close().await;
        }
    }
}

/// Checks that write-back stopped with the one operation of the failed export left in `phase`,
/// listed as a finding that no recovery discard can select.
async fn stops_for_recovery(started: &Started, phase: writeback::Phase, case: &str) {
    let writeback = started.root.default_writeback().await.unwrap();
    assert_eq!(
        writeback.status().await.unwrap().stop(),
        Stop::RecoveryRequired,
        "{case}"
    );
    let pending = writeback.page(0, 256).await.unwrap();
    assert_eq!(
        pending.iter().map(|item| item.phase).collect::<Vec<_>>(),
        [phase],
        "{case}"
    );
    let revision = started.root.observation().await.unwrap().revision;
    let findings = started
        .root
        .observation_page(
            revision,
            ObservationCursor::START,
            PageLimit::try_from(256).unwrap(),
        )
        .await
        .unwrap();
    let writebacks = findings
        .iter()
        .filter(|item| item.category.as_str() == "writeback")
        .collect::<Vec<_>>();
    assert_eq!(writebacks.len(), 1, "{case}");
    assert!(writebacks[0].recovery.is_none(), "{case}");
    let recoveries = started
        .root
        .recovery_page(
            Ordinal::START,
            PageLimit::LARGEST,
            ControlAuthority::Permissive,
        )
        .await
        .unwrap();
    assert!(recoveries.is_empty(), "{case}");
}
