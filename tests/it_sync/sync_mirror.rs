// Directory synchronization supports only Linux and macOS; elsewhere roots are rejected.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use crate::common;
use bytes::Bytes;
use common::TestDir;
use gcs_mimic::{
    clock::SystemClock,
    config::Config,
    object::UploadChunk,
    storage::{
        Catalog,
        admin::{BucketDefinition, BucketName, ObjectName, Origin, Publication, PublishMode},
        sync_state::root::{Activation, Baseline},
    },
    sync,
};
use std::{
    fs,
    io::{BufRead, BufReader},
    num::NonZeroUsize,
    process::Stdio,
};

async fn bucket(catalog: &Catalog, name: &str, owner: &str) -> BucketName {
    let name = BucketName::new(name).unwrap();
    catalog
        .admin(Origin::SyncIngest)
        .ensure_bucket(BucketDefinition::default_for(&name, owner, &SystemClock).unwrap())
        .await
        .unwrap();
    name
}
async fn object(catalog: &Catalog, bucket: &BucketName, name: &str, body: &[u8]) {
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    tx.try_send(Ok(UploadChunk::Data(Bytes::copy_from_slice(body))))
        .unwrap();
    tx.try_send(Ok(UploadChunk::End)).unwrap();
    catalog
        .admin(Origin::SyncIngest)
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
        .unwrap();
}
fn config(data: &TestDir, tree: &TestDir) -> Config {
    Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    }
}

#[tokio::test]
async fn initial_export_pages_all_owners_preserves_empty_binary_bodies_and_api_independence() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let first = bucket(&catalog, "first-bucket", "owner-a").await;
    bucket(&catalog, "empty-bucket", "owner-b").await;
    let last = bucket(&catalog, "last-bucket", "owner-c").await;
    for n in 0..258 {
        object(&catalog, &first, &format!("nested/{n:04}"), b"\0\xffbody").await;
    }
    object(&catalog, &last, "empty", b"").await;
    let config = config(&data, &tree);
    let root = sync::preflight(&catalog, &config).await.unwrap().unwrap();
    let summary = sync::reconcile(&catalog, &root).await.unwrap();
    assert_eq!(
        (
            summary.exported,
            summary.identical,
            summary.external_only,
            summary.unrepresentable
        ),
        (259, 0, 0, 0)
    );
    assert!(summary.baseline_complete);
    assert!(tree.0.join("empty-bucket").is_dir());
    assert_eq!(
        fs::read(tree.0.join("first-bucket/nested/0257")).unwrap(),
        b"\0\xffbody"
    );
    assert_eq!(fs::read(tree.0.join("last-bucket/empty")).unwrap(), b"");
    let summary = sync::reconcile(&catalog, &root).await.unwrap();
    assert_eq!((summary.exported, summary.identical), (0, 259));
    fs::write(tree.0.join("first-bucket/nested/0000"), b"external").unwrap();
    let (_, reader) = catalog
        .admin(Origin::SyncIngest)
        .open_object(&first, &ObjectName::new("nested/0000").unwrap(), None)
        .await
        .unwrap();
    let mut file = reader.into_file().await.unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).unwrap();
    assert_eq!(bytes, b"\0\xffbody");
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn initial_export_preserves_external_conflicts_and_records_mapping_findings() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let name = bucket(&catalog, "test-bucket", "owner").await;
    for (key, bytes) in [
        ("same", &b"same"[..]),
        ("different", b"api"),
        ("new", b"api"),
        (".DS_Store", b"ignored"),
        ("a//b", b"invalid"),
        ("prefix", b"file"),
        ("prefix/child", b"child"),
    ] {
        object(&catalog, &name, key, bytes).await;
    }
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/same"), b"same").unwrap();
    fs::write(tree.0.join("test-bucket/different"), b"external").unwrap();
    fs::write(tree.0.join("test-bucket/external"), b"only").unwrap();
    fs::write(tree.0.join("test-bucket/4913"), b"editor").unwrap();
    let config = config(&data, &tree);
    let root = sync::preflight(&catalog, &config).await.unwrap().unwrap();
    let summary = sync::reconcile(&catalog, &root).await.unwrap();
    assert_eq!(
        (
            summary.exported,
            summary.identical,
            summary.conflicting,
            summary.external_only,
            summary.ignored,
            summary.unrepresentable
        ),
        (2, 1, 1, 1, 2, 3)
    );
    assert!(!summary.baseline_complete);
    assert_eq!(
        fs::read(tree.0.join("test-bucket/different")).unwrap(),
        b"api"
    );
    let retained = fs::read_dir(data.0.join("sync-conflicts"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(retained.len(), 1);
    assert_eq!(fs::read(retained[0].path()).unwrap(), b"external");
    assert_eq!(
        fs::read(tree.0.join("test-bucket/external")).unwrap(),
        b"only"
    );
    assert!(!tree.0.join("test-bucket/prefix").exists());
    assert_eq!(root.findings_page(0, 256).await.unwrap().len(), 4);
    drop(root);
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, Activation::Open)
        .await
        .unwrap();
    assert_eq!(root.status().baseline, Baseline::Activating);
    drop(root);
    catalog.close().await.unwrap();
}

