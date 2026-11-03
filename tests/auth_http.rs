use crate::common::server::declare_server;
mod common;

#[path = "auth_http/iam.rs"]
mod iam;

#[path = "auth_http/xml.rs"]
mod xml;

#[path = "auth_http/xml_put.rs"]
mod xml_put;

#[path = "auth_http/cors.rs"]
mod cors;

#[path = "auth_http/signing.rs"]
mod signing;

#[path = "auth_http/virtual_host.rs"]
mod virtual_host;

use common::TestDir;
use common::server::{http_command, request_head};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

const ADMIN: &str = "Authorization: Bearer admin-token\r\n";
const VIEWER: &str = "Authorization: Bearer viewer-token\r\n";
const CREATOR: &str = "Authorization: Bearer creator-token\r\n";
const EDITOR: &str = "Authorization: Bearer editor-token\r\n";
const PROJECT_A_ONLY: &str = "Authorization: Bearer a-only-token\r\n";
const NO_ROLES: &str = "Authorization: Bearer no-roles-token\r\n";

fn registry() -> Value {
    json!({"identities": [
        {"id": "admin@example.invalid", "tokens": ["admin-token", "admin-token-2"],
         "project_roles": {"project-a": ["roles/storage.admin"], "project-b": ["roles/storage.admin"]}},
        {"id": "viewer@example.invalid", "tokens": ["viewer-token"],
         "project_roles": {"project-a": ["roles/storage.objectViewer"]}},
        {"id": "creator@example.invalid", "tokens": ["creator-token"],
         "project_roles": {"project-a": ["roles/storage.objectCreator"]}},
        {"id": "editor@example.invalid", "tokens": ["editor-token"],
         "project_roles": {"project-a": ["roles/storage.editor"]}},
        {"id": "a-only@example.invalid", "tokens": ["a-only-token"],
         "project_roles": {"project-a": ["roles/storage.admin"]}},
        {"id": "no-roles@example.invalid", "tokens": ["no-roles-token"]}
    ]})
}

impl Server {
    fn start(dir: &TestDir, auth: &[&str]) -> (Self, Value) {
        Self::start_with_stderr(dir, auth, Stdio::inherit())
    }

    fn start_with_stderr(dir: &TestDir, auth: &[&str], stderr: Stdio) -> (Self, Value) {
        // Restart comparisons retain one advertised origin while ephemeral listeners change.
        let api_options: &[&str] = if auth.iter().any(|arg| arg.starts_with("--api-base-url")) {
            &[]
        } else {
            &["--api-base-url", "http://127.0.0.1:4443"]
        };
        // The connection deadline also bounds a whole non-object request and the gaps
        // between response chunks. A short one answered ordinary requests with 503 or
        // cut a download short under loaded runs, so only deadline tests set their own.
        let timeout_options: &[&str] = if auth.contains(&"--connection-timeout-ms") {
            &[]
        } else {
            &["--connection-timeout-ms", "20000"]
        };
        let server = Self::launch(
            http_command(&dir.0)
                .args(timeout_options)
                .args(api_options)
                .args(auth)
                .stderr(stderr),
        );
        let ready = server.ready.clone();
        (server, ready)
    }

    fn strict(dir: &TestDir, identities: &Path) -> Self {
        Self::strict_with(dir, identities, &[])
    }

    fn strict_with(dir: &TestDir, identities: &Path, options: &[&str]) -> Self {
        let mut args = vec![
            "--auth-mode",
            "strict",
            "--identity-file",
            identities.to_str().unwrap(),
        ];
        args.extend_from_slice(options);
        let (server, ready) = Self::start(dir, &args);
        assert_eq!(ready["auth_mode"], "strict");
        assert_eq!(ready["identities"], 6);
        server
    }

    fn stream(&self) -> TcpStream {
        let stream = TcpStream::connect(&self.address).unwrap();
        // A hang guard, not a latency bound; it outlasts the server's connection deadline
        // so a stalled request reports the server's answer rather than a client timeout.
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream
    }

