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
        admin::{
            BucketDefinition, BucketName, GenerationCondition, ObjectName, Origin, Publication,
            PublishMode,
        },
    },
    sync,
};
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader},
    process::{Child, ChildStdout, Stdio},
    time::Duration,
};

struct SyncServer {
    child: Child,
    stdout: BufReader<ChildStdout>,
    address: String,
}
impl SyncServer {
    fn start(data: &std::path::Path, tree: &std::path::Path) -> Self {
        Self::start_with_args(data, tree, &[])
    }
    fn start_with_args(data: &std::path::Path, tree: &std::path::Path, extra: &[&str]) -> Self {
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(data)
            .arg("--sync-dir")
            .arg(tree)
            .args(extra)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready");
        Self {
            child,
            stdout,
            address: ready["listen"].as_str().unwrap().into(),
        }
    }
    fn request(&self, method: &str, path: &str, body: &[u8]) -> common::effects::Reply {
        common::effects::request(&self.address, method, path, "", body)
    }
    fn stop(&mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
    }
    fn rescan(&self) -> common::effects::Reply {
        let reply = self.request("POST", "/_internal/sync/rescan", b"{}");
        reply.expect(202);
        let value: Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(value["completed"], true);
        reply
    }
    fn wait(&self, payload: &[u8]) -> (common::effects::Reply, Duration) {
        let started = std::time::Instant::now();
        let reply = self.request("POST", "/_internal/sync/wait", payload);
        let elapsed = started.elapsed();
        (reply, elapsed)
    }
}

#[test]
fn a_tree_past_the_entry_limit_pauses_until_a_restart_raises_the_limit_and_it_stays_up() {
    let data = TestDir::new();
    let tree = TestDir::new();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    for name in ["one", "two", "three"] {
        fs::write(tree.0.join("test-bucket").join(name), b"body").unwrap();
    }
    let mut server = SyncServer::start_with_args(&data.0, &tree.0, &["--max-sync-entries", "3"]);
    let (waited, _) = server.wait(br#"{"timeout_ms":30000}"#);
    waited.expect(409);
    let waited: Value = serde_json::from_slice(&waited.body).unwrap();
    assert_eq!(waited["reason"], "Paused");
    server.request("GET", "/_internal/ready", b"").expect(503);
    server.stop();
    for _ in 0..2 {
        let mut server =
            SyncServer::start_with_args(&data.0, &tree.0, &["--max-sync-entries", "4"]);
        server.wait(br#"{"timeout_ms":30000}"#).0.expect(200);
        server.request("GET", "/_internal/ready", b"").expect(200);
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/test-bucket/o/three?alt=media", b"")
                .body,
            b"body"
        );
        server.stop();
    }
}

#[test]
fn live_wait_observes_source_work_beyond_the_connection_inactivity_budget() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let mut server =
        SyncServer::start_with_args(&data.0, &tree.0, &["--request-timeout-ms", "100"]);
    server.wait(b"{}").0.expect(200);
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/body"), b"waited body").unwrap();
    let started = std::time::Instant::now();
    let (waited, request_elapsed) = server.wait(br#"{"timeout_ms":5000}"#);
    waited.expect(200);
    assert!(waited.json_ok()["completed"].as_bool().unwrap());
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert!(request_elapsed >= Duration::from_millis(100));
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/body?alt=media", b"")
            .body,
        b"waited body"
    );
    server.stop();
}
impl Drop for SyncServer {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
    }
}

