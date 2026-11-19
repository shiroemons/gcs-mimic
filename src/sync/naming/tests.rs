#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]

#[cfg(target_os = "linux")]
use super::probe::FOLDED;
use super::probe::{DIRECTORY, UNLINKED, inject};
use super::*;
use crate::{
    config::Config,
    object::UploadChunk,
    storage::{
        Catalog,
        admin::{BucketName, ObjectName, Origin, Publication, PublishMode},
        fault::{self, Crash, Fault, Step},
        sync_state::{
            OpaqueId,
            findings::FindingKind,
            root::{RootError, RootLease},
            writeback::Stop,
        },
    },
    sync::{
        MirrorState, Reconciliation,
        mapping::{self, Reason},
        test_support::{probed, unprobed_fixture},
    },
    test_support::TestDir,
};
use std::{fs, path::Path, time::Duration};

fn observed(colon: Colon, case: Case, spelling: Spelling) -> Observation {
    Observation {
        colon,
        case,
        spelling,
        name_max: NameMax::from(Some(255)),
    }
}

fn bucket() -> BucketName {
    BucketName::new("test-bucket").unwrap()
}

/// Names left in the probe directory, sorted.
fn probe_entries(tree: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(tree.join(mapping::WORKSPACE).join(DIRECTORY))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

async fn publish(catalog: &Catalog, name: &str) {
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(bytes::Bytes::from_static(b"body"))))
        .unwrap();
    sender.try_send(Ok(UploadChunk::End)).unwrap();
    catalog
        .admin(Origin::Api)
        .publish_object(
            &bucket(),
            &ObjectName::new(name).unwrap(),
            Publication {
                content_type: "application/octet-stream".into(),
                metadata: Default::default(),
                expected: Default::default(),
                condition: None,
                max_bytes: 4096,
                mode: PublishMode::Replace,
            },
            receiver,
        )
        .await
        .unwrap();
}

async fn ingest(catalog: &Catalog, root: &RootLease, config: &Config) -> Reconciliation {
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    crate::sync::ingest::rescan(catalog, root, config, &stopping)
        .await
        .unwrap()
}

async fn findings(root: &RootLease) -> Vec<(String, FindingKind)> {
    root.findings_page(0, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|finding| (finding.name, finding.reason))
        .collect()
}

#[test]
fn probe_names_never_take_the_operation_id_form() {
    for name in UNLINKED.into_iter().chain([DIRECTORY]) {
        assert!(name.parse::<OpaqueId>().is_err(), "{name}");
        assert!(name.len() != 32, "{name}");
    }
}

#[test]
fn observations_become_rules_only_when_names_keep_their_spelling() {
    assert!(matches!(
        RootNaming::try_from(observed(
            Colon::Accepted,
            Case::Sensitive,
            Spelling::Rewritten
        )),
        Err(ProbeError::RewritesNames)
    ));
    let limit = |name_max| {
        RootNaming::try_from(Observation {
            name_max: NameMax::from(name_max),
            ..observed(Colon::Accepted, Case::Insensitive, Spelling::Preserved)
        })
        .unwrap()
        .component_limit()
    };
    assert_eq!(limit(None), 255);
    assert_eq!(limit(Some(1024)), 255);
    assert_eq!(limit(Some(143)), 143);
    let rejecting = RootNaming::try_from(observed(
        Colon::Rejected,
        Case::Sensitive,
        Spelling::Preserved,
    ))
    .unwrap();
    assert!(!rejecting.admits("a:b".as_ref()));
    assert!(rejecting.admits("a?b".as_ref()));
    assert!(RootNaming::posix().admits("a:b".as_ref()));
    assert!(RootNaming::posix().admits("x".repeat(255).as_ref()));
    assert!(!RootNaming::posix().admits("x".repeat(256).as_ref()));
}