    fn raw(&self, request: &[u8]) -> Reply {
        let mut stream = self.stream();
        stream.write_all(request).unwrap();
        common::http::receive(stream, false).into_joined()
    }

    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        let mut request = request_head(method, path, "local", headers, body.len()).into_bytes();
        request.extend_from_slice(body);
        let mut stream = self.stream();
        stream.write_all(&close_after_response(&request)).unwrap();
        common::http::receive(stream, method == "HEAD").into_joined()
    }

    fn create_bucket(&self, auth: &str, project: &str, name: &str) -> Reply {
        self.request(
            "POST",
            &format!("/storage/v1/b?project={project}"),
            &format!("{auth}Content-Type: application/json\r\n"),
            format!(r#"{{"name":"{name}"}}"#).as_bytes(),
        )
    }

    fn upload(&self, auth: &str, bucket: &str, name: &str, body: &[u8]) -> Reply {
        self.request(
            "POST",
            &format!("/upload/storage/v1/b/{bucket}/o?uploadType=media&name={name}"),
            auth,
            body,
        )
    }

    fn multipart(&self, auth: &str, bucket: &str, name: &str, media: &[u8]) -> Reply {
        let mut body =
            b"--boundary\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n".to_vec();
        body.extend_from_slice(format!(r#"{{"name":"{name}"}}"#).as_bytes());
        body.extend_from_slice(b"\r\n--boundary\r\nContent-Type: text/plain\r\n\r\n");
        body.extend_from_slice(media);
        body.extend_from_slice(b"\r\n--boundary--\r\n");
        self.request(
            "POST",
            &format!("/upload/storage/v1/b/{bucket}/o?uploadType=multipart"),
            &format!("{auth}Content-Type: multipart/related; boundary=boundary\r\n"),
            &body,
        )
    }

    fn get(&self, auth: &str, path: &str) -> Reply {
        self.request("GET", path, auth, b"")
    }

    fn stop(&mut self) {
        self.0.stop();
    }
}

/// Adds `Connection: close` after the request line, so the server closes the
/// connection first (see [`common::await_server_close`]) and a HEAD reply can be
/// checked for stray body bytes.
fn close_after_response(request: &[u8]) -> Vec<u8> {
    let line = request
        .windows(2)
        .position(|bytes| bytes == b"\r\n")
        .unwrap()
        + 2;
    [&request[..line], b"Connection: close\r\n", &request[line..]].concat()
}

use common::http::Reply;

fn identity_file(config: &TestDir) -> PathBuf {
    let path = config.0.join("identities.json");
    std::fs::write(&path, registry().to_string()).unwrap();
    path
}

/// Stops a server started with piped stderr and returns its JSON events.
fn stderr_events(server: &mut Server) -> Vec<Value> {
    server.stop();
    let mut stderr = String::new();
    server
        .child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    stderr
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn strict_jwt_bearer_fails_unchanged_and_warns_once_about_self_signed_tokens() {
    let config = TestDir::new();
    let identities = identity_file(&config);
    let data = TestDir::new();
    let (mut server, ready) = Server::start_with_stderr(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            identities.to_str().unwrap(),
        ],
        Stdio::piped(),
    );
    assert_eq!(ready["auth_mode"], "strict");
    // The shape googleauth sends outside googleapis.com: base64url header.payload.signature.
    let jwt = "Authorization: Bearer eyJhbGciOiJSUzI1NiJ9.eyJpc3MiOiJsb2NhbCJ9.c2lnbmF0dXJl\r\n";
    for _ in 0..2 {
        server
            .get(jwt, "/storage/v1/b?project=project-a")
            .invalid_credentials();
    }
    let warnings: Vec<_> = stderr_events(&mut server)
        .into_iter()
        .filter(|e| e["event"] == "auth_self_signed_jwt_rejected")
        .collect();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0]["level"], "warn");
    let message = warnings[0]["message"].as_str().unwrap();
    assert!(
        message.contains("identity file") && message.contains("permissive"),
        "{message}"
    );
    assert!(
        !message.contains("eyJ"),
        "the token must not be logged: {message}"
    );

    // An unknown opaque token keeps the same failure and gets no JWT advice.
    let data = TestDir::new();
    let (mut server, _) = Server::start_with_stderr(
        &data,
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            identities.to_str().unwrap(),
        ],
        Stdio::piped(),
    );
    server
        .get(
            "Authorization: Bearer unknown-token\r\n",
            "/storage/v1/b?project=project-a",
        )
        .invalid_credentials();
    assert!(
        !stderr_events(&mut server)
            .iter()
            .any(|e| e["event"] == "auth_self_signed_jwt_rejected")
    );
}