/// An object past the depth limit is one finding, not a stopped root; a raised limit exports it,
/// and a limit lowered under a tree that already holds it names the setting to raise again.
#[tokio::test]
async fn the_depth_limit_keeps_an_object_out_of_the_tree_until_it_is_raised() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let name = bucket(&catalog, "test-bucket", "owner").await;
    object(&catalog, &name, "one/shallow", b"shallow").await;
    object(&catalog, &name, "one/two/deep", b"deep").await;
    let deep = tree.0.join("test-bucket/one/two/deep");

    let mut settings = config(&data, &tree);
    settings.max_sync_depth = NonZeroUsize::new(2);
    let root = sync::preflight(&catalog, &settings).await.unwrap().unwrap();
    let summary = sync::reconcile(&catalog, &root).await.unwrap();
    assert_eq!((summary.exported, summary.unrepresentable), (1, 1));
    assert!(tree.0.join("test-bucket/one/shallow").exists());
    assert!(!deep.exists());
    assert_eq!(root.findings_page(0, 256).await.unwrap().len(), 1);
    drop(root);

    settings.max_sync_depth = NonZeroUsize::new(3);
    let root = sync::preflight(&catalog, &settings).await.unwrap().unwrap();
    let summary = sync::reconcile(&catalog, &root).await.unwrap();
    assert_eq!(summary.exported, 1);
    assert_eq!(fs::read(&deep).unwrap(), b"deep");
    drop(root);

    settings.max_sync_depth = NonZeroUsize::new(2);
    let root = sync::preflight(&catalog, &settings).await.unwrap().unwrap();
    let failure = sync::reconcile(&catalog, &root).await.unwrap_err();
    let failure = format!("{failure:?}");
    assert!(failure.contains("max_sync_depth"), "{failure}");
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn the_entry_limit_comes_from_the_configuration_and_survives_a_restart() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let name = bucket(&catalog, "test-bucket", "owner").await;
    for key in ["a", "b", "c"] {
        object(&catalog, &name, key, b"body").await;
    }
    let mut settings = config(&data, &tree);
    settings.max_sync_entries = NonZeroUsize::new(4);
    let root = sync::preflight(&catalog, &settings).await.unwrap().unwrap();
    assert_eq!(sync::reconcile(&catalog, &root).await.unwrap().exported, 3);
    drop(root);
    catalog.close().await.unwrap();

    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = sync::preflight(&catalog, &settings).await.unwrap().unwrap();
    assert_eq!(sync::reconcile(&catalog, &root).await.unwrap().identical, 3);
    drop(root);

    fs::write(tree.0.join("test-bucket/external"), b"over").unwrap();
    let root = sync::preflight(&catalog, &settings).await.unwrap().unwrap();
    let failure = sync::reconcile(&catalog, &root).await.unwrap_err();
    let failure = format!("{failure:?}");
    assert!(failure.contains("max_sync_entries"), "{failure}");
    drop(root);

    settings.max_sync_entries = NonZeroUsize::new(5);
    let root = sync::preflight(&catalog, &settings).await.unwrap().unwrap();
    assert_eq!(
        sync::reconcile(&catalog, &root)
            .await
            .unwrap()
            .external_only,
        1
    );
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn marker_preflight_rejects_missing_foreign_malformed_and_allows_exact_reinitialize() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let mut config = config(&data, &tree);
    let root = sync::preflight(&catalog, &config).await.unwrap().unwrap();
    let id = root.status().catalog_id;
    drop(root);
    let marker = tree.0.join(sync::mapping::MARKER);
    let original = fs::read(&marker).unwrap();
    fs::remove_file(&marker).unwrap();
    let error = sync::preflight(&catalog, &config).await.err().unwrap();
    assert!(error.to_string().contains("--sync-reinitialize"));
    config.sync_reinitialize = Some(gcs_mimic::storage::sync_state::CatalogId::from_bytes(
        [0; 16],
    ));
    assert!(sync::preflight(&catalog, &config).await.is_err());
    config.sync_reinitialize = Some(id);
    drop(sync::preflight(&catalog, &config).await.unwrap());
    fs::write(&marker, b"malformed").unwrap();
    assert!(sync::preflight(&catalog, &config).await.is_err());
    assert_eq!(fs::read(&marker).unwrap(), b"malformed");
    let mut foreign: serde_json::Value = serde_json::from_slice(&original).unwrap();
    foreign["catalog_id"] = serde_json::json!("00000000000000000000000000000000");
    fs::write(&marker, foreign.to_string()).unwrap();
    let error = sync::preflight(&catalog, &config)
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("committed to Git or copied from another environment"));
    assert!(error.contains(".gitignore"));
    assert!(error.contains("intended data_dir"));
    catalog.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_mapping_reports_type_length_and_symlink_conflicts_without_following_them() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let outside = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let name = bucket(&catalog, "test-bucket", "owner").await;
    for key in [
        "link/child".to_owned(),
        "file/child".into(),
        "directory".into(),
        "x".repeat(256),
    ] {
        object(&catalog, &name, &key, b"api").await;
    }
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::create_dir(tree.0.join("test-bucket/directory")).unwrap();
    fs::write(tree.0.join("test-bucket/file"), b"external").unwrap();
    fs::write(outside.0.join("child"), b"protected").unwrap();
    std::os::unix::fs::symlink(&outside.0, tree.0.join("test-bucket/link")).unwrap();
    let config = config(&data, &tree);
    let root = sync::preflight(&catalog, &config).await.unwrap().unwrap();
    let summary = sync::reconcile(&catalog, &root).await.unwrap();
    assert_eq!(summary.exported, 0);
    assert!(!summary.baseline_complete);
    let findings = root.findings_page(0, 256).await.unwrap();
    assert_eq!(findings.len(), 4);
    assert_eq!(fs::read(outside.0.join("child")).unwrap(), b"protected");
    assert_eq!(
        fs::read(tree.0.join("test-bucket/file")).unwrap(),
        b"external"
    );
    drop(root);
    catalog.close().await.unwrap();
}

