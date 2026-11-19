//! What ends a `sync wait`, and what a successful one counts.

use super::*;
use crate::sync::control::{WaitReason, WaitResult};

/// The state of a live run whose baseline is complete and that replays the journal from `cursor`.
fn live_state(cursor: JournalFence) -> LiveState {
    LiveState {
        cursor,
        baseline: BaselineProgress {
            gate: BaselineGate::Allowed,
            complete: true,
        },
        skipped: SkippedExports::default(),
    }
}

/// Runs `run` on a control of a live run in `state`.
async fn with_control<T>(
    catalog: &Catalog,
    root: &RootLease,
    mut state: LiveState,
    run: impl AsyncFnOnce(&mut LiveControl<'_>) -> T,
) -> T {
    let config = Config::default();
    let (_sender, stopping) = tokio::sync::watch::channel(false);
    let writeback = root.default_writeback().await.unwrap();
    let mut control = LiveControl {
        scope: LiveScope {
            catalog,
            root,
            config: &config,
            writeback: &writeback,
            stopping: &stopping,
        },
        state: &mut state,
    };
    run(&mut control).await
}

/// Answers one `sync wait` of a live run in `state`.
async fn wait_in(catalog: &Catalog, root: &RootLease, state: LiveState) -> WaitResult {
    with_control(catalog, root, state, async |control| {
        control
            .wait(crate::test_support::hang_deadline())
            .await
            .unwrap()
    })
    .await
}

/// The baseline stays complete once reached, so an entry added to the tree afterwards that no
/// object name can hold must stop the wait by itself: the scan that met it, not the baseline,
/// says the tree is not reflected.
#[tokio::test]
async fn wait_stops_for_an_unrepresentable_entry_added_after_the_baseline() {
    type Add = fn(&std::path::Path);
    let additions: [(&str, Add); 2] = [
        ("symlink", |bucket| {
            std::os::unix::fs::symlink("object", bucket.join("link")).unwrap();
        }),
        ("backslash", |bucket| {
            fs::write(bucket.join(r"dir\file"), b"backslash").unwrap();
        }),
    ];
    for (label, add) in additions {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        catalog.enable_change_journal().await.unwrap();
        publish_origin(&catalog, &bucket, b"before", Origin::Api).await;
        assert!(
            reconcile(&catalog, &root).await.unwrap().baseline_complete,
            "{label}"
        );
        let cursor = catalog.current_fence().await.unwrap();
        add(&tree.0.join("test-bucket"));
        let result = wait_from(&catalog, &root, cursor).await;
        assert_eq!(result.reason, WaitReason::SourceUnrepresentable, "{label}");
        assert!(!result.completed, "{label}");
        drop(root);
        catalog.close().await.unwrap();
    }
}

/// An export skipped because the file it would replace kept changing runs again only once the
/// tree settles, so until then an API change is not in the tree. A retry that skips again during
/// the wait's own pass counts nothing, so the wait checks the skipped exports themselves; only a
/// race between the retry and a writer reaches that, hence the decision is driven directly.
#[tokio::test]
async fn wait_stops_while_a_skipped_export_waits_for_its_retry() {
    let (_data, _tree, catalog, bucket, root) = live_fixture().await;
    let api = publish_origin(&catalog, &bucket, b"api", Origin::Api).await;
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    let mut state = live_state(catalog.current_fence().await.unwrap());
    state.skipped.record(
        (bucket.clone(), ObjectName::new("object").unwrap()),
        Some(SkippedExport {
            api: Some(Generation::new(api.generation).unwrap()),
            tracked: None,
        }),
    );
    let reason = with_control(&catalog, &root, state, async |control| {
        control
            .observed_wait_reason(
                &Reconciliation {
                    baseline_complete: true,
                    ..Reconciliation::default()
                },
                crate::test_support::hang_deadline(),
            )
            .await
            .unwrap()
            .0
    })
    .await;
    assert_eq!(reason, WaitReason::SourceChanging);
    drop(root);
    catalog.close().await.unwrap();
}

/// Whether `directory` resolves names that differ only in case to the same entry.
fn folds_case(directory: &std::path::Path) -> bool {
    let probe = directory.join("case-probe");
    fs::write(&probe, b"").unwrap();
    let folds = directory.join("CASE-PROBE").exists();
    fs::remove_file(probe).unwrap();
    folds
}

/// When the API objects a wait brings to the tree arrive: before the baseline, so that the
/// wait's own pass reconciles them, or after it, so that the wait replays them from the journal.
#[derive(Clone, Copy, Debug)]
enum Arrival {
    BeforeBaseline,
    AfterBaseline,
}

/// Objects whose names the tree cannot hold stay reachable through the API, so a wait that
/// meets them succeeds and counts them, whether its own pass reconciles them before the
/// baseline or replays them after it. A name the tree ignores by design, such as `.DS_Store`,
/// is neither waited for nor counted. The count comes with the observation revision it was read
/// at, which binds the findings view that lists the objects.
#[tokio::test]
async fn wait_succeeds_past_names_the_tree_cannot_hold_and_counts_them() {
    use crate::storage::sync_state::findings::FindingEffect;
    let long = "l".repeat(256);
    for arrival in [Arrival::BeforeBaseline, Arrival::AfterBaseline] {
        for (label, names) in [
            ("backslash", vec![r"dir\file"]),
            ("ignored", vec!["x/.DS_Store"]),
            ("256-byte component", vec![long.as_str()]),
            ("file and directory", vec!["a", "a/b"]),
            ("case alias", vec!["Case", "case"]),
        ] {
            let (_data, tree, catalog, bucket, root) = live_fixture().await;
            catalog.enable_change_journal().await.unwrap();
            fs::create_dir(tree.0.join("test-bucket")).unwrap();
            // A file and a directory of one name hold each other out of the tree, and only a
            // tree that folds case aliases the two spellings.
            let expected = match label {
                "ignored" => 0,
                "file and directory" => 2,
                "case alias" if !folds_case(&tree.0.join("test-bucket")) => 0,
                _ => 1,
            };
            if matches!(arrival, Arrival::AfterBaseline) {
                assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
            }
            let cursor = catalog.current_fence().await.unwrap();
            for name in &names {
                publish_named(&catalog, &bucket, name, b"api", Origin::Api).await;
            }
            // Before the baseline, the wait's own pass reconciles every object before it
            // replays the journal.
            let baseline_complete = matches!(arrival, Arrival::AfterBaseline);
            let state = LiveState {
                baseline: BaselineProgress {
                    gate: BaselineGate::Allowed,
                    complete: baseline_complete,
                },
                ..live_state(cursor)
            };
            let result = wait_in(&catalog, &root, state).await;
            let context = format!("{label}, {arrival:?}");
            assert_eq!(result.reason, WaitReason::Ready, "{context}");
            assert!(result.completed, "{context}");
            let findings = result.findings.unwrap();
            assert_eq!(findings.unmapped, expected, "{context}");
            let observation = root.observation().await.unwrap();
            assert_eq!(
                findings.observation_revision, observation.revision,
                "{context}"
            );
            let reported = root
                .findings_page(0, 256)
                .await
                .unwrap()
                .iter()
                .filter(|finding| finding.reason.effect() == FindingEffect::Reports)
                .count();
            assert_eq!(reported as u64, expected, "{context}");
            drop(root);
            catalog.close().await.unwrap();
        }
    }
}

/// An export conflict holds an external edit that is in neither the API nor the tree's record,
/// so a name collision that a later generation of the same object meets must not replace it: the
/// collision only reports, and the wait would succeed with the edit unresolved. The API writes
/// the newer generation, and a child that makes its name a directory too, after the wait's scan
/// recorded the conflict and before it captures the fence it replays to.
#[tokio::test]
async fn wait_keeps_stopping_for_an_export_conflict_a_later_name_collision_meets() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    catalog.enable_change_journal().await.unwrap();
    publish_origin(&catalog, &bucket, b"before", Origin::Api).await;
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    publish_origin(&catalog, &bucket, b"newer", Origin::Api).await;
    let path = tree.0.join("test-bucket/object");
    fs::write(&path, b"edited").unwrap();
    let state = live_state(catalog.current_fence().await.unwrap());
    let hook = std::sync::Arc::new(Hook {
        phase: "wait-ingested",
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let result = with_control(&catalog, &root, state, async |control| {
        EXPORT_HOOK
            .scope(hook.clone(), async {
                let work = control.wait(crate::test_support::hang_deadline());
                tokio::pin!(work);
                tokio::select! {
                    result = &mut work => panic!("the wait ended before its scan finished: {:?}", result.map(|value| value.reason)),
                    entered = tokio::time::timeout(HANG_GUARD, hook.entered.notified()) => entered.unwrap(),
                }
                let findings = root.findings_page(0, 256).await.unwrap();
                assert_eq!(
                    findings.iter().map(|finding| finding.reason).collect::<Vec<_>>(),
                    [FindingKind::ExportConflict]
                );
                publish_origin(&catalog, &bucket, b"newest", Origin::Api).await;
                publish_named(&catalog, &bucket, "object/child", b"child", Origin::Api).await;
                hook.release.notify_one();
                work.await.unwrap()
            })
            .await
    })
    .await;
    assert_eq!(result.reason, WaitReason::FindingsPending);
    let findings = root.findings_page(0, 256).await.unwrap();
    assert!(
        findings.iter().any(
            |finding| finding.name == "object" && finding.reason == FindingKind::ExportConflict
        ),
        "{findings:?}"
    );
    assert_eq!(fs::read(&path).unwrap(), b"edited");
    drop(root);
    catalog.close().await.unwrap();
}

/// An object a name kept out of the tree stops counting once the API deletes it: the replay of
/// the deletion removes its finding, whether no path could hold the name or the path collides
/// with another object, so the next wait counts only the objects that remain.
#[tokio::test]
async fn wait_stops_counting_objects_the_api_deleted() {
    let long = "l".repeat(256);
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    catalog.enable_change_journal().await.unwrap();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
    let cursor = catalog.current_fence().await.unwrap();
    let mut published = Vec::new();
    for name in [r"dir\file", "a", "a/b", long.as_str()] {
        let record = publish_named(&catalog, &bucket, name, b"api", Origin::Api).await;
        published.push((name, record.generation));
    }
    let first = wait_from(&catalog, &root, cursor).await;
    assert_eq!(
        (first.reason, first.findings.map(|counted| counted.unmapped)),
        (WaitReason::Ready, Some(4))
    );
    let cursor = catalog.current_fence().await.unwrap();
    for (name, generation) in &published[..3] {
        catalog
            .admin(Origin::Api)
            .delete_object(
                &bucket,
                &ObjectName::new(*name).unwrap(),
                GenerationCondition::Is(Generation::new(*generation).unwrap()),
            )
            .await
            .unwrap();
    }
    let second = wait_from(&catalog, &root, cursor).await;
    assert_eq!(
        (
            second.reason,
            second.findings.map(|counted| counted.unmapped)
        ),
        (WaitReason::Ready, Some(1))
    );
    let findings = root.findings_page(0, 256).await.unwrap();
    assert_eq!(
        findings
            .iter()
            .map(|finding| finding.name.as_str())
            .collect::<Vec<_>>(),
        [long.as_str()]
    );
    drop(root);
    catalog.close().await.unwrap();
}

/// The external edit a scan meets, against an API change that the journal cursor has already
/// passed, so that only the export conflict the scan records stands between the wait and Ready.
#[derive(Clone, Copy, Debug)]
enum ExportConflict {
    /// The API holds a newer generation than the tree tracks.
    NewerGeneration,
    /// The API deleted the object the tree tracks.
    Deleted,
}

/// Each way the scan records an export conflict stops the wait: an edited file whose API object
/// moved to a newer generation, and one whose API object was deleted. Both leave the edit as it
/// was, and nothing else is pending.
#[tokio::test]
async fn wait_stops_for_either_export_conflict_a_scan_records() {
    for conflict in [ExportConflict::NewerGeneration, ExportConflict::Deleted] {
        let (_data, tree, catalog, bucket, root) = live_fixture().await;
        catalog.enable_change_journal().await.unwrap();
        let exported = publish_origin(&catalog, &bucket, b"before", Origin::Api).await;
        assert!(reconcile(&catalog, &root).await.unwrap().baseline_complete);
        match conflict {
            ExportConflict::NewerGeneration => {
                publish_origin(&catalog, &bucket, b"newer", Origin::Api).await;
            }
            ExportConflict::Deleted => {
                catalog
                    .admin(Origin::Api)
                    .delete_object(
                        &bucket,
                        &ObjectName::new("object").unwrap(),
                        GenerationCondition::Is(Generation::new(exported.generation).unwrap()),
                    )
                    .await
                    .unwrap();
            }
        }
        let path = tree.0.join("test-bucket/object");
        fs::write(&path, b"edited").unwrap();
        let result = wait_from(&catalog, &root, catalog.current_fence().await.unwrap()).await;
        assert_eq!(result.reason, WaitReason::FindingsPending, "{conflict:?}");
        let observation = root.observation().await.unwrap();
        assert_eq!(
            (
                observation.blocking_findings,
                observation.unresolved_conflicts,
                observation.path_stops,
                observation.pending_writeback
            ),
            (1, 0, 0, 0),
            "{conflict:?}"
        );
        let findings = root.findings_page(0, 256).await.unwrap();
        assert_eq!(findings.len(), 1, "{conflict:?}");
        assert_eq!(
            findings[0].reason,
            FindingKind::ExportConflict,
            "{conflict:?}"
        );
        assert_eq!(fs::read(&path).unwrap(), b"edited", "{conflict:?}");
        drop(root);
        catalog.close().await.unwrap();
    }
}