#[test]
fn auth_http_permissive_ignores_any_authorization() {
    let dir = TestDir::new();
    let (server, ready) = Server::start(&dir, &[]);
    assert_eq!(ready["auth_mode"], "permissive");
    assert!(ready.get("identities").is_none());
    for auth in [
        "",
        "Authorization: Bearer unknown\r\n",
        "Authorization: Basic Z2FyYmFnZQ==\r\n",
        "Authorization: GOOG4-RSA-SHA256 Credential=x\r\n",
        "Authorization: garbage\r\nAuthorization: more garbage\r\n",
    ] {
        let name = format!("bucket-{}", auth.len());
        server.create_bucket(auth, "local", &name).success();
        server.get(auth, &format!("/storage/v1/b/{name}")).success();
        server.upload(auth, &name, "o", b"data").success();
        assert_eq!(
            server
                .get(auth, &format!("/storage/v1/b/{name}/o/o?alt=media"))
                .body,
            b"data"
        );
    }
}

#[test]
fn auth_http_strict_rejects_anonymous_and_unsupported_credentials() {
    let config = TestDir::new();
    let dir = TestDir::new();
    let server = Server::strict(&dir, &identity_file(&config));
    server
        .create_bucket(ADMIN, "project-a", "bucket-a")
        .success();
    server.upload(ADMIN, "bucket-a", "o", b"data").success();
    // Anonymous callers need a login on every supported route, even for a missing bucket.
    server
        .create_bucket("", "project-a", "anon")
        .login_required();
    server
        .get("", "/storage/v1/b?project=project-a")
        .login_required();
    server.get("", "/storage/v1/b/bucket-a").login_required();
    server
        .get("", "/storage/v1/b/missing-bucket")
        .login_required();
    server
        .get("", "/storage/v1/b/bucket-a/o/o")
        .login_required();
    server
        .get("", "/storage/v1/b/bucket-a/o/o?alt=media")
        .login_required();
    server
        .get("", "/download/storage/v1/b/bucket-a/o/o?alt=media")
        .login_required();
    server.get("", "/storage/v1/b/bucket-a/o").login_required();
    server
        .get("", "/storage/v1/b/missing-bucket/o/o")
        .login_required();
    server.upload("", "bucket-a", "o", b"new").login_required();
    server
        .multipart("", "bucket-a", "o", b"new")
        .login_required();
    server
        .request("PATCH", "/storage/v1/b/bucket-a/o/o", "", b"{}")
        .login_required();
    server
        .request("PUT", "/storage/v1/b/bucket-a/o/o", "", b"{}")
        .login_required();
    server
        .request("DELETE", "/storage/v1/b/bucket-a/o/o", "", b"")
        .login_required();
    server
        .request("DELETE", "/storage/v1/b/bucket-a", "", b"")
        .login_required();
    // Unknown, empty, malformed, duplicate or unsupported credentials never fall back.
    for auth in [
        "Authorization: Bearer unknown-token\r\n",
        "Authorization: Bearer \r\n",
        "Authorization: Bearer\r\n",
        "Authorization: Bearer admin-token extra\r\n",
        "Authorization: Basic YWRtaW4tdG9rZW46\r\n",
        "Authorization: GOOG4-RSA-SHA256 Credential=x, Signature=y\r\n",
        "Authorization: GOOG4-HMAC-SHA256 Credential=x\r\n",
        "Authorization: AWS4-HMAC-SHA256 Credential=x\r\n",
        "Authorization: admin-token\r\n",
        "Authorization: Bearer admin-token\r\nAuthorization: Bearer admin-token\r\n",
    ] {
        server
            .get(auth, "/storage/v1/b/bucket-a/o/o")
            .invalid_credentials();
        server
            .get(auth, "/storage/v1/b/missing-bucket")
            .invalid_credentials();
        server
            .upload(auth, "bucket-a", "o", b"new")
            .invalid_credentials();
    }
    for query in [
        "access_token=admin-token",
        "X-Goog-Signature=00",
        "X-Goog-Credential=x",
        "X-Goog-Algorithm=GOOG4-RSA-SHA256",
        "Signature=x",
        "GoogleAccessId=x",
    ] {
        for auth in ["", ADMIN] {
            server
                .get(auth, &format!("/storage/v1/b/bucket-a/o/o?{query}"))
                .invalid_credentials();
            server
                .get(auth, &format!("/storage/v1/b?project=project-a&{query}"))
                .invalid_credentials();
        }
    }
    // Non-UTF-8 header bytes are invalid credentials, not anonymous.
    server
        .raw(b"GET /storage/v1/b/bucket-a HTTP/1.1\r\nHost: local\r\nAuthorization: Bearer \xff\r\n\r\n")
        .invalid_credentials();
    // Bearer scheme is case-insensitive; every registered token resolves the identity.
    for auth in [
        "Authorization: bearer admin-token\r\n",
        "Authorization: BEARER   admin-token-2\r\n",
    ] {
        assert_eq!(
            server
                .get(auth, "/storage/v1/b/bucket-a/o/o?alt=media")
                .body,
            b"data"
        );
    }
    // Authentication precedes route validation; anonymous route syntax errors stay 400.
    server
        .get(
            "Authorization: Bearer unknown\r\n",
            "/storage/v1/b/INVALID/o/o",
        )
        .invalid_credentials();
    server
        .get("", "/storage/v1/b/INVALID/o/o")
        .auth_error(400, "invalidParameter");
    // Known unsupported routes validate credentials; unknown routes and probes retain their handling.
    server
        .request(
            "POST",
            "/storage/v1/b/bucket-a/o",
            "Authorization: Bearer unknown\r\n",
            b"{}",
        )
        .invalid_credentials();
    for auth in ["", "Authorization: Bearer unknown\r\n", ADMIN] {
        if auth != "Authorization: Bearer unknown\r\n" {
            server
                .request("POST", "/storage/v1/b/bucket-a/o", auth, b"{}")
                .auth_error(400, "badRequest");
        }
        assert_eq!(server.get(auth, "/storage/v1/unknown").status, 404);
        assert_eq!(server.get(auth, "/INVALID").status, 404);
        assert_eq!(server.get(auth, "/_internal/ready").status, 200);
        assert_eq!(server.get(auth, "/_internal/health").status, 200);
    }
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-a/o/o?alt=media")
            .body,
        b"data"
    );
}

