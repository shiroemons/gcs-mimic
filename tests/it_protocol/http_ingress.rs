use crate::common;

use common::TestDir;
use common::ingress::{Server, gzip};

const JSON: &str = "Content-Type: application/json\r\n";
const GZIP: &str = "Content-Type: application/json\r\nContent-Encoding: gzip\r\n";
const OVERRIDE: &str = "Content-Type: application/json\r\nX-HTTP-Method-Override: PATCH\r\n";
const BUCKET: &str = "/storage/v1/b/ingress-bucket";
const BODY_LIMIT: usize = 16 * 1024 * 1024;

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

/// Cloud Storage refuses a body that ends inside a gzip member or continues past the last one,
/// but it does not compare a member's trailer with its data (recorded 2026-11-25): a wrong
/// CRC-32 or size decodes as the data it carries.
#[test]
fn gzip_bodies_that_do_not_decode_never_mutate_metadata() {
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
    let metageneration = || server.request("GET", BUCKET, "", b"").json()["metageneration"].clone();
    let valid = gzip(br#"{"labels":{"key":"value"}}"#);
    for invalid in [
        b"invalid".to_vec(),
        valid[..valid.len() - 1].to_vec(),
        valid[..valid.len() - 8].to_vec(),
        [valid.as_slice(), b"garbage"].concat(),
    ] {
        server
            .request("PATCH", BUCKET, GZIP, &invalid)
            .undecodable();
        assert_eq!(metageneration(), "1");
    }
    let mut crc = gzip(br#"{"labels":{"key":"crc"}}"#);
    let checksum = crc.len() - 8;
    crc[checksum] ^= 1;
    let mut size = gzip(br#"{"labels":{"key":"size"}}"#);
    let length = size.len() - 4;
    size[length] ^= 1;
    for (label, body) in [("crc", crc), ("size", size)] {
        let updated = server.request("PATCH", BUCKET, GZIP, &body);
        updated.status(200);
        assert_eq!(updated.json()["labels"]["key"], label);
    }
    let members = [gzip(br#"{"labels":"#), gzip(br#"{"key":"value"}}"#)].concat();
    server.request("PATCH", BUCKET, GZIP, &members).status(200);
    assert_eq!(metageneration(), "4");
    let chunked = format!("{:x}\r\n", valid.len()).into_bytes();
    let body = [
        chunked,
        valid,
        b"\r\n0\r\nx-unexpected: trailer\r\n\r\n".to_vec(),
    ]
    .concat();
    server.raw(&format!("PATCH {BUCKET} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nTransfer-Encoding: chunked\r\nTrailer: x-unexpected\r\n{GZIP}\r\n"), &body).status(400);
    assert_eq!(metageneration(), "4");
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
    exact.resize(BODY_LIMIT, b' ');
    let compressed = gzip(&exact);
    server
        .request("PATCH", BUCKET, GZIP, &compressed)
        .status(200);
    exact.push(b' ');
    // A small input that expands past the limit is refused by what it expands to: raising the
    // limit must not let a compressed body grow without bound.
    let bomb = gzip(&exact);
    assert!(bomb.len() < BODY_LIMIT / 100, "{}", bomb.len());
    server.request("PATCH", BUCKET, GZIP, &bomb).status(413);
    let far_larger = gzip(&vec![b' '; 8 * BODY_LIMIT]);
    assert!(far_larger.len() < BODY_LIMIT / 10, "{}", far_larger.len());
    server
        .request("PATCH", BUCKET, GZIP, &far_larger)
        .status(413);
    server
        .request("PATCH", BUCKET, GZIP, &vec![0; BODY_LIMIT + 1])
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
fn gzip_expands_before_authentication_and_override_uses_patch_permissions() {
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
    // Cloud Storage's front end expands a compressed body before it checks the caller (recorded
    // 2026-11-25), so a body that expands past the limit or does not decode answers 413 or 400
    // to a caller who would be refused, and the expansion stays bounded for every caller.
    let bomb = gzip(&vec![b' '; BODY_LIMIT + 1]);
    let viewer = format!("{GZIP}Authorization: Bearer viewer\r\n");
    for (method, path, headers) in [
        ("POST", "/storage/v1/b?project=project-a", GZIP),
        ("POST", "/storage/v1/b?project=project-a", viewer.as_str()),
        ("PATCH", BUCKET, GZIP),
        ("PATCH", BUCKET, viewer.as_str()),
        ("GET", BUCKET, GZIP),
    ] {
        server.request(method, path, headers, &bomb).status(413);
        server
            .request(method, path, headers, b"not valid gzip")
            .undecodable();
    }
    let resource = server.request("GET", BUCKET, "Authorization: Bearer admin\r\n", b"");
    resource.status(200);
    assert_eq!(resource.json()["metageneration"], "1");
}

/// A compressed body is expanded before the request is routed, so a body that decodes meets the
/// same refusal and CORS headers as the same body sent plain, and a body that does not decode
/// answers 400 with the CORS headers of its route (recorded 2026-11-25).
#[test]
fn gzip_and_plain_bodies_meet_the_same_refusal() {
    let dir = TestDir::new();
    let registry = common::process::write_identities(
        &dir.0,
        &serde_json::json!({"identities":[
            {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"project-a":["roles/storage.admin"]}},
            {"id":"viewer@example.invalid","tokens":["viewer"],"project_roles":{"project-a":["roles/storage.objectViewer"]}},
            {"id":"stranger@example.invalid","tokens":["stranger"]}
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
    let admin = "Authorization: Bearer admin\r\n";
    server
        .request(
            "POST",
            "/storage/v1/b?project=project-a",
            &format!("{JSON}{admin}"),
            br#"{"name":"ingress-bucket"}"#,
        )
        .status(200);
    server
        .request(
            "POST",
            "/upload/storage/v1/b/ingress-bucket/o?uploadType=media&name=object",
            &format!("Content-Type: text/plain\r\n{admin}"),
            b"stored",
        )
        .status(200);
    let upload = "/upload/storage/v1/b/ingress-bucket/o";
    let stranger = "Authorization: Bearer stranger\r\n";
    for (method, path, headers, expected) in [
        (
            "PATCH",
            BUCKET.to_owned(),
            format!("{stranger}Origin: https://app.example\r\n"),
            403,
        ),
        (
            "POST",
            format!("{BUCKET}/o/object/rewriteTo/b/ingress-bucket/o/copy"),
            stranger.to_owned(),
            403,
        ),
        (
            "POST",
            format!("{upload}?uploadType=media&name=x&ifGenerationMatch=abc"),
            stranger.to_owned(),
            400,
        ),
        (
            "PATCH",
            format!("{BUCKET}/o/object"),
            "Authorization: Bearer viewer\r\n".to_owned(),
            403,
        ),
        // Cloud Storage decodes the query key, so this names a session that does not exist.
        (
            "POST",
            format!("{upload}?uploadType=resumable&name=r&upload%5Fid=zzz"),
            admin.to_owned(),
            404,
        ),
        ("POST", "/storage/v1/unknown".to_owned(), String::new(), 404),
    ] {
        let body = br#"{"labels":{"key":"value"}}"#;
        let plain = server.request(method, &path, &format!("{JSON}{headers}"), body);
        let compressed = server.request(method, &path, &format!("{GZIP}{headers}"), &gzip(body));
        assert_eq!(plain.status, expected, "{method} {path}: {}", plain.body);
        assert_eq!(compressed.status, plain.status, "{method} {path}");
        assert_eq!(compressed.body, plain.body, "{method} {path}");
        let cors =
            |reply: &common::ingress::Reply| reply.headers.contains("access-control-allow-origin");
        assert_eq!(cors(&compressed), cors(&plain), "{method} {path}");
    }
    let refused = server.request(
        "PATCH",
        BUCKET,
        &format!("{GZIP}{stranger}Origin: https://app.example\r\n"),
        &gzip(b"{}"),
    );
    assert!(
        refused.headers.contains("access-control-allow-origin"),
        "{}",
        refused.headers
    );
    for credentials in ["", stranger] {
        let anonymous = server.request(
            "PATCH",
            BUCKET,
            &format!("{GZIP}{credentials}Origin: https://app.example\r\n"),
            b"not gzip",
        );
        anonymous.undecodable();
        assert!(
            anonymous.headers.contains("access-control-allow-origin"),
            "{}",
            anonymous.headers
        );
    }
}
