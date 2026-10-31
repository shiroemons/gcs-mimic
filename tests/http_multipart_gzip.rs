mod common;
mod ingress_support;

use common::TestDir;
use ingress_support::{Server, gzip};
use std::{
    io::{BufReader, Write},
    time::{Duration, Instant},
};

const PATH: &str = "/upload/storage/v1/b/gzip-bucket/o?uploadType=multipart";
const OBJECT: &str = "/storage/v1/b/gzip-bucket/o/object";
const TYPE: &str =
    "Content-Type: multipart/related; boundary=upload-boundary\r\nContent-Encoding: gzip\r\n";

fn bucket(server: &Server, auth: &str) {
    server
        .request(
            "POST",
            "/storage/v1/b?project=project-a",
            &format!("Content-Type: application/json\r\n{auth}"),
            br#"{"name":"gzip-bucket"}"#,
        )
        .status(200);
}

fn multipart(metadata: &str, media: &[u8]) -> Vec<u8> {
    [format!("--upload-boundary\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n--upload-boundary\r\nContent-Type: text/plain\r\n\r\n").into_bytes(), media.to_vec(), b"\r\n--upload-boundary--\r\n".to_vec()].concat()
}

#[test]
fn multipart_transport_gzip_streams_large_bodies_members_and_chunk_splits() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--max-object-bytes", "4194304"]);
    bucket(&server, "");
    let media = "large multipart payload\n".repeat(100_000);
    let envelope = multipart(
        r#"{"name":"object","metadata":{"key":"value"}}"#,
        media.as_bytes(),
    );
    let compressed = gzip(&envelope);
    let result = server.request("POST", PATH, TYPE, &compressed);
    result.status(200);
    assert_eq!(result.json()["size"], media.len().to_string());
    assert_eq!(result.json()["metadata"]["key"], "value");
    assert!(result.json().get("contentEncoding").is_none());
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        media
    );
    let envelope = multipart(r#"{"name":"object"}"#, b"split gzip members");
    let members = [
        gzip(&envelope[..envelope.len() / 2]),
        gzip(&envelope[envelope.len() / 2..]),
    ]
    .concat();
    let mut chunks = Vec::new();
    for byte in members {
        chunks.extend_from_slice(&[b'1', b'\r', b'\n', byte, b'\r', b'\n']);
    }
    chunks.extend_from_slice(b"0\r\n\r\n");
    server.raw(&format!("POST {PATH} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n{TYPE}\r\n"), &chunks).status(200);
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        "split gzip members"
    );
}

#[test]
fn multipart_gzip_checks_crc_size_truncation_and_trailers_after_the_mime_close() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    bucket(&server, "");
    server
        .request(
            "POST",
            PATH,
            TYPE,
            &gzip(&multipart(
                r#"{"name":"object","metadata":{"old":"yes"}}"#,
                b"old content",
            )),
        )
        .status(200);
    let before = server.request("GET", OBJECT, "", b"").json();
    let valid = gzip(&multipart(
        r#"{"name":"object","metadata":{"new":"no"}}"#,
        b"new content",
    ));
    let mut crc = valid.clone();
    crc[valid.len() - 8] ^= 1;
    let mut size = valid.clone();
    size[valid.len() - 4] ^= 1;
    for invalid in [
        crc,
        size,
        valid[..valid.len() - 1].to_vec(),
        valid[..valid.len() - 8].to_vec(),
        [valid.as_slice(), b"garbage"].concat(),
        b"invalid".to_vec(),
    ] {
        server.request("POST", PATH, TYPE, &invalid).status(400);
        assert_eq!(server.request("GET", OBJECT, "", b"").json(), before);
        assert_eq!(
            server
                .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
                .body,
            "old content"
        );
    }
    let chunks = [
        format!("{:x}\r\n", valid.len()).into_bytes(),
        valid,
        b"\r\n0\r\nx-bad: trailer\r\n\r\n".to_vec(),
    ]
    .concat();
    server.raw(&format!("POST {PATH} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nTransfer-Encoding: chunked\r\nTrailer: x-bad\r\n{TYPE}\r\n"), &chunks).status(400);
    assert_eq!(server.request("GET", OBJECT, "", b"").json(), before);
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn multipart_gzip_keeps_stored_encoding_separate_and_never_decodes_raw_media() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    bucket(&server, "");
    let stored = gzip(b"stored gzip content");
    let envelope = multipart(r#"{"name":"object","contentEncoding":"gzip"}"#, &stored);
    let result = server.request("POST", PATH, TYPE, &gzip(&envelope));
    result.status(200);
    assert_eq!(result.json()["contentEncoding"], "gzip");
    assert_eq!(result.json()["size"], stored.len().to_string());
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        "stored gzip content"
    );
    let result = server.request(
        "POST",
        "/upload/storage/v1/b/gzip-bucket/o?uploadType=media&name=raw",
        "Content-Type: text/plain\r\nContent-Encoding: gzip\r\n",
        b"raw media is not transport gzip",
    );
    result.status(400);
    assert!(result.body.contains("content-encoding"));
    server
        .request("GET", "/storage/v1/b/gzip-bucket/o/raw", "", b"")
        .status(404);
}