#[test]
fn auth_http_admin_full_crud_and_restart_with_same_registry() {
    let config = TestDir::new();
    let identities = identity_file(&config);
    let dir = TestDir::new();
    let mut server = Server::strict(&dir, &identities);
    server
        .create_bucket(ADMIN, "project-a", "bucket-a")
        .success();
    assert_eq!(
        server.get(ADMIN, "/storage/v1/b/bucket-a").success().json()["name"],
        "bucket-a"
    );
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b?project=project-a")
            .success()
            .json()["items"][0]["name"],
        "bucket-a"
    );
    server.upload(ADMIN, "bucket-a", "media", b"one").success();
    server.upload(ADMIN, "bucket-a", "media", b"two").success();
    server
        .multipart(ADMIN, "bucket-a", "multi", b"three")
        .success();
    server
        .multipart(ADMIN, "bucket-a", "multi", b"four")
        .success();
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-a/o/media")
            .success()
            .json()["size"],
        "3"
    );
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-a/o/multi?alt=media")
            .body,
        b"four"
    );
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-a/o")
            .success()
            .json()["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let patch = server
        .request(
            "PATCH",
            "/storage/v1/b/bucket-a/o/media",
            &format!("{ADMIN}Content-Type: application/json\r\n"),
            br#"{"metadata":{"k":"v"}}"#,
        )
        .success()
        .json();
    assert_eq!(patch["metadata"]["k"], "v");
    server
        .request(
            "PUT",
            "/storage/v1/b/bucket-a/o/media",
            &format!("{ADMIN}Content-Type: application/json\r\n"),
            br#"{"contentType":"text/plain"}"#,
        )
        .success();
    server.stop();

    let mut server = Server::strict(&dir, &identities);
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-a/o/media?alt=media")
            .body,
        b"two"
    );
    server.get("", "/storage/v1/b/bucket-a").login_required();
    for name in ["media", "multi"] {
        server
            .request(
                "DELETE",
                &format!("/storage/v1/b/bucket-a/o/{name}"),
                ADMIN,
                b"",
            )
            .success();
    }
    server
        .request("DELETE", "/storage/v1/b/bucket-a", ADMIN, b"")
        .success();
    server
        .get(ADMIN, "/storage/v1/b/bucket-a")
        .auth_error(404, "notFound");
    server.stop();
}

