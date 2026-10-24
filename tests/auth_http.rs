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
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Stdio},
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

struct Server {
    child: Child,
    address: String,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(dir: &TestDir, auth: &[&str]) -> (Self, Value) {
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
            .args(["--connection-timeout-ms", "2000"])
            .args(auth)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).expect("ready event");
        assert_eq!(ready["event"], "ready");
        (
            Self {
                child,
                address: ready["listen"].as_str().unwrap().into(),
                stdout,
            },
            ready,
        )
    }

    fn strict(dir: &TestDir, identities: &Path) -> Self {
        let (server, ready) = Self::start(
            dir,
            &[
                "--auth-mode",
                "strict",
                "--identity-file",
                identities.to_str().unwrap(),
            ],
        );
        assert_eq!(ready["auth_mode"], "strict");
        assert_eq!(ready["identities"], 6);
        server
    }

    fn stream(&self) -> TcpStream {
        let stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
    }

    fn raw(&self, request: &[u8]) -> Reply {
        let mut stream = self.stream();
        stream.write_all(request).unwrap();
        Reply::read(stream)
    }

    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: local\r\nContent-Length: {}\r\n{headers}\r\n",
            body.len()
        )
        .into_bytes();
        request.extend_from_slice(body);
        if method == "HEAD" {
            let mut stream = self.stream();
            stream.write_all(&request).unwrap();
            Reply::read_body(stream, true)
        } else {
            self.raw(&request)
        }
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
        common::stop_server(&mut self.child, &mut self.stdout);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
    }
}

struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Reply {
    fn read(stream: TcpStream) -> Self {
        Self::read_body(stream, false)
    }

    fn read_body(stream: TcpStream, head: bool) -> Self {
        let mut reader = BufReader::new(stream);
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            assert_ne!(reader.read_until(b'\n', &mut bytes).unwrap(), 0);
        }
        let text = std::str::from_utf8(&bytes[..bytes.len() - 4]).unwrap();
        let mut lines = text.split("\r\n");
        let status = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers: BTreeMap<String, String> = lines
            .map(|line| {
                let (key, value) = line.split_once(':').unwrap();
                (key.to_ascii_lowercase(), value.trim().into())
            })
            .collect();
        let length = if head || matches!(status, 204 | 304) {
            0
        } else {
            headers["content-length"].parse().unwrap()
        };
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        if head {
            let mut extra = Vec::new();
            reader.read_to_end(&mut extra).unwrap();
            assert!(extra.is_empty(), "HEAD emitted a response body");
        }
        Self {
            status,
            headers,
            body,
        }
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    fn ok(&self) -> &Self {
        assert!(
            matches!(self.status, 200 | 204),
            "{}: {}",
            self.status,
            String::from_utf8_lossy(&self.body)
        );
        self
    }

    fn error(&self, status: u16, reason: &str) {
        assert_eq!(
            self.status,
            status,
            "{}",
            String::from_utf8_lossy(&self.body)
        );
        assert_eq!(
            self.headers["content-type"],
            "application/json; charset=UTF-8"
        );
        assert_eq!(self.json()["error"]["errors"][0]["reason"], reason);
        assert!(!self.headers.contains_key("www-authenticate"));
    }

    fn login_required(&self) {
        self.error(401, "required");
        assert_eq!(
            self.json(),
            json!({"error":{"errors":[{"domain":"global","reason":"required","message":"Login Required","locationType":"header","location":"Authorization"}],"code":401,"message":"Login Required"}})
        );
    }

    fn invalid_credentials(&self) {
        self.error(401, "authError");
        assert_eq!(
            self.json(),
            json!({"error":{"errors":[{"domain":"global","reason":"authError","message":"Invalid Credentials","locationType":"header","location":"Authorization"}],"code":401,"message":"Invalid Credentials"}})
        );
    }

    fn forbidden(&self) {
        self.error(403, "forbidden");
        assert_eq!(
            self.json(),
            json!({"error":{"errors":[{"domain":"global","reason":"forbidden","message":"Forbidden"}],"code":403,"message":"Forbidden"}})
        );
    }
}

fn identity_file(config: &TestDir) -> PathBuf {
    let path = config.0.join("identities.json");
    std::fs::write(&path, registry().to_string()).unwrap();
    path
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
        server.create_bucket(auth, "local", &name).ok();
        server.get(auth, &format!("/storage/v1/b/{name}")).ok();
        server.upload(auth, &name, "o", b"data").ok();
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
    server.create_bucket(ADMIN, "project-a", "bucket-a").ok();
    server.upload(ADMIN, "bucket-a", "o", b"data").ok();
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
        .error(400, "invalidParameter");
    // Unknown routes keep 404 and probes stay unauthenticated, whatever the credentials.
    for auth in ["", "Authorization: Bearer unknown\r\n", ADMIN] {
        let reply = server.request("POST", "/storage/v1/b/bucket-a/o", auth, b"{}");
        assert_eq!(reply.status, 404);
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
    server.create_bucket(ADMIN, "project-a", "bucket-a").ok();
    assert_eq!(
        server.get(ADMIN, "/storage/v1/b/bucket-a").ok().json()["name"],
        "bucket-a"
    );
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b?project=project-a")
            .ok()
            .json()["items"][0]["name"],
        "bucket-a"
    );
    server.upload(ADMIN, "bucket-a", "media", b"one").ok();
    server.upload(ADMIN, "bucket-a", "media", b"two").ok();
    server.multipart(ADMIN, "bucket-a", "multi", b"three").ok();
    server.multipart(ADMIN, "bucket-a", "multi", b"four").ok();
    assert_eq!(
        server
            .get(ADMIN, "/storage/v1/b/bucket-a/o/media")
            .ok()
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
        server.get(ADMIN, "/storage/v1/b/bucket-a/o").ok().json()["items"]
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
        .ok()
        .json();
    assert_eq!(patch["metadata"]["k"], "v");
    server
        .request(
            "PUT",
            "/storage/v1/b/bucket-a/o/media",
            &format!("{ADMIN}Content-Type: application/json\r\n"),
            br#"{"contentType":"text/plain"}"#,
        )
        .ok();
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
            .ok();
    }
    server
        .request("DELETE", "/storage/v1/b/bucket-a", ADMIN, b"")
        .ok();
    server
        .get(ADMIN, "/storage/v1/b/bucket-a")
        .error(404, "notFound");
    server.stop();
}