#[test]
fn multipart_gzip_limits_and_declared_checksums_preserve_previous_objects() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--max-object-bytes", "64"]);
    bucket(&server, "");
    server
        .request(
            "POST",
            PATH,
            TYPE,
            &gzip(&multipart(r#"{"name":"object"}"#, b"old")),
        )
        .status(200);
    let before = server.request("GET", OBJECT, "", b"").json();
    let oversized = multipart(r#"{"name":"object"}"#, &[b'x'; 65]);
    server
        .request("POST", PATH, TYPE, &gzip(&oversized))
        .status(413);
    let checksum = multipart(
        r#"{"name":"object","md5Hash":"AAAAAAAAAAAAAAAAAAAAAA=="}"#,
        b"wrong hash",
    );
    server
        .request("POST", PATH, TYPE, &gzip(&checksum))
        .status(400);
    let epilogue = [
        multipart(r#"{"name":"object"}"#, b"new"),
        vec![b' '; 300_000],
    ]
    .concat();
    server
        .request("POST", PATH, TYPE, &gzip(&epilogue))
        .status(400);
    server
        .request("POST", PATH, TYPE, &vec![b'x'; 400_000])
        .status(400);
    assert_eq!(server.request("GET", OBJECT, "", b"").json(), before);
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn multipart_gzip_strict_permissions_precede_publication() {
    let dir = TestDir::new();
    let registry = dir.0.join("identities.json");
    std::fs::write(&registry, serde_json::json!({"identities":[
        {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"project-a":["roles/storage.admin"]}},
        {"id":"viewer@example.invalid","tokens":["viewer"],"project_roles":{"project-a":["roles/storage.objectViewer"]}}
    ]}).to_string()).unwrap();
    let server = Server::start(
        &dir,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            registry.to_str().unwrap(),
        ],
    );
    bucket(&server, "Authorization: Bearer admin\r\n");
    let envelope = gzip(&multipart(r#"{"name":"object"}"#, b"must not publish"));
    for (token, status) in [("viewer", 403), ("invalid", 401)] {
        server
            .request(
                "POST",
                PATH,
                &format!("{TYPE}Authorization: Bearer {token}\r\n"),
                &envelope,
            )
            .status(status);
    }
    server
        .request("GET", OBJECT, "Authorization: Bearer admin\r\n", b"")
        .status(404);
    server
        .request(
            "POST",
            PATH,
            &format!("{TYPE}Authorization: Bearer admin\r\n"),
            &envelope,
        )
        .status(200);
}

fn await_staging(dir: &TestDir, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::fs::read_dir(dir.0.join("staging")).unwrap().count() != count {
        assert!(
            Instant::now() < deadline,
            "staging did not reach expected count {count}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn multipart_gzip_disconnect_and_absolute_deadline_cancel_unvalidated_streams() {
    for timeout in [false, true] {
        let dir = TestDir::new();
        let server = Server::start(
            &dir,
            &[
                "--max-connections",
                "2",
                "--object-request-timeout-ms",
                if timeout { "250" } else { "5000" },
            ],
        );
        bucket(&server, "");
        let compressed = gzip(&multipart(
            r#"{"name":"object"}"#,
            b"complete MIME without a gzip trailer",
        ));
        let mut client = server.connect();
        write!(
            client,
            "POST {PATH} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n{TYPE}\r\n",
            compressed.len()
        )
        .unwrap();
        client
            .write_all(&compressed[..compressed.len() - 8])
            .unwrap();
        await_staging(&dir, 1);
        if timeout {
            let mut reader = BufReader::new(client);
            let reply = common::read_http_response(&mut reader, false).unwrap();
            let reply = std::str::from_utf8(&reply).unwrap();
            assert!(reply.starts_with("HTTP/1.1 408"), "{reply}");
        } else {
            drop(client);
        }
        await_staging(&dir, 0);
        server.request("GET", OBJECT, "", b"").status(404);
        server.request("POST", PATH, TYPE, &compressed).status(200);
    }
}