#[test]
fn null_generation_metadata_updates_keep_strict_authorization() {
    let config = TestDir::new();
    let dir = TestDir::new();
    let mut server = Server::strict(&dir, &identity_file(&config));
    server
        .create_bucket(ADMIN, "project-a", "bucket-a")
        .success();
    server.upload(ADMIN, "bucket-a", "o", b"original").success();
    let path = "/storage/v1/b/bucket-a/o/o";
    let body = br#"{"generation":null,"metadata":{"fixture":"updated"}}"#;
    for (method, headers) in [
        ("PATCH", ""),
        ("PUT", ""),
        ("POST", "X-HTTP-Method-Override: PATCH\r\n"),
    ] {
        let before = server.get(ADMIN, path).success().json();
        for (auth, status, reason) in [("", 401, "required"), (VIEWER, 403, "forbidden")] {
            server
                .request(
                    method,
                    path,
                    &format!("{auth}{headers}Content-Type: application/json\r\n"),
                    body,
                )
                .auth_error(status, reason);
            assert_eq!(server.get(ADMIN, path).success().json(), before);
        }
        let updated = server
            .request(
                method,
                path,
                &format!("{ADMIN}{headers}Content-Type: application/json\r\n"),
                body,
            )
            .success()
            .json();
        assert_eq!(updated["metadata"], json!({"fixture":"updated"}));
        for field in ["generation", "size", "md5Hash", "crc32c"] {
            assert_eq!(updated[field], before[field]);
        }
        assert_eq!(
            server
                .get(ADMIN, &format!("{path}?alt=media"))
                .success()
                .body,
            b"original"
        );
    }
    server.stop();
}

