use crate::common;

use common::TestDir;
use common::ingress::{Server, gzip};

const PATH: &str = "/upload/storage/v1/b/gzip-bucket/o?uploadType=resumable&ifGenerationMatch=0";
const GZIP: &str = "Content-Type: application/json; charset=utf-8\r\nContent-Encoding: gzip\r\n";

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

fn sessions(dir: &TestDir) -> i64 {
    common::catalog::count(
        &rusqlite::Connection::open_with_flags(
            dir.0.join("catalog.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap(),
        "resumable_sessions",
    )
}

fn session_path(headers: &str) -> String {
    let location = headers
        .lines()
        .find_map(|line| line.strip_prefix("location: "))
        .unwrap();
    format!("/{}", location.splitn(4, '/').nth(3).unwrap())
}

#[test]
fn gzip_initiation_metadata_keeps_chunks_and_stored_encoding_separate() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    bucket(&server, "");
    let metadata = serde_json::json!({
        "bucket":"gzip-bucket", "contentType":"application/octet-stream",
        "name":"stream/日本語 +%?.bin", "metadata":{"fixture":"value"}
    })
    .to_string();
    let members = [
        gzip(&metadata.as_bytes()[..20]),
        gzip(&metadata.as_bytes()[20..]),
    ]
    .concat();
    let initiated = server.request(
        "POST",
        PATH,
        &format!("{GZIP}X-Upload-Content-Type: application/octet-stream\r\nX-Upload-Content-Length: 12\r\n"),
        &members,
    );
    initiated.expect(200);
    assert!(initiated.body.is_empty());
    assert_eq!(sessions(&dir), 1);
    let path = session_path(&initiated.headers);
    let rejected = server.request("POST", &path, GZIP, b"chunk bytes!");
    rejected.status(400);
    assert!(rejected.body.contains("content-encoding"));
    server
        .request("PUT", &path, "Content-Range: bytes */12\r\n", b"")
        .expect(410);
    let restarted = server.request("POST", PATH, GZIP, &gzip(metadata.as_bytes()));
    restarted.expect(200);
    assert_eq!(sessions(&dir), 2);
    let path = session_path(&restarted.headers);
    let uploaded = server.request(
        "PUT",
        &path,
        "Content-Range: bytes 0-11/12\r\n",
        b"chunk bytes!",
    );
    uploaded.status(200);
    assert_eq!(uploaded.json()["name"], "stream/日本語 +%?.bin");
    assert_eq!(uploaded.json()["metadata"]["fixture"], "value");
    assert!(uploaded.json().get("contentEncoding").is_none());
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/gzip-bucket/o/stream%2F%E6%97%A5%E6%9C%AC%E8%AA%9E%20%2B%25%3F.bin?alt=media",
                "",
                b"",
            )
            .body,
        "chunk bytes!"
    );
}

/// Cloud Storage does not compare a member's trailer with its data (recorded 2026-11-25), so an
/// initiation whose CRC-32 or size is wrong starts a session for the metadata it carries.
#[test]
fn invalid_gzip_initiations_never_create_sessions() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    bucket(&server, "");
    let valid = gzip(br#"{"name":"object"}"#);
    for invalid in [
        b"invalid".to_vec(),
        valid[..valid.len() - 1].to_vec(),
        valid[..valid.len() - 8].to_vec(),
        [valid.as_slice(), b"garbage"].concat(),
    ] {
        server.request("POST", PATH, GZIP, &invalid).undecodable();
        assert_eq!(sessions(&dir), 0);
    }
    for encoding in ["br", "gzip, gzip", "gzip\r\nContent-Encoding: gzip"] {
        server
            .request(
                "POST",
                PATH,
                &format!("Content-Type: application/json\r\nContent-Encoding: {encoding}\r\n"),
                &valid,
            )
            .status(400);
    }
    let body = [
        format!("{:x}\r\n", valid.len()).into_bytes(),
        valid.clone(),
        b"\r\n0\r\nx-unexpected: trailer\r\n\r\n".to_vec(),
    ]
    .concat();
    server.raw(&format!("POST {PATH} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nTransfer-Encoding: chunked\r\nTrailer: x-unexpected\r\n{GZIP}\r\n"), &body).status(400);
    assert_eq!(sessions(&dir), 0);
    assert_eq!(common::process::staging_count(&dir), 0);
    for field in [8, 4] {
        let mut body = valid.clone();
        body[valid.len() - field] ^= 1;
        server.request("POST", PATH, GZIP, &body).expect(200);
    }
    assert_eq!(sessions(&dir), 2);
}

#[test]
fn gzip_initiation_bounds_compressed_and_expanded_metadata() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    bucket(&server, "");
    const BODY_LIMIT: usize = 16 * 1024 * 1024;
    let mut metadata = br#"{"name":"object"}"#.to_vec();
    metadata.resize(BODY_LIMIT, b' ');
    let initiated = server.request("POST", PATH, GZIP, &gzip(&metadata));
    initiated.expect(200);
    assert!(initiated.body.is_empty());
    assert!(session_path(&initiated.headers).contains("upload_id="));
    assert_eq!(sessions(&dir), 1);
    metadata.push(b' ');
    server
        .request("POST", PATH, GZIP, &gzip(&metadata))
        .status(413);
    server
        .request("POST", PATH, GZIP, &vec![0; BODY_LIMIT + 1])
        .status(413);
    assert_eq!(sessions(&dir), 1);
}

/// Cloud Storage expands the initiation's metadata before it checks the caller (recorded
/// 2026-11-25): a body that expands past the limit or does not decode answers 413 or 400 to a
/// caller who would be refused, and a body that decodes meets the caller's refusal.
#[test]
fn gzip_initiation_expands_before_strict_authentication_and_upload_permissions() {
    let dir = TestDir::new();
    let registry = common::process::write_identities(
        &dir.0,
        &serde_json::json!({"identities":[
            {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"project-a":["roles/storage.admin"]}},
            {"id":"viewer@example.invalid","tokens":["viewer"],"project_roles":{"project-a":["roles/storage.objectViewer"]}}
        ]}),
    );
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
    let bomb = gzip(&vec![b' '; 16 * 1024 * 1024 + 1]);
    let metadata = gzip(br#"{"name":"object"}"#);
    for (auth, refusal) in [
        ("", 401),
        ("Authorization: Bearer invalid\r\n", 401),
        ("Authorization: Bearer viewer\r\n", 403),
    ] {
        let headers = format!("{GZIP}{auth}");
        server.request("POST", PATH, &headers, &bomb).status(413);
        server
            .request("POST", PATH, &headers, b"invalid gzip")
            .undecodable();
        server
            .request("POST", PATH, &headers, &metadata)
            .status(refusal);
        assert_eq!(sessions(&dir), 0);
    }
    let initiated = server.request(
        "POST",
        PATH,
        &format!("{GZIP}Authorization: Bearer admin\r\n"),
        &gzip(br#"{"name":"object"}"#),
    );
    initiated.expect(200);
    assert!(initiated.body.is_empty());
    assert!(session_path(&initiated.headers).contains("upload_id="));
}
