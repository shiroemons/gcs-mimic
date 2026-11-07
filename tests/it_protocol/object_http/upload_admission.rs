use super::*;
use std::sync::{Arc, Barrier};

#[test]
fn simultaneous_uploads_follow_the_connection_limit_and_keep_reads_working() {
    for (connections, count) in [(32, 12), (32, 40), (64, 64)] {
        let dir = TestDir::new();
        let server = Server::with_connections(&dir, 1_000_000, 10_000, 30_000, 10_000, connections);
        server.bucket();
        server.upload("name=existing", "", b"existing").expect(200);
        let start = Arc::new(Barrier::new(count + 1));
        thread::scope(|scope| {
            let mut uploads = Vec::new();
            for index in 0..count {
                let start = start.clone();
                let server = &server;
                uploads.push(scope.spawn(move || {
                    let mut stream = server.stream();
                    stream.write_all(format!("POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=parallel-{index} HTTP/1.1\r\nHost: local\r\nContent-Length: 131072\r\n\r\n").as_bytes()).unwrap();
                    start.wait();
                    for _ in 0..2 {
                        stream.write_all(&[b'x'; 65536]).unwrap();
                        thread::yield_now();
                    }
                    let reply = Reply::framed(stream);
                    reply.expect(200);
                }));
            }
            start.wait();
            if count == 40 {
                assert_eq!(server.get("existing?alt=media").body, b"existing");
                server
                    .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
                    .expect(200);
            }
            for upload in uploads {
                upload.join().unwrap();
            }
        });
        for index in 0..count {
            assert_eq!(
                server.get(&format!("parallel-{index}")).json()["size"],
                "131072"
            );
        }
        assert_eq!(common::process::staging_count(&dir), 0);
    }
}

#[test]
fn simultaneous_resumable_initiation_and_final_chunks_wait_for_catalog_admission() {
    for (connections, count) in [(32, 40), (64, 64)] {
        let dir = TestDir::new();
        let server = Server::with_connections(&dir, 1_000_000, 10_000, 30_000, 10_000, connections);
        server.bucket();
        let start = Arc::new(Barrier::new(count));
        thread::scope(|scope| {
            let mut uploads = Vec::new();
            for index in 0..count {
                let server = &server;
                let start = start.clone();
                uploads.push(scope.spawn(move || {
                    start.wait();
                    let init = server.request(
                        "POST",
                        &format!("/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=session-{index}"),
                        "",
                        b"",
                    );
                    init.expect(200);
                    let location = &init.headers["location"];
                    let path = location
                        .strip_prefix("https://advertised.invalid/api")
                        .unwrap();
                    let reply = server.request(
                        "PUT",
                        path,
                        "Content-Range: bytes 0-131071/131072\r\n",
                        &[b'x'; 131072],
                    );
                    reply.expect(200);
                }));
            }
            for upload in uploads {
                upload.join().unwrap();
            }
        });
        for index in 0..count {
            assert_eq!(
                server.get(&format!("session-{index}")).json()["size"],
                "131072"
            );
        }
        assert_eq!(common::process::staging_count(&dir), 0);
    }
}