#[test]
fn auth_http_project_roles_gate_each_route() {
    let config = TestDir::new();
    let dir = TestDir::new();
    let server = Server::strict(&dir, &identity_file(&config));
    server
        .create_bucket(ADMIN, "project-a", "bucket-a")
        .success();
    server
        .create_bucket(ADMIN, "project-b", "bucket-b")
        .success();
    server.upload(ADMIN, "bucket-a", "o", b"old").success();
    server.upload(ADMIN, "bucket-b", "o", b"b").success();
    let original = server
        .get(ADMIN, "/storage/v1/b/bucket-a/o/o")
        .success()
        .json();

    // objectViewer reads objects but cannot see bucket metadata or mutate objects.
    assert_eq!(
        server
            .get(VIEWER, "/storage/v1/b/bucket-a/o/o")
            .success()
            .json(),
        original
    );
    assert_eq!(
        server
            .get(VIEWER, "/storage/v1/b/bucket-a/o/o?alt=media")
            .body,
        b"old"
    );
    server.get(VIEWER, "/storage/v1/b/bucket-a/o").success();
    server.get(VIEWER, "/storage/v1/b/bucket-a").forbidden();
    server
        .get(VIEWER, "/storage/v1/b?project=project-a")
        .forbidden();
    server.upload(VIEWER, "bucket-a", "new", b"x").forbidden();
    server
        .multipart(VIEWER, "bucket-a", "new", b"x")
        .forbidden();
    server
        .request("PATCH", "/storage/v1/b/bucket-a/o/o", VIEWER, b"{}")
        .forbidden();
    server
        .request("PUT", "/storage/v1/b/bucket-a/o/o", VIEWER, b"{}")
        .forbidden();
    server
        .request("DELETE", "/storage/v1/b/bucket-a/o/o", VIEWER, b"")
        .forbidden();
    server
        .request("DELETE", "/storage/v1/b/bucket-a", VIEWER, b"")
        .forbidden();
    server
        .create_bucket(VIEWER, "project-a", "viewer-bucket")
        .forbidden();

    // objectCreator creates new objects, but overwriting needs storage.objects.delete.
    let created = server
        .upload(CREATOR, "bucket-a", "created", b"new")
        .success()
        .json();
    server
        .multipart(CREATOR, "bucket-a", "created-multi", b"new")
        .success();
    server
        .upload(CREATOR, "bucket-a", "o", b"overwrite")
        .forbidden();
    server
        .multipart(CREATOR, "bucket-a", "o", b"overwrite")
        .forbidden();
    server
        .upload(CREATOR, "bucket-a", "created", b"again")
        .forbidden();
    // A failing precondition reports 412: nothing would have been replaced.
    server
        .request(
            "POST",
            "/upload/storage/v1/b/bucket-a/o?uploadType=media&name=o&ifGenerationMatch=0",
            CREATOR,
            b"x",
        )
        .auth_error(412, "conditionNotMet");
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-a/o/o")
            .success()
            .json(),
        original
    );
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-a/o/o?alt=media")
            .body,
        b"old"
    );
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-a/o/created?alt=media")
            .body,
        b"new"
    );
    assert_eq!(
        server
            .get(CREATOR, "/storage/v1/b/bucket-a/o/created")
            .success()
            .json(),
        created
    );
    assert_eq!(
        server
            .get(CREATOR, "/storage/v1/b/bucket-a/o/created?alt=media")
            .body,
        b"new"
    );
    server
        .get(CREATOR, "/storage/v1/b/bucket-a/o/o")
        .forbidden();
    server
        .get(NO_ROLES, "/storage/v1/b/bucket-a/o/created")
        .forbidden();
    server
        .request("DELETE", "/storage/v1/b/bucket-a/o/created", CREATOR, b"")
        .forbidden();
    server.get(CREATOR, "/storage/v1/b/bucket-a/o").forbidden();

    // storage.editor creates and lists buckets but lacks storage.buckets.get.
    server
        .create_bucket(EDITOR, "project-a", "editor-bucket")
        .success();
    server
        .get(EDITOR, "/storage/v1/b?project=project-a")
        .success();
    server
        .get(EDITOR, "/storage/v1/b/editor-bucket")
        .forbidden();
    server
        .create_bucket(EDITOR, "project-b", "editor-b")
        .forbidden();
    server.get(EDITOR, "/storage/v1/b/bucket-a/o/o").forbidden();

    // Roles in project A never apply to a bucket owned by project B.
    server
        .get(PROJECT_A_ONLY, "/storage/v1/b/bucket-a")
        .success();
    for path in [
        "/storage/v1/b/bucket-b",
        "/storage/v1/b/bucket-b/o",
        "/storage/v1/b/bucket-b/o/o",
        "/storage/v1/b/bucket-b/o/o?alt=media",
        "/storage/v1/b?project=project-b",
    ] {
        server.get(PROJECT_A_ONLY, path).forbidden();
    }
    server
        .upload(PROJECT_A_ONLY, "bucket-b", "o", b"x")
        .forbidden();
    server
        .request("DELETE", "/storage/v1/b/bucket-b/o/o", PROJECT_A_ONLY, b"")
        .forbidden();
    server
        .request("DELETE", "/storage/v1/b/bucket-b", PROJECT_A_ONLY, b"")
        .forbidden();
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-b/o/o?alt=media")
            .body,
        b"b"
    );

    // A missing bucket is 404 only for callers holding the permission in some project.
    server
        .get(ADMIN, "/storage/v1/b/missing-bucket")
        .auth_error(404, "notFound");
    server
        .get(VIEWER, "/storage/v1/b/missing-bucket/o/o")
        .auth_error(404, "notFound");
    server
        .get(VIEWER, "/storage/v1/b/missing-bucket/o")
        .auth_error(404, "notFound");
    server
        .upload(CREATOR, "missing-bucket", "o", b"x")
        .auth_error(404, "notFound");
    server
        .get(VIEWER, "/storage/v1/b/missing-bucket")
        .forbidden();
    server
        .get(NO_ROLES, "/storage/v1/b/missing-bucket")
        .forbidden();
    server
        .get(NO_ROLES, "/storage/v1/b/missing-bucket/o/o")
        .forbidden();
    server
        .upload(NO_ROLES, "missing-bucket", "o", b"x")
        .forbidden();
    server
        .get(NO_ROLES, "/storage/v1/b/bucket-a/o/o")
        .forbidden();
}

#[test]
fn auth_http_rejected_uploads_do_not_read_or_stage_the_body() {
    let config = TestDir::new();
    let dir = TestDir::new();
    let server = Server::strict(&dir, &identity_file(&config));
    server
        .create_bucket(ADMIN, "project-a", "bucket-a")
        .success();
    server.upload(ADMIN, "bucket-a", "exists", b"old").success();
    for (auth, query, status, reason) in [
        ("", "uploadType=media&name=new", 401, "required"),
        (
            "Authorization: Bearer unknown\r\n",
            "uploadType=media&name=new",
            401,
            "authError",
        ),
        (VIEWER, "uploadType=media&name=new", 403, "forbidden"),
        (VIEWER, "uploadType=multipart", 403, "forbidden"),
        (CREATOR, "uploadType=media&name=exists", 403, "forbidden"),
        (NO_ROLES, "uploadType=media&name=new", 403, "forbidden"),
    ] {
        let start = Instant::now();
        let mut stream = server.stream();
        stream
            .write_all(
                format!(
                    "POST /upload/storage/v1/b/bucket-a/o?{query} HTTP/1.1\r\nHost: local\r\n{auth}Content-Type: multipart/related; boundary=b\r\nContent-Length: 8388608\r\n\r\n"
                )
                .as_bytes(),
            )
            .unwrap();
        let mut writer = stream.try_clone().unwrap();
        let sending = thread::spawn(move || {
            // Rejection may interrupt the write; the response must still reach the reader.
            let _ = writer.write_all(&vec![b'x'; 8 * 1024 * 1024]);
        });
        common::http::receive(stream, false)
            .into_joined()
            .auth_error(status, reason);
        sending.join().unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "rejection must not wait for the advertised body"
        );
    }
    // A request that never sends its advertised body is still rejected promptly.
    let start = Instant::now();
    server
        .raw(b"POST /upload/storage/v1/b/bucket-a/o?uploadType=media&name=exists HTTP/1.1\r\nHost: local\r\nAuthorization: Bearer creator-token\r\nContent-Length: 900000\r\n\r\n")
        .forbidden();
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-a/o/exists?alt=media")
            .body,
        b"old"
    );
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-a/o")
            .success()
            .json()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(common::server::staging_count(&dir), 0);
}