/// The probe reports what the filesystem of the test directory does. Case is compared with an
/// independent check, since either platform can mount a filesystem of the other kind.
#[tokio::test]
async fn host_probe_reports_the_filesystem_of_the_root() {
    let (_data, tree, catalog, root, _config) = unprobed_fixture(None).await;
    let writeback = root.default_writeback().await.unwrap();
    let observed = writeback.probe_naming().await.unwrap().unwrap();
    fs::write(tree.0.join("Folding-Check"), b"").unwrap();
    let case = if tree.0.join("folding-check").exists() {
        Case::Insensitive
    } else {
        Case::Sensitive
    };
    fs::remove_file(tree.0.join("Folding-Check")).unwrap();
    assert_eq!(observed.case, case);
    assert_eq!(observed.colon, Colon::Accepted);
    assert_eq!(observed.name_max, NameMax::from(Some(255)));
    // APFS, unlike HFS+, stores both Unicode normalizations as they were created, even though it
    // resolves one to the other on lookup.
    #[cfg(target_os = "macos")]
    assert_eq!(observed.spelling, Spelling::Preserved);
    // Linux local filesystems store name bytes exactly unless a directory is casefolded.
    #[cfg(target_os = "linux")]
    assert_eq!(observed.spelling, Spelling::Preserved);
    assert!(probe_entries(&tree.0).is_empty());
    drop((writeback, root));
    catalog.close().await.unwrap();
}

/// A second probe of the same root, as a rescan after a pause or a retried failure runs, records
/// the rules the first one did instead of stopping the root as changed. Each probe unlinks and
/// creates its names again, and the case check reads their spelling, never their inode numbers,
/// which Docker Desktop's shared folders can report anew for a recreated name.
#[tokio::test]
async fn probing_the_same_root_again_records_the_same_rules() {
    let (_data, tree, catalog, root, _config) = unprobed_fixture(None).await;
    let writeback = root.default_writeback().await.unwrap();
    let first = activate(&writeback).await.unwrap();
    assert_eq!(activate(&writeback).await.unwrap(), first);
    assert_eq!(root.naming().unwrap(), first);
    assert!(probe_entries(&tree.0).is_empty());
    drop((writeback, root));
    catalog.close().await.unwrap();
}

/// The case check takes anything it can open under the folded spelling for proof that the root
/// folds case, so an entry someone put there under that spelling must not survive into the
/// check: on a root that tells case apart, the probe unlinks it first and still reports that.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_folded_name_left_in_the_probe_directory_is_no_proof_of_folding() {
    let (_data, tree, catalog, root, _config) = unprobed_fixture(None).await;
    let writeback = root.default_writeback().await.unwrap();
    let directory = tree.0.join(mapping::WORKSPACE).join(DIRECTORY);
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join(FOLDED), b"leftover").unwrap();
    let observed = writeback.probe_naming().await.unwrap().unwrap();
    assert_eq!(observed.case, Case::Sensitive);
    assert!(probe_entries(&tree.0).is_empty());
    drop((writeback, root));
    catalog.close().await.unwrap();
}

/// A root that stops because a probe observed other rules logs both sets, so the operator sees
/// which rule changed under it.
#[test]
fn a_naming_change_reports_the_recorded_and_the_observed_rules() {
    let observed = RootNaming::try_from(observed(
        Colon::Rejected,
        Case::Insensitive,
        Spelling::Preserved,
    ))
    .unwrap();
    let change = NamingChange {
        recorded: RootNaming::posix(),
        observed,
    };
    assert_eq!(
        serde_json::to_value(change).unwrap(),
        serde_json::json!({
            "recorded": {"colon": "accepted", "case": "sensitive", "name_max": 255},
            "observed": {"colon": "rejected", "case": "insensitive", "name_max": 255},
        })
    );
}

