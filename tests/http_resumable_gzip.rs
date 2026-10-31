mod common;
mod ingress_support;

use common::TestDir;
use ingress_support::{Server, gzip};

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
    rusqlite::Connection::open_with_flags(
        dir.0.join("catalog.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
    .query_row("SELECT count(*) FROM resumable_sessions", [], |row| {
        row.get(0)
    })
    .unwrap()
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
    assert_eq!(initiated.status, 200, "{}", initiated.body);
    assert!(initiated.body.is_empty());
    assert_eq!(sessions(&dir), 1);
    let path = session_path(&initiated.headers);
    let rejected = server.request("POST", &path, GZIP, b"chunk bytes!");
    rejected.status(400);
    assert!(rejected.body.contains("content-encoding"));
    assert_eq!(
        server
            .request("PUT", &path, "Content-Range: bytes */12\r\n", b"")
            .status,
        410
    );
    let restarted = server.request("POST", PATH, GZIP, &gzip(metadata.as_bytes()));
    assert_eq!(restarted.status, 200, "{}", restarted.body);
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

#[test]
fn invalid_gzip_initiations_never_create_sessions() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    bucket(&server, "");
    let valid = gzip(br#"{"name":"object"}"#);
    let mut crc = valid.clone();
    crc[valid.len() - 8] ^= 1;
    let mut size = valid.clone();
    size[valid.len() - 4] ^= 1;
    for invalid in [
        b"invalid".to_vec(),
        valid[..valid.len() - 1].to_vec(),
        crc,
        size,
        [valid.as_slice(), b"garbage"].concat(),
    ] {
        server.request("POST", PATH, GZIP, &invalid).status(400);
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
        valid,
        b"\r\n0\r\nx-unexpected: trailer\r\n\r\n".to_vec(),
    ]
    .concat();
    server.raw(&format!("POST {PATH} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nTransfer-Encoding: chunked\r\nTrailer: x-unexpected\r\n{GZIP}\r\n"), &body).status(400);
    assert_eq!(sessions(&dir), 0);
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn gzip_initiation_bounds_compressed_and_expanded_metadata() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    bucket(&server, "");
    let mut metadata = br#"{"name":"object"}"#.to_vec();
    metadata.resize(64 * 1024, b' ');
    let initiated = server.request("POST", PATH, GZIP, &gzip(&metadata));
    assert_eq!(initiated.status, 200, "{}", initiated.body);
    assert!(initiated.body.is_empty());
    assert!(session_path(&initiated.headers).contains("upload_id="));
    assert_eq!(sessions(&dir), 1);
    metadata.push(b' ');
    server
        .request("POST", PATH, GZIP, &gzip(&metadata))
        .status(413);
    server
        .request("POST", PATH, GZIP, &vec![0; 64 * 1024 + 1])
        .status(413);
    assert_eq!(sessions(&dir), 1);
}

#[test]
fn gzip_initiation_retains_strict_authentication_and_upload_permissions() {
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
    for (token, body, expected) in [
        ("invalid", b"invalid gzip".to_vec(), 401),
        ("viewer", gzip(br#"{"name":"object"}"#), 403),
    ] {
        server
            .request(
                "POST",
                PATH,
                &format!("{GZIP}Authorization: Bearer {token}\r\n"),
                &body,
            )
            .status(expected);
        assert_eq!(sessions(&dir), 0);
    }
    let initiated = server.request(
        "POST",
        PATH,
        &format!("{GZIP}Authorization: Bearer admin\r\n"),
        &gzip(br#"{"name":"object"}"#),
    );
    assert_eq!(initiated.status, 200, "{}", initiated.body);
    assert!(initiated.body.is_empty());
    assert!(session_path(&initiated.headers).contains("upload_id="));
}