#[test]
fn sync_marker_failure_precedes_bind_and_ready_and_process_exports_after_ready() {
    let root = TestDir::new();
    fs::create_dir(root.0.join("tree")).unwrap();
    fs::write(root.0.join("tree/.gcs-mimic-sync.json"), b"invalid").unwrap();
    let mut command = common::server_command();
    command
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
        ])
        .arg(root.0.join("data"))
        .arg("--sync-dir")
        .arg(root.0.join("tree"));
    let output = command.output().unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("ready"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("foreign/malformed"));
    let root = TestDir::new();
    fs::create_dir(root.0.join("tree")).unwrap();
    let mut command = common::server_command();
    command
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
        ])
        .arg(root.0.join("data"))
        .arg("--sync-dir")
        .arg(root.0.join("tree"));
    fs::create_dir_all(root.0.join("seed/test-bucket")).unwrap();
    fs::write(root.0.join("seed/test-bucket/body"), b"seed").unwrap();
    command.arg("--seed-dir").arg(root.0.join("seed"));
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    let count = stdout.read_line(&mut line).unwrap();
    if count == 0 {
        let mut error = String::new();
        std::io::Read::read_to_string(child.stderr.as_mut().unwrap(), &mut error).unwrap();
        panic!("startup failed: {error}");
    }
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap()["event"],
        "ready"
    );
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    loop {
        line.clear();
        assert_ne!(stderr.read_line(&mut line).unwrap(), 0);
        if line.contains("sync_reconciled") {
            break;
        }
        assert!(!line.contains("sync_stopped"), "{line}");
    }
    assert_eq!(
        fs::read(root.0.join("tree/test-bucket/body")).unwrap(),
        b"seed"
    );
    common::stop_server(&mut child, &mut stdout);
}