#[test]
fn live_sync_controls_rescan_external_deletions_and_preserve_http_namespace_errors() {
    let data = TestDir::new();
    let tree = TestDir::new();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/body"), b"initial").unwrap();
    let mut server = SyncServer::start(&data.0, &tree.0);
    let scanned = server.rescan();
    scanned.expect(202);
    let status = server.request("GET", "/_internal/sync", b"").json_ok();
    assert_eq!(status["auth_mode"], "permissive");
    assert_eq!(status["state"], "Ready");
    server
        .request("PUT", "/_internal/sync", b"{}")
        .json_reason(405, "methodNotAllowed");
    server
        .request("GET", "/_internal/sync?limit=1", b"")
        .json_reason(400, "invalidArgument");
    let (waited, _) = server.wait(b"{}");
    waited.expect(200);
    assert_eq!(waited.json_ok()["completed"], true);
    fs::remove_file(tree.0.join("test-bucket/body")).unwrap();
    server.rescan().expect(202);
    server
        .request("GET", "/storage/v1/b/test-bucket/o/body", b"")
        .json_reason(404, "notFound");
    server
        .request(
            "POST",
            "/_internal/sync/approve",
            br#"{"deletion_set":"00000000000000000000000000000000"}"#,
        )
        .json_reason(409, "conflict");
    fs::write(tree.0.join("test-bucket/body"), b"recreated externally").unwrap();
    server.rescan().expect(202);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/body?alt=media", b"")
            .body,
        b"recreated externally"
    );
    assert!(tree.0.join("test-bucket").is_dir());
    server.stop();
}

async fn publish(catalog: &Catalog, bucket: &BucketName, name: &str, body: &[u8]) -> i64 {
    let (tx, rx) = tokio::sync::mpsc::channel(2);
    tx.try_send(Ok(UploadChunk::Data(Bytes::copy_from_slice(body))))
        .unwrap();
    tx.try_send(Ok(UploadChunk::End)).unwrap();
    catalog
        .admin(Origin::Api)
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
        .unwrap()
        .generation
}

async fn wait_body(path: &std::path::Path, body: Option<&[u8]>) {
    common::input_fixtures::eventually_async("the exported file to match", || {
        std::future::ready(match (fs::read(path), body) {
            (Ok(found), Some(body)) => found == body,
            (Err(e), None) => e.kind() == std::io::ErrorKind::NotFound,
            _ => false,
        })
    })
    .await;
}

async fn wait_generation(catalog: &Catalog, bucket: &BucketName, path: &str, generation: i64) {
    common::input_fixtures::eventually_async(
        "the sync record to reach the generation",
        || async move {
            let binding = catalog.sync_state().binding(bucket).await.unwrap();
            catalog
                .sync_state()
                .get_path(binding, path.into())
                .await
                .unwrap()
                .is_some_and(|record| record.generation.get() == generation)
        },
    )
    .await;
}

