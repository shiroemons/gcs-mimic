// Directory synchronization supports only Linux and macOS; elsewhere roots are rejected.
#![cfg(any(target_os = "linux", target_os = "macos"))]

mod common;
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
    let summary = sync::reconcile(&catalog, &root, &config).await.unwrap();
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
    let summary = sync::reconcile(&catalog, &root, &config).await.unwrap();
    assert_eq!((summary.exported, summary.identical), (0, 259));
    fs::write(tree.0.join("first-bucket/nested/0000"), b"external").unwrap();
    let (_, reader) = catalog
        .admin(Origin::SyncIngest)
        .open_object(&first, &ObjectName::new("nested/0000").unwrap(), None)
        .await
        .unwrap();
    let mut file = reader.into_file().await;
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
    let summary = sync::reconcile(&catalog, &root, &config).await.unwrap();
    assert_eq!(
        (
            summary.exported,
            summary.identical,
            summary.conflicting,
            summary.external_only,
            summary.ignored,
            summary.unrepresentable
        ),
        (1, 1, 1, 1, 2, 3)
    );
    assert!(!summary.baseline_complete);
    assert_eq!(
        fs::read(tree.0.join("test-bucket/different")).unwrap(),
        b"external"
    );
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
    let summary = sync::reconcile(&catalog, &root, &config).await.unwrap();
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