/// A hang guard, not a latency bound. A root that pauses keeps its server running and logs nothing
/// more, so without this a pass that cannot finish would wait forever instead of failing.
const STARTUP_PROGRESS_GUARD: std::time::Duration = std::time::Duration::from_secs(120);

/// Runs a server over `tree` until its startup pass ends, reads `object` of `test-bucket`, then
/// stops the server and returns what it logged.
fn startup_events(
    data: &TestDir,
    tree: &TestDir,
    options: &[&str],
    object: &str,
) -> Vec<serde_json::Value> {
    let mut child = common::server_command()
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
        ])
        .arg(&data.0)
        .arg("--sync-dir")
        .arg(&tree.0)
        .args(options)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["event"], "ready", "{line}");
    let address = ready["listen"].as_str().unwrap().to_owned();
    let stderr = child.stderr.take().unwrap();
    let (send, receive) = std::sync::mpsc::channel::<serde_json::Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if send
                .send(serde_json::from_str(&line.unwrap()).unwrap())
                .is_err()
            {
                break;
            }
        }
    });
    let mut events = Vec::new();
    loop {
        let event = receive.recv_timeout(STARTUP_PROGRESS_GUARD);
        let reconciled = event
            .as_ref()
            .is_ok_and(|event| event["event"] == "sync_reconciled");
        let stopped = event
            .as_ref()
            .is_ok_and(|event| event["event"] == "sync_stopped");
        events.extend(event.as_ref().ok().cloned());
        if event.is_err() || stopped {
            // The server would otherwise outlive the failed test.
            let _ = child.kill();
            panic!("the startup pass did not finish: {event:?} after {events:?}");
        }
        if reconciled {
            break;
        }
    }
    let path = format!(
        "/storage/v1/b/test-bucket/o/{}?alt=media",
        object.replace('/', "%2F")
    );
    let read = common::effects::request(&address, "GET", &path, "", b"");
    assert_eq!(read.status, 200, "{}", String::from_utf8_lossy(&read.body));
    assert_eq!(read.body, b"deep");
    common::stop_server(&mut child, &mut stdout);
    events.extend(receive);
    events
}

fn large_tree_warnings(events: &[serde_json::Value]) -> usize {
    events
        .iter()
        .filter(|event| event["event"] == "sync_tree_large")
        .count()
}

fn startup_summary<'a>(events: &'a [serde_json::Value], name: &str) -> &'a serde_json::Value {
    events
        .iter()
        .find(|event| event["event"] == name)
        .unwrap_or_else(|| panic!("no {name}: {events:?}"))
}

/// Past every former sync default at once: 10,000 entries and 128 levels. Without a limit the tree
/// has to synchronize, warn once however many scans it takes, and keep its state across a restart.
/// Editor temporaries count as entries without being ingested, which saves a settle interval per
/// page of them; the entries that do ingest show that the pass got to the end of the tree.
#[test]
fn a_tree_past_the_former_defaults_synchronizes_warns_once_and_survives_a_restart() {
    const IGNORED: usize = 10_001;
    // 129 levels below the root counting the bucket, one past the former default.
    const DEEP_DIRECTORIES: usize = 128;
    let data = TestDir::new();
    let tree = TestDir::new();
    let bucket = tree.0.join("test-bucket");
    let deep =
        bucket.join(std::iter::repeat_n("d", DEEP_DIRECTORIES).collect::<std::path::PathBuf>());
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("deep"), b"deep").unwrap();
    for index in 0..IGNORED {
        fs::write(bucket.join(format!("editor-{index:05}~")), b"").unwrap();
    }
    fs::write(bucket.join("zz-last"), b"deep").unwrap();
    let object = format!("{}/deep", ["d"; DEEP_DIRECTORIES].join("/"));

    let events = startup_events(&data, &tree, &[], &object);
    assert_eq!(large_tree_warnings(&events), 1, "{events:?}");
    let ingested = startup_summary(&events, "sync_ingested");
    assert_eq!(ingested["ingested"], 2, "{ingested}");
    assert_eq!(ingested["ignored"], IGNORED, "{ingested}");

    let events = startup_events(&data, &tree, &[], "zz-last");
    assert_eq!(large_tree_warnings(&events), 1, "{events:?}");
    assert_eq!(startup_summary(&events, "sync_ingested")["ingested"], 0);

    // A limit someone chose already says the size is deliberate.
    let entries = (IGNORED + 2 + 1 + DEEP_DIRECTORIES).to_string();
    let events = startup_events(&data, &tree, &["--max-sync-entries", &entries], &object);
    assert_eq!(large_tree_warnings(&events), 0, "{events:?}");
}