#[test]
fn auth_http_upload_into_a_bucket_recreated_in_another_project_is_forbidden() {
    let config = TestDir::new();
    let dir = TestDir::new();
    let server = Server::strict(&dir, &identity_file(&config));
    server.create_bucket(ADMIN, "project-a", "race").success();
    let mut stream = server.stream();
    stream
        .write_all(
            b"POST /upload/storage/v1/b/race/o?uploadType=media&name=object HTTP/1.1\r\nHost: local\r\nAuthorization: Bearer a-only-token\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n",
        )
        .unwrap();
    // The server polls the body only after authorization and the advisory upload check, so
    // the interim response deterministically orders the recreation after both.
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut interim = Vec::new();
    while !interim.ends_with(b"\r\n\r\n") {
        assert_ne!(reader.read_until(b'\n', &mut interim).unwrap(), 0);
    }
    assert!(interim.starts_with(b"HTTP/1.1 100"), "{interim:?}");
    server
        .request("DELETE", "/storage/v1/b/race", ADMIN, b"")
        .success();
    server.create_bucket(ADMIN, "project-b", "race").success();
    stream.write_all(b"late").unwrap();
    // The caller holds no role in project-b, so publication must not reach the new bucket.
    common::http::receive(stream, false)
        .into_joined()
        .forbidden();
    let listing = server.get(ADMIN, "/storage/v1/b/race/o").success().json();
    assert!(listing.get("items").is_none(), "{listing}");
    // The same caller still learns nothing about the recreated bucket's objects.
    server
        .get(PROJECT_A_ONLY, "/storage/v1/b/race/o/object?alt=media")
        .forbidden();
    server
        .get(PROJECT_A_ONLY, "/storage/v1/b/race/o")
        .forbidden();
    assert_eq!(common::server::staging_count(&dir), 0);
}