async fn setup() -> (TestDir, TestDir, Catalog, BucketName, Config) {
    let data = TestDir::new();
    let tree = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    catalog
        .admin(Origin::Api)
        .ensure_bucket(BucketDefinition::default_for(&bucket, "owner", &SystemClock).unwrap())
        .await
        .unwrap();
    let config = Config {
        data_dir: data.0.clone(),
        sync_dir: Some(tree.0.clone()),
        ..Default::default()
    };
    (data, tree, catalog, bucket, config)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_export_creates_replaces_deletes_and_preserves_edited_files() {
    let (data, tree, catalog, bucket, config) = setup().await;
    let root = sync::preflight(&catalog, &config).await.unwrap().unwrap();
    let worker = sync::start(catalog.clone(), root.clone(), &config).unwrap();
    let path = tree.0.join("test-bucket/body");
    let generation = publish(&catalog, &bucket, "body", b"first\0\xff").await;
    wait_generation(&catalog, &bucket, "test-bucket/body", generation).await;
    wait_body(&path, Some(b"first\0\xff")).await;
    let generation = publish(&catalog, &bucket, "body", b"second").await;
    wait_generation(&catalog, &bucket, "test-bucket/body", generation).await;
    wait_body(&path, Some(b"second")).await;
    fs::write(&path, b"external overwrite").unwrap();
    let winner = publish(&catalog, &bucket, "body", b"third").await;
    let delete_generation = publish(&catalog, &bucket, "delete", b"api delete").await;
    wait_generation(&catalog, &bucket, "test-bucket/delete", delete_generation).await;
    let delete_path = tree.0.join("test-bucket/delete");
    fs::write(&delete_path, b"external delete").unwrap();
    catalog
        .admin(Origin::Api)
        .delete_object(
            &bucket,
            &ObjectName::new("delete").unwrap(),
            GenerationCondition::Is(
                gcs_mimic::storage::admin::Generation::new(delete_generation).unwrap(),
            ),
        )
        .await
        .unwrap();
    let clean = publish(&catalog, &bucket, "clean", b"").await;
    wait_generation(&catalog, &bucket, "test-bucket/clean", clean).await;
    catalog
        .admin(Origin::Api)
        .delete_object(
            &bucket,
            &ObjectName::new("clean").unwrap(),
            GenerationCondition::Is(gcs_mimic::storage::admin::Generation::new(clean).unwrap()),
        )
        .await
        .unwrap();
    wait_body(&tree.0.join("test-bucket/clean"), None).await;
    worker.finish().await.unwrap();
    assert_eq!(fs::read(path).unwrap(), b"third");
    assert!(!delete_path.exists());
    assert_eq!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket, &ObjectName::new("body").unwrap(), None)
            .await
            .unwrap()
            .generation,
        winner
    );
    assert!(matches!(
        catalog
            .admin(Origin::Api)
            .get_object(&bucket, &ObjectName::new("delete").unwrap(), None)
            .await,
        Err(gcs_mimic::storage::admin::AdminError::NotFound)
    ));
    let retained = fs::read_dir(data.0.join("sync-conflicts"))
        .unwrap()
        .map(|entry| fs::read(entry.unwrap().path()).unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        retained,
        std::collections::BTreeSet::from([
            b"external overwrite".to_vec(),
            b"external delete".to_vec()
        ])
    );
    let findings = root.findings_page(0, 256).await.unwrap();
    assert_eq!(
        findings
            .iter()
            .map(|finding| finding.name.as_str())
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::new()
    );
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_export_skips_colon_and_backslash_names_and_keeps_exporting() {
    let (_data, tree, catalog, bucket, config) = setup().await;
    let root = sync::preflight(&catalog, &config).await.unwrap().unwrap();
    let worker = sync::start(catalog.clone(), root.clone(), &config).unwrap();
    publish(&catalog, &bucket, "logs/2026-10-28T12:00:00.log", b"colon").await;
    publish(&catalog, &bucket, r"dir\file", b"backslash").await;
    let after = publish(&catalog, &bucket, "after", b"after").await;
    wait_generation(&catalog, &bucket, "test-bucket/after", after).await;
    wait_body(&tree.0.join("test-bucket/after"), Some(b"after")).await;
    // One change page is exported in name order, so "after" can land before the skipped names
    // are recorded; stopping at that point would drop their findings.
    let findings = &root;
    common::input_fixtures::eventually_async(
        "both skipped names recorded as findings",
        || async move { findings.findings_page(0, 256).await.unwrap().len() >= 2 },
    )
    .await;
    worker.finish().await.unwrap();
    assert!(
        !tree
            .0
            .join("test-bucket/logs/2026-10-28T12:00:00.log")
            .exists()
    );
    assert!(!tree.0.join(r"test-bucket/dir\file").exists());
    assert_eq!(root.findings_page(0, 256).await.unwrap().len(), 2);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_export_recovers_a_changed_journal_fence_with_paged_reconciliation() {
    let (_data, tree, catalog, bucket, config) = setup().await;
    let root = sync::preflight(&catalog, &config).await.unwrap().unwrap();
    let worker = sync::start(catalog.clone(), root.clone(), &config).unwrap();
    let generation = publish(&catalog, &bucket, "body", b"before").await;
    wait_generation(&catalog, &bucket, "test-bucket/body", generation).await;
    let removed = publish(&catalog, &bucket, "removed", b"before gap deletion").await;
    wait_generation(&catalog, &bucket, "test-bucket/removed", removed).await;
    catalog.disable_change_journal().await.unwrap();
    let generation = publish(&catalog, &bucket, "body", b"after gap").await;
    catalog
        .admin(Origin::Api)
        .delete_object(
            &bucket,
            &ObjectName::new("removed").unwrap(),
            GenerationCondition::Is(gcs_mimic::storage::admin::Generation::new(removed).unwrap()),
        )
        .await
        .unwrap();
    catalog.enable_change_journal().await.unwrap();
    wait_generation(&catalog, &bucket, "test-bucket/body", generation).await;
    wait_body(&tree.0.join("test-bucket/body"), Some(b"after gap")).await;
    worker.finish().await.unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn live_export_http_copy_compose_and_bucket_creation_keep_deleted_bucket_directories() {
    let data = TestDir::new();
    let tree = TestDir::new();
    let mut server = SyncServer::start(&data.0, &tree.0);
    server
        .request(
            "POST",
            "/storage/v1/b?project=owner",
            br#"{"name":"test-bucket"}"#,
        )
        .json_ok();
    server
        .request(
            "POST",
            "/storage/v1/b?project=owner",
            br#"{"name":"empty-bucket"}"#,
        )
        .json_ok();
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=source",
            b"\0\xffsource",
        )
        .json_ok();
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/source/copyTo/b/test-bucket/o/copied",
            b"",
        )
        .json_ok();
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/composed/compose",
            br#"{"sourceObjects":[{"name":"source"},{"name":"copied"}]}"#,
        )
        .json_ok();
    wait_body(&tree.0.join("test-bucket/copied"), Some(b"\0\xffsource")).await;
    wait_body(
        &tree.0.join("test-bucket/composed"),
        Some(b"\0\xffsource\0\xffsource"),
    )
    .await;
    wait_body(&tree.0.join("test-bucket/removed"), None).await;
    common::input_fixtures::eventually_async("the empty bucket directory exported", || {
        std::future::ready(tree.0.join("empty-bucket").is_dir())
    })
    .await;
    server
        .request("DELETE", "/storage/v1/b/empty-bucket", b"")
        .expect(204);
    server.stop();
    assert!(tree.0.join("empty-bucket").is_dir());
}

