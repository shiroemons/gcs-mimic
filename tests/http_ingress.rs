mod common;
mod ingress_support;

use common::TestDir;
use ingress_support::{Server, gzip};

const JSON: &str = "Content-Type: application/json\r\n";
const GZIP: &str = "Content-Type: application/json\r\nContent-Encoding: gzip\r\n";
const OVERRIDE: &str = "Content-Type: application/json\r\nX-HTTP-Method-Override: PATCH\r\n";
const BUCKET: &str = "/storage/v1/b/ingress-bucket";

#[test]
fn gzip_json_and_post_patch_override_preserve_conditions_and_object_bytes() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    server
        .request(
            "POST",
            "/storage/v1/b?project=project-a",
            GZIP,
            &gzip(br#"{"name":"ingress-bucket"}"#),
        )
        .status(200);
    let updated = server.request(
        "POST",
        &format!("{BUCKET}?ifMetagenerationMatch=1"),
        OVERRIDE,
        br#"{"labels":{"key":"value"}}"#,
    );
    updated.status(200);
    assert_eq!(updated.json()["labels"]["key"], "value");
    server
        .request(
            "POST",
            &format!("{BUCKET}?ifMetagenerationMatch=1"),
            OVERRIDE,
            br#"{"labels":{"key":"lost"}}"#,
        )
        .status(412);
    server
        .request(
            "POST",
            "/upload/storage/v1/b/ingress-bucket/o?uploadType=media&name=object",
            "Content-Type: text/plain\r\n",
            b"unchanged",
        )
        .status(200);
    let headers = format!("{GZIP}X-HTTP-Method-Override: PATCH\r\n");
    let updated = server.request(
        "POST",
        &format!("{BUCKET}/o/object?ifMetagenerationMatch=1"),
        &headers,
        &gzip(br#"{"metadata":{"key":"value"}}"#),
    );
    updated.status(200);
    assert_eq!(updated.json()["metadata"]["key"], "value");
    assert_eq!(
        server
            .request("GET", &format!("{BUCKET}/o/object?alt=media"), "", b"")
            .body,
        "unchanged"
    );
    assert_eq!(
        server.request("GET", BUCKET, "", b"").json()["labels"]["key"],
        "value"
    );
}

#[test]
fn gzip_corruption_members_and_trailers_never_mutate_metadata() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    server
        .request(
            "POST",
            "/storage/v1/b?project=project-a",
            JSON,
            br#"{"name":"ingress-bucket"}"#,
        )
        .status(200);
    let valid = gzip(br#"{"labels":{"key":"value"}}"#);
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
        server.request("PATCH", BUCKET, GZIP, &invalid).status(400);
        assert_eq!(
            server.request("GET", BUCKET, "", b"").json()["metageneration"],
            "1"
        );
    }
    let members = [gzip(br#"{"labels":"#), gzip(br#"{"key":"value"}}"#)].concat();
    server.request("PATCH", BUCKET, GZIP, &members).status(200);
    let chunked = format!("{:x}\r\n", valid.len()).into_bytes();
    let body = [
        chunked,
        valid,
        b"\r\n0\r\nx-unexpected: trailer\r\n\r\n".to_vec(),
    ]
    .concat();
    server.raw(&format!("PATCH {BUCKET} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nTransfer-Encoding: chunked\r\nTrailer: x-unexpected\r\n{GZIP}\r\n"), &body).status(400);
    assert_eq!(
        server.request("GET", BUCKET, "", b"").json()["metageneration"],
        "2"
    );
}

#[test]
fn gzip_input_and_expansion_limits_and_override_scope_fail_closed() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    server
        .request(
            "POST",
            "/storage/v1/b?project=project-a",
            JSON,
            br#"{"name":"ingress-bucket"}"#,
        )
        .status(200);
    let mut exact = br#"{"labels":{}}"#.to_vec();
    exact.resize(64 * 1024, b' ');
    server
        .request("PATCH", BUCKET, GZIP, &gzip(&exact))
        .status(200);
    exact.push(b' ');
    server
        .request("PATCH", BUCKET, GZIP, &gzip(&exact))
        .status(413);
    server
        .request("PATCH", BUCKET, GZIP, &vec![0; 64 * 1024 + 1])
        .status(413);
    for (method, path, headers) in [
        ("PATCH", BUCKET, OVERRIDE.to_string()),
        (
            "POST",
            BUCKET,
            format!("{JSON}X-HTTP-Method-Override: DELETE\r\n"),
        ),
        (
            "POST",
            BUCKET,
            format!("{OVERRIDE}X-HTTP-Method-Override: PATCH\r\n"),
        ),
        ("POST", "/ingress-bucket/object", OVERRIDE.to_string()),
        (
            "POST",
            "/upload/storage/v1/b/ingress-bucket/o?uploadType=media&name=wrong",
            OVERRIDE.to_string(),
        ),
        ("POST", "/_internal/health", OVERRIDE.to_string()),
    ] {
        server.request(method, path, &headers, b"{}").status(400);
    }
    assert_eq!(
        server.request("GET", BUCKET, "", b"").json()["metageneration"],
        "2"
    );
}

#[test]
fn gzip_and_override_use_effective_patch_permissions() {
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
    server
        .request(
            "POST",
            "/storage/v1/b?project=project-a",
            &format!("{GZIP}Authorization: Bearer admin\r\n"),
            &gzip(br#"{"name":"ingress-bucket"}"#),
        )
        .status(200);
    for token in ["viewer", "invalid"] {
        let headers =
            format!("{GZIP}X-HTTP-Method-Override: PATCH\r\nAuthorization: Bearer {token}\r\n");
        server
            .request(
                "POST",
                BUCKET,
                &headers,
                &gzip(br#"{"labels":{"key":"denied"}}"#),
            )
            .status(if token == "viewer" { 403 } else { 401 });
    }
    let resource = server.request("GET", BUCKET, "Authorization: Bearer admin\r\n", b"");
    resource.status(200);
    assert_eq!(resource.json()["metageneration"], "1");
}