#[test]
fn auth_http_concurrent_creates_without_delete_have_one_winner() {
    let config = TestDir::new();
    let dir = TestDir::new();
    let server = Server::strict(&dir, &identity_file(&config));
    server
        .create_bucket(ADMIN, "project-a", "bucket-a")
        .success();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let results = thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|index| {
                let barrier = barrier.clone();
                let server = &server;
                scope.spawn(move || {
                    barrier.wait();
                    let body = format!("writer-{index}");
                    let reply = server.upload(CREATOR, "bucket-a", "race", body.as_bytes());
                    (reply.status, body)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    let winners: Vec<_> = results
        .iter()
        .filter(|(status, _)| *status == 200)
        .collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    assert_eq!(
        results.iter().filter(|(status, _)| *status == 403).count(),
        7,
        "{results:?}"
    );
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-a/o/race?alt=media")
            .body,
        winners[0].1.as_bytes()
    );
    assert_eq!(common::server::staging_count(&dir), 0);
}

#[test]
fn auth_http_startup_rejects_invalid_profiles_and_registries() {
    let config = TestDir::new();
    let valid = identity_file(&config);
    let oversize = config.0.join("oversize.json");
    let mut document = registry().to_string();
    document.push_str(&" ".repeat(64 * 1024 + 1 - document.len()));
    std::fs::write(&oversize, document).unwrap();
    let mut cases: Vec<(&str, Vec<String>, &str)> = vec![
        (
            "strict without registry",
            vec!["--auth-mode".into(), "strict".into()],
            "auth_mode=strict requires identity_file",
        ),
        (
            "registry in permissive",
            vec!["--identity-file".into(), valid.to_str().unwrap().into()],
            "identity_file requires auth_mode=strict",
        ),
        (
            "unknown mode",
            vec!["--auth-mode".into(), "open".into()],
            "auth_mode must be permissive or strict",
        ),
        (
            "oversize",
            vec![
                "--auth-mode".into(),
                "strict".into(),
                "--identity-file".into(),
                oversize.to_str().unwrap().into(),
            ],
            "identity_file exceeds 64 KiB",
        ),
        (
            "missing file",
            vec![
                "--auth-mode".into(),
                "strict".into(),
                "--identity-file".into(),
                config.0.join("absent.json").to_str().unwrap().into(),
            ],
            "identity_file: ",
        ),
    ];
    let identity = |extra: &str| format!(r#"{{"id":"a","tokens":["t"]{extra}}}"#);
    let documents = [
        (
            "unknown role",
            format!(
                r#"{{"identities":[{}]}}"#,
                identity(r#","project_roles":{"p":["roles/storage.unknown"]}"#)
            ),
            "unsupported role roles/storage.unknown",
        ),
        (
            "basic role",
            format!(
                r#"{{"identities":[{}]}}"#,
                identity(r#","project_roles":{"p":["roles/owner"]}"#)
            ),
            "unsupported role roles/owner",
        ),
        (
            "duplicate token",
            format!(
                r#"{{"identities":[{},{{"id":"b","tokens":["t"]}}]}}"#,
                identity("")
            ),
            "duplicate token",
        ),
        (
            "duplicate id",
            format!(
                r#"{{"identities":[{},{{"id":"a","tokens":["u"]}}]}}"#,
                identity("")
            ),
            "duplicate id: a",
        ),
        (
            "unknown field",
            format!(r#"{{"identities":[{}]}}"#, identity(r#","roles":[]"#)),
            "unknown field: roles",
        ),
        (
            "unknown top-level field",
            format!(r#"{{"identities":[{}],"version":1}}"#, identity("")),
            "unknown field: version",
        ),
        (
            "invalid project",
            format!(
                r#"{{"identities":[{}]}}"#,
                identity(r#","project_roles":{"bad project":["roles/storage.admin"]}"#)
            ),
            "project \"bad project\"",
        ),
        ("malformed", "{\"identities\":[".into(), "identity_file: "),
    ];
    let paths: Vec<_> = documents
        .iter()
        .enumerate()
        .map(|(index, (_, document, _))| {
            let path = config.0.join(format!("invalid-{index}.json"));
            std::fs::write(&path, document).unwrap();
            path
        })
        .collect();
    for ((case, _, message), path) in documents.iter().zip(&paths) {
        cases.push((
            case,
            vec![
                "--auth-mode".into(),
                "strict".into(),
                "--identity-file".into(),
                path.to_str().unwrap().into(),
            ],
            message,
        ));
    }
    for (case, args, message) in cases {
        let dir = TestDir::new();
        let output = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(&dir.0)
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{case}");
        assert!(
            output.stdout.is_empty(),
            "{case}: no readiness before failure"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        let events: Vec<serde_json::Value> = stderr
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let event = events
            .iter()
            .find(|event| event["level"] == "error")
            .unwrap();
        assert_eq!(event["event"], "diagnostics", "{case}");
        assert_eq!(event["level"], "error", "{case}");
        if matches!(
            case,
            "strict without registry" | "registry in permissive" | "unknown mode"
        ) {
            assert!(stderr.contains(message), "{case}: {stderr}");
        } else {
            assert_eq!(event["diagnostics"][0]["source"], "identity", "{case}");
            assert_eq!(event["diagnostics"][0]["key"], "identity_file", "{case}");
            assert!(
                event["diagnostics"][0]["fix"]
                    .as_str()
                    .unwrap()
                    .contains("readable valid registry")
            );
        }
        // The registry is checked before the catalog is created or the listener is bound.
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0, "{case}");
    }
    // Environment variables select the same profile.
    let dir = TestDir::new();
    let mut child = common::server_command()
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
        ])
        .arg(&dir.0)
        .env("GCS_MIMIC_AUTH_MODE", "strict")
        .env("GCS_MIMIC_IDENTITY_FILE", &valid)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    let ready: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["auth_mode"], "strict");
    assert_eq!(ready["identities"], 6);
    common::stop_server(&mut child, &mut stdout);
}

declare_server!();
