use super::*;
use std::process::Command;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn stopping_during_publication_and_journal_maintenance_emits_no_warnings() {
    const CHILD: &str = "GCSM_TEST_STOP_DURING_RECLAMATION";
    if std::env::var_os(CHILD).is_none() {
        let name = std::thread::current().name().unwrap().to_owned();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.lines().any(|line| line == r#"{"event":"stopped"}"#),
            "{stdout}"
        );
        assert!(stdout.contains("1 passed"), "{stdout}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap().trim_end(),
            r#"{"event":"stopping","level":"info"}"#
        );
        return;
    }

    let dir = TestDir::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = test_config(
        &dir.0,
        &[
            "--pubsub-emulator-host",
            &listener.local_addr().unwrap().to_string(),
        ],
    );
    let options = CatalogOptions::for_server(&config, &Auth::permissive(), Arc::new(SystemClock));
    let catalog = Catalog::open_with_options((&config).into(), options)
        .await
        .unwrap();
    let database = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    database
        .execute_batch(
            "UPDATE notification_outbox_state SET last_id=1;
             INSERT INTO notification_outbox VALUES
             (1,'//pubsub.googleapis.com/projects/p/topics/topic','{}',0,0);",
        )
        .unwrap();
    catalog.start_notifications(&config).await.unwrap();
    let (connection, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut publisher = BufReader::new(connection);
    let mut request = String::new();
    publisher.read_line(&mut request).await.unwrap();
    assert!(
        request.starts_with("POST ") && request.contains(":publish"),
        "{request}"
    );

    let id = "0123456789abcdef0123456789abcdef";
    let shard = dir.0.join("blobs/01");
    fs::create_dir_all(&shard).unwrap();
    let blob = shard.join(id);
    fs::write(&blob, b"retired").unwrap();
    database
        .execute("INSERT INTO pending_blob_deletions VALUES (?1)", [id])
        .unwrap();
    // An absent file with its journal row still locked places the running pass between
    // unlink and commit. The publisher independently holds its attempt until we reply.
    database.execute_batch("BEGIN IMMEDIATE").unwrap();
    catalog.start_resumable_maintenance();
    crate::test_support::eventually_async("maintenance to unlink the journaled blob", || async {
        !blob.exists()
    })
    .await;
    let stopping = finish_startup(&catalog, shutdown_result(Ok(())));
    tokio::pin!(stopping);
    tokio::select! {
        biased;
        _ = &mut stopping => panic!("shutdown did not wait for the running work"),
        () = std::future::ready(()) => {}
    }
    assert!(!catalog.is_healthy());
    database.execute_batch("COMMIT").unwrap();
    publisher
        .get_mut()
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{\"messageIds\":[\"1\"]}")
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), stopping)
        .await
        .unwrap()
        .unwrap();
    let pending: usize = database
        .query_row("SELECT count(*) FROM pending_blob_deletions", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(pending, 0);
}
