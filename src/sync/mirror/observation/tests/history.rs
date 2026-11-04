#![expect(
    clippy::unwrap_used,
    reason = "fixture failures must fail at their source in platform-gated tests"
)]
use super::*;
use crate::test_support::TestDir;
use std::fs;

struct Fixture {
    _data: TestDir,
    tree: TestDir,
    catalog: Catalog,
    root: RootLease,
    config: Config,
    writeback: Writeback,
}
impl Fixture {
    async fn new() -> Self {
        let (data, tree, catalog, root, config) = crate::sync::test_support::fixture(None).await;
        fs::create_dir(tree.0.join("test-bucket")).unwrap();
        for index in 0..120 {
            fs::write(tree.0.join(format!("test-bucket/{index:03}")), b"body").unwrap();
        }
        catalog.enable_change_journal().await.unwrap();
        let writeback = root.default_writeback().await.unwrap();
        let mut fixture = Self {
            _data: data,
            tree,
            catalog,
            root,
            config,
            writeback,
        };
        fixture.scan().await;
        for index in 0..60 {
            fs::remove_file(fixture.tree.0.join(format!("test-bucket/{index:03}"))).unwrap();
        }
        fixture.scan().await;
        fixture.scan().await;
        assert_eq!(fixture.root.automatic_deletion_count().await.unwrap(), 60);
        assert_eq!(
            fixture
                .root
                .deletion_set()
                .await
                .unwrap()
                .unwrap()
                .candidate_count,
            0
        );
        fixture
    }
    async fn scan(&mut self) {
        let (_stop, stopping) = tokio::sync::watch::channel(false);
        let mut gate = BaselineGate::Allowed;
        let mut baseline = self.root.observation().await.unwrap().baseline_complete;
        ingest_external(
            &self.catalog,
            &self.root,
            &self.config,
            &self.writeback,
            &stopping,
            &mut gate,
            &mut baseline,
        )
        .await
        .unwrap();
    }
    async fn quiet(&self) -> DirectoryWatch {
        let mut watcher = DirectoryWatch::new(&self.root).await.unwrap();
        watcher.continue_quiet();
        assert!(matches!(
            watcher.next_observation().await.unwrap(),
            Observed::Settled(_, Quiet::Established)
        ));
        watcher
    }
    async fn close(self) {
        drop(self.writeback);
        drop(self.root);
        self.catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn changes_during_a_scan_preserve_history_and_guard_the_next_deletion_wave() {
    let mut fixture = Fixture::new().await;
    let mut watcher = fixture.quiet().await;
    let witness = watcher.watch.notification_witness().unwrap();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let mut gate = BaselineGate::Allowed;
    let mut baseline = true;
    let mut cursor = fixture
        .catalog
        .read_changes(None, None, 1)
        .await
        .unwrap()
        .fence;
    let mut control = LiveControl {
        catalog: &fixture.catalog,
        root: &fixture.root,
        config: &fixture.config,
        writeback: &fixture.writeback,
        stopping: &stopping,
        ingest_complete: &mut gate,
        baseline_complete: &mut baseline,
        cursor: &mut cursor,
    };
    let hook = std::sync::Arc::new(crate::sync::Hook {
        phase: "history-scanned",
        entered: Default::default(),
        release: Default::default(),
    });
    let (reply, response) = tokio::sync::oneshot::channel();
    let (result, ()) = EXPORT_HOOK
        .scope(hook.clone(), async {
            tokio::join!(
                control.handle(
                    Command::Rescan {
                        reply,
                        identity: None
                    },
                    Quiet::Established,
                    &mut watcher
                ),
                async {
                    hook.entered.notified().await;
                    for index in 60..120 {
                        fs::remove_file(fixture.tree.0.join(format!("test-bucket/{index:03}")))
                            .unwrap();
                    }
                    let ready = tokio::time::timeout(Duration::from_secs(5), witness.readable())
                        .await
                        .unwrap()
                        .unwrap();
                    drop(ready);
                    hook.release.notify_one();
                }
            )
        })
        .await;
    result.unwrap();
    assert!(response.await.unwrap().unwrap().baseline_complete);
    assert_eq!(fixture.root.automatic_deletion_count().await.unwrap(), 60);
    assert!(watcher.pending.is_some());
    fixture.scan().await;
    let set = fixture.root.deletion_set().await.unwrap().unwrap();
    assert!(set.guarded);
    assert_eq!(set.candidate_count, 60);
    assert_eq!(
        fixture.root.observation().await.unwrap().tracked_objects,
        60
    );
    drop(watcher);
    fixture.close().await;
}

#[tokio::test]
async fn queued_native_events_cannot_reuse_an_old_quiet_flag_for_history_reset() {
    let fixture = Fixture::new().await;
    let mut watcher = fixture.quiet().await;
    let witness = watcher.watch.notification_witness().unwrap();
    fs::write(fixture.tree.0.join("test-bucket/.DS_Store"), b"changed").unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(5), witness.readable())
        .await
        .unwrap()
        .unwrap();
    drop(ready);
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let mut gate = BaselineGate::Allowed;
    let mut baseline = true;
    let mut cursor = fixture
        .catalog
        .read_changes(None, None, 1)
        .await
        .unwrap()
        .fence;
    let mut control = LiveControl {
        catalog: &fixture.catalog,
        root: &fixture.root,
        config: &fixture.config,
        writeback: &fixture.writeback,
        stopping: &stopping,
        ingest_complete: &mut gate,
        baseline_complete: &mut baseline,
        cursor: &mut cursor,
    };
    let (reply, response) = tokio::sync::oneshot::channel();
    control
        .handle(
            Command::Rescan {
                reply,
                identity: None,
            },
            Quiet::Established,
            &mut watcher,
        )
        .await
        .unwrap();
    assert!(response.await.unwrap().unwrap().baseline_complete);
    assert_eq!(fixture.root.automatic_deletion_count().await.unwrap(), 60);
    assert!(watcher.pending.is_some());
    drop(watcher);
    fixture.close().await;
}

#[tokio::test]
async fn history_reset_failure_keeps_its_cause_and_answers_wait_and_rescan() {
    let fixture = Fixture::new().await;
    let mut watcher = fixture.quiet().await;
    fixture.root.reject_deletion_history_reset().await.unwrap();
    let summary = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let error = watcher
        .history_scan(Quiet::Established)
        .reset(&fixture.root, &summary)
        .await
        .unwrap_err();
    assert!(std::error::Error::source(&error).is_some());
    assert!(
        crate::error::Report(&error)
            .to_string()
            .contains("history reset blocked")
    );
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let mut gate = BaselineGate::Allowed;
    let mut baseline = true;
    let mut cursor = fixture
        .catalog
        .read_changes(None, None, 1)
        .await
        .unwrap()
        .fence;
    let mut control = LiveControl {
        catalog: &fixture.catalog,
        root: &fixture.root,
        config: &fixture.config,
        writeback: &fixture.writeback,
        stopping: &stopping,
        ingest_complete: &mut gate,
        baseline_complete: &mut baseline,
        cursor: &mut cursor,
    };
    let (reply, response) = tokio::sync::oneshot::channel();
    control
        .handle(
            Command::Wait {
                deadline: tokio::time::Instant::now() + Duration::from_secs(10),
                settling: Default::default(),
                reply,
                identity: None,
            },
            Quiet::Established,
            &mut watcher,
        )
        .await
        .unwrap();
    assert_eq!(
        response.await.unwrap().unwrap().reason,
        crate::sync::control::WaitReason::Ready
    );
    let (reply, response) = tokio::sync::oneshot::channel();
    control
        .handle(
            Command::Rescan {
                reply,
                identity: None,
            },
            Quiet::Established,
            &mut watcher,
        )
        .await
        .unwrap();
    assert!(response.await.unwrap().unwrap().baseline_complete);
    assert_eq!(fixture.root.automatic_deletion_count().await.unwrap(), 60);
    assert!(
        fixture
            .root
            .observation()
            .await
            .unwrap()
            .paused_reason
            .is_none()
    );
    drop(watcher);
    fixture.close().await;
}

#[tokio::test]
async fn explicit_rescan_requires_verified_quiet_before_clearing_history() {
    let fixture = Fixture::new().await;
    let mut watcher = fixture.quiet().await;
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let mut gate = BaselineGate::Allowed;
    let mut baseline = true;
    let mut cursor = fixture
        .catalog
        .read_changes(None, None, 1)
        .await
        .unwrap()
        .fence;
    let mut control = LiveControl {
        catalog: &fixture.catalog,
        root: &fixture.root,
        config: &fixture.config,
        writeback: &fixture.writeback,
        stopping: &stopping,
        ingest_complete: &mut gate,
        baseline_complete: &mut baseline,
        cursor: &mut cursor,
    };
    let (reply, response) = tokio::sync::oneshot::channel();
    control
        .handle(
            Command::Rescan {
                reply,
                identity: None,
            },
            Quiet::Unverified,
            &mut watcher,
        )
        .await
        .unwrap();
    assert!(response.await.unwrap().unwrap().baseline_complete);
    assert_eq!(fixture.root.automatic_deletion_count().await.unwrap(), 60);
    let (reply, response) = tokio::sync::oneshot::channel();
    control
        .handle(
            Command::Rescan {
                reply,
                identity: None,
            },
            Quiet::Established,
            &mut watcher,
        )
        .await
        .unwrap();
    assert!(response.await.unwrap().unwrap().baseline_complete);
    assert_eq!(fixture.root.automatic_deletion_count().await.unwrap(), 0);
    drop(watcher);
    fixture.close().await;
}