#[tokio::test]
async fn probe_leftovers_are_removed_by_name_and_other_entries_are_kept() {
    let (_data, tree, catalog, root, _config) = unprobed_fixture(None).await;
    let writeback = root.default_writeback().await.unwrap();
    let directory = tree.0.join(mapping::WORKSPACE).join(DIRECTORY);
    fs::create_dir(&directory).unwrap();
    for name in UNLINKED.into_iter().chain(["keep-me"]) {
        fs::write(directory.join(name), b"leftover").unwrap();
    }
    activate(&writeback).await.unwrap();
    assert_eq!(probe_entries(&tree.0), ["keep-me"]);
    drop((writeback, root));
    catalog.close().await.unwrap();
}

/// A crash at any boundary of the probe leaves at most some of its fixed names behind. The next
/// start's recovery, probe and scans must take none of them for data.
#[tokio::test]
async fn a_crash_at_any_probe_boundary_leaves_nothing_recovery_or_scans_take_for_data() {
    let mut boundaries = 0;
    'boundaries: for skip in 0.. {
        for &crash in Crash::ALL {
            let (data, tree, catalog, root, config) = unprobed_fixture(Some(&bucket())).await;
            let writeback = root.default_writeback().await.unwrap();
            let snapshot = TestDir::new();
            fault::arm(
                &tree.0,
                Step::SyncNamingProbe,
                skip,
                crash.fault(snapshot.0.clone()),
            );
            let observed = writeback.probe_naming().await.unwrap().unwrap();
            if fault::pending(&tree.0) > 0 {
                drop((writeback, root));
                catalog.close().await.unwrap();
                break 'boundaries;
            }
            drop((writeback, root));
            catalog.close().await.unwrap();
            let live = tree.0.join(mapping::WORKSPACE).join(DIRECTORY);
            let saved = snapshot.0.join(mapping::WORKSPACE).join(DIRECTORY);
            fs::remove_dir_all(&live).unwrap();
            if saved.exists() {
                fs::create_dir(&live).unwrap();
                for entry in fs::read_dir(&saved).unwrap() {
                    let entry = entry.unwrap();
                    fs::copy(entry.path(), live.join(entry.file_name())).unwrap();
                }
            }
            let catalog = Catalog::open(&data.0).await.unwrap();
            let root = crate::sync::preflight(&catalog, &config)
                .await
                .unwrap()
                .unwrap();
            let writeback = root.default_writeback().await.unwrap();
            assert_eq!(
                writeback.recover_pending().await.unwrap().stop(),
                Stop::Ready
            );
            assert_eq!(
                writeback.preserve_protected().await.unwrap().stop(),
                Stop::Ready
            );
            assert_eq!(
                activate(&writeback).await.unwrap(),
                RootNaming::try_from(observed).unwrap()
            );
            assert!(probe_entries(&tree.0).is_empty(), "{skip} {crash:?}");
            let ingested = ingest(&catalog, &root, &config).await;
            assert_eq!(
                (
                    ingested.ingested,
                    ingested.unrepresentable,
                    ingested.deferred
                ),
                (0, 0, 0)
            );
            assert_eq!(writeback.status().await.unwrap().entries, 0);
            assert!(findings(&root).await.is_empty());
            drop((writeback, root));
            catalog.close().await.unwrap();
        }
        boundaries += 1;
    }
    // Opening the directory, then creating and removing each of the four names.
    assert_eq!(boundaries, 9);
}