#[test]
fn auth_http_project_roles_gate_each_route() {
    let config = TestDir::new();
    let dir = TestDir::new();
    let server = Server::strict(&dir, &identity_file(&config));
    server.create_bucket(ADMIN, "project-a", "bucket-a").ok();
    server.create_bucket(ADMIN, "project-b", "bucket-b").ok();
    server.upload(ADMIN, "bucket-a", "o", b"old").ok();
    server.upload(ADMIN, "bucket-b", "o", b"b").ok();
    let original = server.get(ADMIN, "/storage/v1/b/bucket-a/o/o").ok().json();

    // objectViewer reads objects but cannot see bucket metadata or mutate objects.
    assert_eq!(
        server.get(VIEWER, "/storage/v1/b/bucket-a/o/o").ok().json(),
        original
    );
    assert_eq!(
        server
            .get(VIEWER, "/storage/v1/b/bucket-a/o/o?alt=media")
            .body,
        b"old"
    );
    server.get(VIEWER, "/storage/v1/b/bucket-a/o").ok();
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
    server.upload(CREATOR, "bucket-a", "created", b"new").ok();
    server
        .multipart(CREATOR, "bucket-a", "created-multi", b"new")
        .ok();
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
        .error(412, "conditionNotMet");
    assert_eq!(
        server.get(ADMIN, "/storage/v1/b/bucket-a/o/o").ok().json(),
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
    server
        .get(CREATOR, "/storage/v1/b/bucket-a/o/created")
        .forbidden();
    server
        .get(CREATOR, "/storage/v1/b/bucket-a/o/created?alt=media")
        .forbidden();
    server.get(CREATOR, "/storage/v1/b/bucket-a/o").forbidden();

    // storage.editor creates and lists buckets but lacks storage.buckets.get.
    server
        .create_bucket(EDITOR, "project-a", "editor-bucket")
        .ok();
    server.get(EDITOR, "/storage/v1/b?project=project-a").ok();
    server
        .get(EDITOR, "/storage/v1/b/editor-bucket")
        .forbidden();
    server
        .create_bucket(EDITOR, "project-b", "editor-b")
        .forbidden();
    server.get(EDITOR, "/storage/v1/b/bucket-a/o/o").forbidden();

    // Roles in project A never apply to a bucket owned by project B.
    server.get(PROJECT_A_ONLY, "/storage/v1/b/bucket-a").ok();
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
        .error(404, "notFound");
    server
        .get(VIEWER, "/storage/v1/b/missing-bucket/o/o")
        .error(404, "notFound");
    server
        .get(VIEWER, "/storage/v1/b/missing-bucket/o")
        .error(404, "notFound");
    server
        .upload(CREATOR, "missing-bucket", "o", b"x")
        .error(404, "notFound");
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
    server.create_bucket(ADMIN, "project-a", "bucket-a").ok();
    server.upload(ADMIN, "bucket-a", "exists", b"old").ok();
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
        Reply::read(stream).error(status, reason);
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
        server.get(ADMIN, "/storage/v1/b/bucket-a/o").ok().json()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn auth_http_upload_into_a_bucket_recreated_in_another_project_is_forbidden() {
    let config = TestDir::new();
    let dir = TestDir::new();
    let server = Server::strict(&dir, &identity_file(&config));
    server.create_bucket(ADMIN, "project-a", "race").ok();
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
        .ok();
    server.create_bucket(ADMIN, "project-b", "race").ok();
    stream.write_all(b"late").unwrap();
    // The caller holds no role in project-b, so publication must not reach the new bucket.
    Reply::read(stream).forbidden();
    let listing = server.get(ADMIN, "/storage/v1/b/race/o").ok().json();
    assert!(listing.get("items").is_none(), "{listing}");
    // The same caller still learns nothing about the recreated bucket's objects.
    server
        .get(PROJECT_A_ONLY, "/storage/v1/b/race/o/object?alt=media")
        .forbidden();
    server
        .get(PROJECT_A_ONLY, "/storage/v1/b/race/o")
        .forbidden();
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
}

#[test]
fn auth_http_concurrent_creates_without_delete_have_one_winner() {
    let config = TestDir::new();
    let dir = TestDir::new();
    let server = Server::strict(&dir, &identity_file(&config));
    server.create_bucket(ADMIN, "project-a", "bucket-a").ok();
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
    assert_eq!(std::fs::read_dir(dir.0.join("staging")).unwrap().count(), 0);
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