#[test]
fn a_retry_takes_over_the_session_when_the_abandoned_chunk_ends() {
    let dir = TestDir::new();
    let server = Server::with_connections(&dir, 1_000_000, 10_000, 30_000, 60_000, 8);
    server.bucket();
    let init = server.request(
        "POST",
        "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=retried",
        "",
        b"",
    );
    init.expect(200);
    let path = init.headers["location"]
        .strip_prefix("https://advertised.invalid/api")
        .unwrap();
    let mut abandoned = server.stream();
    abandoned
        .write_all(
            format!(
                "PUT {path} HTTP/1.1\r\nHost: local\r\nContent-Length: 262144\r\nContent-Range: bytes 0-262143/*\r\n\r\na"
            )
            .as_bytes(),
        )
        .unwrap();
    common::eventually("the abandoned chunk to reach the session", || {
        // Not `DirEntry::metadata`: on Windows that is the size the directory listing recorded,
        // which does not follow a file the server still holds open.
        std::fs::read_dir(dir.0.join("staging"))
            .unwrap()
            .any(|entry| std::fs::metadata(entry.unwrap().path()).unwrap().len() > 0)
    });
    let mut retry = server.stream();
    retry
        .write_all(
            format!(
                "PUT {path} HTTP/1.1\r\nHost: local\r\nContent-Length: 0\r\nContent-Range: bytes */*\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
    // A round trip through the catalog lets the server reach the retry before the old chunk
    // ends. Without a writer wait the retry would already have its 503 by now.
    server
        .request("GET", "/storage/v1/b/test-bucket", "", b"")
        .expect(200);
    drop(abandoned);
    Reply::framed(retry).expect(308);
    server
        .request("PUT", path, "Content-Range: bytes 0-4/5\r\n", b"bytes")
        .expect(200);
}

/// 128 active sessions used to fail further uploads and any more to refuse the next startup.
#[test]
fn sessions_past_the_former_cap_survive_a_restart_and_stay_usable() {
    let dir = TestDir::new();
    let mut server = Server::with_connections(&dir, 1_000_000, 10_000, 30_000, 10_000, 8);
    server.bucket();
    let paths: Vec<String> = (0..300)
        .map(|index| {
            let init = server.request(
                "POST",
                &format!("/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=s-{index}"),
                "",
                b"",
            );
            init.expect(200);
            init.headers["location"]
                .strip_prefix("https://advertised.invalid/api")
                .unwrap()
                .to_owned()
        })
        .collect();
    server.stop();
    let mut restarted = Server::with_connections(&dir, 1_000_000, 10_000, 30_000, 10_000, 8);
    restarted
        .request("PUT", &paths[0], "Content-Range: bytes */*\r\n", b"")
        .expect(308);
    restarted
        .request(
            "PUT",
            &paths[299],
            "Content-Range: bytes 0-4/5\r\n",
            b"bytes",
        )
        .expect(200);
    assert_eq!(restarted.get("s-299?alt=media").body, b"bytes");
    restarted.stop();
}

#[test]
fn a_configured_session_limit_reaches_the_server() {
    let dir = TestDir::new();
    let mut server = Server::launch(http_command(&dir.0).args([
        "--max-resumable-sessions",
        "1",
        "--api-base-url",
        "https://advertised.invalid/api",
    ]));
    server.bucket();
    let initiate = "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=object";
    server.request("POST", initiate, "", b"").expect(200);
    let refused = server.request("POST", initiate, "", b"");
    refused.expect(503);
    assert!(
        String::from_utf8_lossy(&refused.body).contains("max_resumable_sessions"),
        "{}",
        String::from_utf8_lossy(&refused.body)
    );
    server.stop();
}

#[test]
fn disconnect_cleans_staging_and_shutdown_waits_for_upload_completion() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let mut disconnected = server.stream();
    disconnected.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=aborted HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\na").unwrap();
    common::eventually("the aborted upload staged", || {
        common::process::staging_count(&dir) == 1
    });
    drop(disconnected);
    common::eventually("the aborted stage cleaned", || {
        common::process::staging_count(&dir) == 0
    });
    server.get("aborted").expect(404);
    let mut upload = server.stream();
    upload.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=completed HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\na").unwrap();
    common::eventually("the completing upload staged", || {
        common::process::staging_count(&dir) == 1
    });
    thread::scope(|scope| {
        let stop = scope.spawn(|| server.stop());
        thread::sleep(Duration::from_millis(100));
        upload.write_all(b"bc").unwrap();
        Reply::framed(upload).expect(200);
        stop.join().unwrap();
    });
    common::eventually("the completed stage published", || {
        common::process::staging_count(&dir) == 0
    });
    let mut reopened = Server::start(&dir, 1000, 5000);
    assert_eq!(reopened.get("completed?alt=media").body, b"abc");
    reopened.stop();
}