#[tokio::test]
async fn unwritable_roots_pause_as_io_failures_with_the_cause_kept() {
    use std::os::unix::fs::PermissionsExt;
    let (_data, tree, catalog, root, _config) = unprobed_fixture(None).await;
    let writeback = root.default_writeback().await.unwrap();
    for (kind, transient) in [
        (io::ErrorKind::StorageFull, false),
        (io::ErrorKind::WouldBlock, true),
    ] {
        fault::arm(&tree.0, Step::SyncNamingProbe, 0, Fault::Io(kind));
        let error = activate(&writeback).await.unwrap_err();
        let SyncError::RootNaming(ProbeError::Io(cause)) = &error else {
            panic!("a failed probe is a naming failure: {error:?}");
        };
        assert_eq!(cause.kind(), kind);
        assert_eq!(error.pause_reason(), PauseReason::IoFailure);
        assert_eq!(error.is_transient(), transient);
        assert!(error.source().is_some());
        assert!(
            crate::error::Report(&error)
                .to_string()
                .contains("make sync_dir writable"),
            "{error}"
        );
    }
    // A read-only workspace, as on a read-only mount, cannot hold the probe directory.
    let workspace = tree.0.join(mapping::WORKSPACE);
    fs::remove_dir(workspace.join(DIRECTORY)).unwrap();
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o500)).unwrap();
    let error = activate(&writeback).await.unwrap_err();
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        matches!(&error, SyncError::RootNaming(ProbeError::Io(cause)) if cause.kind() == io::ErrorKind::PermissionDenied),
        "{error:?}"
    );
    assert_eq!(error.pause_reason(), PauseReason::IoFailure);
    assert!(root.naming().is_err(), "a failed probe records no rules");
    activate(&writeback).await.unwrap();
    drop((writeback, root));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn colon_rejecting_roots_record_findings_and_skip_colon_files_without_other_restrictions() {
    let (_data, tree, catalog, root, config) = unprobed_fixture(Some(&bucket())).await;
    inject(
        &tree.0,
        observed(Colon::Rejected, Case::Sensitive, Spelling::Preserved),
    );
    let root = probed(root).await;
    for name in ["logs/2026-10-28T12:00:00.log", "a?b", "plain"] {
        publish(&catalog, name).await;
    }
    let summary = crate::sync::reconcile(&catalog, &root).await.unwrap();
    assert_eq!(summary.exported, 2);
    assert!(tree.0.join("test-bucket/a?b").is_file());
    assert!(tree.0.join("test-bucket/plain").is_file());
    assert!(!tree.0.join("test-bucket/logs").exists());
    assert_eq!(
        findings(&root).await,
        [(
            "logs/2026-10-28T12:00:00.log".to_owned(),
            FindingKind::Unmapped(Reason::HostRestriction)
        )]
    );
    // The host stores the colon, but the root's rules do not map it.
    fs::write(tree.0.join("test-bucket/external:file"), b"external").unwrap();
    let ingested = ingest(&catalog, &root, &config).await;
    assert_eq!((ingested.ingested, ingested.unrepresentable), (0, 1));
    assert!(matches!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket(), &ObjectName::new("external:file").unwrap(), None)
            .await,
        Err(crate::storage::admin::AdminError::NotFound)
    ));
    drop(root);
    catalog.close().await.unwrap();
}