/// One byte per level fills a 1024-byte object name 512 levels deep, deeper than `mkdir -p` can go
/// from an absolute path on macOS. Every walk of a synchronized tree stays relative to a pinned
/// directory, so `PATH_MAX` does not apply, and the descriptor it holds per level has to fit in
/// what a server raises its limit to.
#[tokio::test]
async fn the_deepest_object_name_exports_and_is_found_again_after_a_restart() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let object_name = ["a"; 512].join("/");
    assert_eq!(object_name.len(), 1023);
    let catalog = Catalog::open(&data.0).await.unwrap();
    let name = bucket(&catalog, "test-bucket", "local-project").await;
    object(&catalog, &name, &object_name, b"deep").await;
    catalog.close().await.unwrap();

    let events = startup_events(&data, &tree, &[], &object_name);
    assert_eq!(startup_summary(&events, "sync_reconciled")["exported"], 1);
    assert_eq!(startup_summary(&events, "sync_ingested")["deferred"], 0);

    let events = startup_events(&data, &tree, &[], &object_name);
    assert_eq!(startup_summary(&events, "sync_reconciled")["identical"], 1);
    assert_eq!(startup_summary(&events, "sync_ingested")["deferred"], 0);
    // The test process keeps its own smaller descriptor limit, which removing the tree must fit.
    gcs_mimic::fd_limit::raise_for_connections(1024, None);
}

#[test]
fn descriptor_directory_pages_are_bounded_and_never_follow_redirects() {
    let root = TestDir::new();
    for n in 0..258 {
        fs::write(root.0.join(format!("{n:04}")), b"x").unwrap();
    }
    let directory = gcs_mimic::fsutil::open_directory_nofollow(&root.0).unwrap();
    let page = gcs_mimic::fsutil::directory_names_after(&directory, None, 256).unwrap();
    assert_eq!(page.len(), 256);
    assert_eq!(
        gcs_mimic::fsutil::directory_names_after(
            &directory,
            page.last().map(|s| s.as_os_str()),
            256
        )
        .unwrap()
        .len(),
        2
    );
    assert!(gcs_mimic::fsutil::directory_names_after(&directory, None, 257).is_err());
    gcs_mimic::fsutil::directory_writable(&directory).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.0.join("0000"), root.0.join("link")).unwrap();
        assert!(gcs_mimic::fsutil::open_regular_at(&directory, "link".as_ref()).is_err());
        assert_eq!(fs::read(root.0.join("0000")).unwrap(), b"x");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn sigterm_during_initial_export_restarts_without_recovery_stop() {
    use std::time::{Duration, Instant};
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let bucket = bucket(&catalog, "test-bucket", "owner").await;
    for index in 0..128 {
        object(&catalog, &bucket, &format!("{index:04}"), b"api").await;
    }
    catalog.close().await.unwrap();
    for initial in [true, false] {
        let mut command = common::server_command();
        command
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(&data.0)
            .arg("--sync-dir")
            .arg(&tree.0);
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        assert!(line.contains("ready"), "{line}");
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        if initial {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !tree.0.join("test-bucket/0000").exists() {
                assert!(Instant::now() < deadline, "export did not begin");
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                !tree.0.join("test-bucket/0127").exists(),
                "export finished before interruption"
            );
            common::stop_server(&mut child, &mut stdout);
            let mut log = String::new();
            std::io::Read::read_to_string(&mut stderr, &mut log).unwrap();
            assert!(!log.contains("sync_stopped"), "{log}");
        } else {
            loop {
                line.clear();
                assert_ne!(stderr.read_line(&mut line).unwrap(), 0);
                if line.contains("sync_reconciled") {
                    break;
                }
                if line.contains("sync_stopped") {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("{line}");
                }
            }
            assert_eq!(fs::read(tree.0.join("test-bucket/0127")).unwrap(), b"api");
            common::stop_server(&mut child, &mut stdout);
        }
    }
}
