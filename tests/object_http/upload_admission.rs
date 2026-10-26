use super::*;
use std::sync::{Arc, Barrier};

#[test]
fn simultaneous_uploads_follow_the_connection_limit_and_keep_reads_working() {
    for (connections, count) in [(32, 12), (32, 40), (64, 64)] {
        let dir = TestDir::new();
        let server = Server::with_connections(&dir, 1_000_000, 10_000, 30_000, 10_000, connections);
        server.bucket();
        assert_eq!(server.upload("name=existing", "", b"existing").status, 200);
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
                    let reply = Reply::read(stream);
                    assert_eq!(reply.status, 200, "{}", String::from_utf8_lossy(&reply.body));
                }));
            }
            start.wait();
            if count == 40 {
                assert_eq!(server.get("existing?alt=media").body, b"existing");
                assert_eq!(
                    server
                        .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
                        .status,
                    200
                );
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
        assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
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
                    assert_eq!(init.status, 200, "{}", String::from_utf8_lossy(&init.body));
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
                    assert_eq!(
                        reply.status,
                        200,
                        "{}",
                        String::from_utf8_lossy(&reply.body)
                    );
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
        assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
    }
}

fn wait_for_stages(dir: &TestDir, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_dir(dir.0.join("staging")).unwrap().count() != expected {
        assert!(
            Instant::now() < deadline,
            "staging count did not reach {expected}"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn disconnect_cleans_staging_and_shutdown_waits_for_upload_completion() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, 1000, 5000);
    server.bucket();
    let mut disconnected = server.stream();
    disconnected.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=aborted HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\na").unwrap();
    wait_for_stages(&dir, 1);
    drop(disconnected);
    wait_for_stages(&dir, 0);
    assert_eq!(server.get("aborted").status, 404);
    let mut upload = server.stream();
    upload.write_all(b"POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=completed HTTP/1.1\r\nHost: local\r\nContent-Length: 3\r\n\r\na").unwrap();
    wait_for_stages(&dir, 1);
    thread::scope(|scope| {
        let stop = scope.spawn(|| server.stop());
        thread::sleep(Duration::from_millis(100));
        upload.write_all(b"bc").unwrap();
        assert_eq!(Reply::read(upload).status, 200);
        stop.join().unwrap();
    });
    wait_for_stages(&dir, 0);
    let mut reopened = Server::start(&dir, 1000, 5000);
    assert_eq!(reopened.get("completed?alt=media").body, b"abc");
    reopened.stop();
}