/// Findings that a colon-refusing root left, or that releases before colons were mapped recorded,
/// go once a later start exports the object.
#[tokio::test]
async fn colon_findings_clear_at_the_next_start_once_the_root_stores_colons() {
    let (data, tree, catalog, root, config) = unprobed_fixture(Some(&bucket())).await;
    inject(
        &tree.0,
        observed(Colon::Rejected, Case::Sensitive, Spelling::Preserved),
    );
    publish(&catalog, "logs/2026-10-28T12:00:00.log").await;
    crate::sync::reconcile(&catalog, &root).await.unwrap();
    assert_eq!(findings(&root).await.len(), 1);
    drop(root);
    catalog.close().await.unwrap();
    inject(
        &tree.0,
        observed(Colon::Accepted, Case::Sensitive, Spelling::Preserved),
    );
    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = crate::sync::preflight(&catalog, &config)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        crate::sync::reconcile(&catalog, &root)
            .await
            .unwrap()
            .exported,
        1
    );
    assert!(
        tree.0
            .join("test-bucket/logs/2026-10-28T12:00:00.log")
            .is_file()
    );
    assert!(findings(&root).await.is_empty());
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn case_folding_roots_are_recorded_and_still_map_colons() {
    let (_data, tree, catalog, root, _config) = unprobed_fixture(Some(&bucket())).await;
    inject(
        &tree.0,
        observed(Colon::Accepted, Case::Insensitive, Spelling::Preserved),
    );
    let root = probed(root).await;
    let naming = serde_json::to_value(root.naming().unwrap()).unwrap();
    assert_eq!(
        naming,
        serde_json::json!({"colon":"accepted","case":"insensitive","name_max":255})
    );
    publish(&catalog, "a:b").await;
    assert_eq!(
        crate::sync::reconcile(&catalog, &root)
            .await
            .unwrap()
            .exported,
        1
    );
    assert!(tree.0.join("test-bucket/a:b").is_file());
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn external_colon_files_are_ingested_as_new_objects() {
    let (_data, tree, catalog, root, config) =
        crate::sync::test_support::fixture(Some(&bucket())).await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/notes:1.txt"), b"external").unwrap();
    assert_eq!(ingest(&catalog, &root, &config).await.ingested, 1);
    let record = catalog
        .admin(Origin::Api)
        .get_object(&bucket(), &ObjectName::new("notes:1.txt").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(record.size, 8);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rewriting_roots_are_refused_before_any_scan_and_pause_the_mirror() {
    let (_data, tree, catalog, root, config) = unprobed_fixture(Some(&bucket())).await;
    inject(
        &tree.0,
        observed(Colon::Accepted, Case::Insensitive, Spelling::Rewritten),
    );
    publish(&catalog, "object").await;
    // A file the scan would ingest stays out of the catalog while the root is refused.
    fs::create_dir_all(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/external"), b"external").unwrap();
    let failure = crate::sync::reconcile(&catalog, &root).await.unwrap_err();
    // The generic pause remedy asks for a rescan, so the reason itself names the next step.
    let reason = failure.to_string();
    for step in [
        "sync_dir cannot be a synchronization root",
        "A rescan pauses again",
        "set sync_dir to a directory on a filesystem that keeps file names as created and restart",
    ] {
        assert!(reason.contains(step), "{reason}");
    }
    assert!(root.naming().is_err());
    assert!(!tree.0.join("test-bucket/object").exists());
    let task = crate::sync::start(catalog.clone(), root, &config).unwrap();
    tokio::time::timeout(
        Duration::from_secs(30),
        task.status().reached(MirrorState::Paused),
    )
    .await
    .unwrap();
    let (_, root) = catalog.sync_controls().unwrap().unwrap();
    crate::test_support::eventually_async("the pause to be recorded", || async {
        root.observation().await.unwrap().paused_reason == Some(PauseReason::ObserverFailure)
    })
    .await;
    assert!(!tree.0.join("test-bucket/object").exists());
    assert!(matches!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket(), &ObjectName::new("external").unwrap(), None)
            .await,
        Err(crate::storage::admin::AdminError::NotFound)
    ));
    drop(root);
    tokio::time::timeout(Duration::from_secs(10), task.finish())
        .await
        .unwrap()
        .unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn other_rules_on_the_same_root_mean_the_root_changed() {
    let (_data, tree, catalog, root, _config) = unprobed_fixture(None).await;
    let writeback = root.default_writeback().await.unwrap();
    inject(
        &tree.0,
        observed(Colon::Accepted, Case::Sensitive, Spelling::Preserved),
    );
    let first = activate(&writeback).await.unwrap();
    assert_eq!(activate(&writeback).await.unwrap(), first);
    inject(
        &tree.0,
        observed(Colon::Rejected, Case::Sensitive, Spelling::Preserved),
    );
    assert!(matches!(
        activate(&writeback).await,
        Err(SyncError::Root(RootError::RootChanged))
    ));
    assert_eq!(root.naming().unwrap(), first);
    drop((writeback, root));
    catalog.close().await.unwrap();
}