#[tokio::test]
async fn live_export_sigterm_during_api_writes_restarts_to_the_committed_live_body() {
    use std::{
        io::{Read, Write},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    let data = TestDir::new();
    let tree = TestDir::new();
    let mut server = SyncServer::start(&data.0, &tree.0);
    server
        .request(
            "POST",
            "/storage/v1/b?project=owner",
            br#"{"name":"test-bucket"}"#,
        )
        .json_ok();
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=body",
            b"initial",
        )
        .json_ok();
    wait_body(&tree.0.join("test-bucket/body"), Some(b"initial")).await;
    let address = server.address.clone();
    let running = Arc::new(AtomicBool::new(true));
    let writer_running = running.clone();
    let (started, ready) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let mut accepted = 0;
        for sequence in 0..100 {
            if !writer_running.load(Ordering::SeqCst) {
                break;
            }
            let body = format!("committed-{sequence}");
            let Ok(mut connection) = std::net::TcpStream::connect(&address) else {
                break;
            };
            connection
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = format!(
                "POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=body HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            if connection.write_all(request.as_bytes()).is_err() {
                break;
            }
            let mut response = String::new();
            if connection.read_to_string(&mut response).is_err()
                || !response.starts_with("HTTP/1.1 200")
            {
                break;
            }
            accepted += 1;
            if accepted == 4 {
                started.send(()).unwrap();
            }
        }
        accepted
    });
    ready.recv_timeout(Duration::from_secs(10)).unwrap();
    server.stop();
    running.store(false, Ordering::SeqCst);
    assert!(writer.join().unwrap() >= 4);
    let catalog = Catalog::open(&data.0).await.unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    let (_, reader) = catalog
        .admin(Origin::Api)
        .open_object(&bucket, &ObjectName::new("body").unwrap(), None)
        .await
        .unwrap();
    let mut body = Vec::new();
    reader
        .into_file()
        .await
        .unwrap()
        .read_to_end(&mut body)
        .unwrap();
    catalog.close().await.unwrap();
    let mut restarted = SyncServer::start(&data.0, &tree.0);
    wait_body(&tree.0.join("test-bucket/body"), Some(&body)).await;
    restarted.stop();
    assert_eq!(fs::read(tree.0.join("test-bucket/body")).unwrap(), body);
    assert_eq!(
        fs::read_dir(tree.0.join(".gcs-mimic-sync-work"))
            .unwrap()
            .count(),
        0
    );
}
