mod common;

use common::TestDir;
use gcs_mimic::auth::signature::sign_url;
use hyper::{HeaderMap, Method, Uri};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Stdio},
    time::{Duration, SystemTime},
};

const ORIGIN: &str = "https://browser.example.invalid";
const DENIED: &str = "https://denied.example.invalid";
const ADMIN: &str = "Authorization: Bearer admin\r\n";
const PRIVATE: &[u8] = include_bytes!("../fixtures/signing/test-only-private.pem");
const PUBLIC: &str = include_str!("../fixtures/signing/test-only-public.pem");
const BUCKET: &str = "/storage/v1/b/browser-bucket";
const EXPOSED: &str = "Content-Type, ETag, X-Goog-Generation, X-Goog-If-Generation-Match";

struct Server {
    child: Child,
    address: String,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(data: &TestDir, config: &TestDir) -> Self {
        let identities = config.0.join("identities.json");
        std::fs::write(
            &identities,
            json!({"identities":[
                {"id":"admin@example.invalid","tokens":["admin"],"public_keys":[PUBLIC],
                 "project_roles":{"local":["roles/storage.admin"]}},
                {"id":"denied@example.invalid","tokens":[],"public_keys":[PUBLIC]}
            ]})
            .to_string(),
        )
        .unwrap();
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--api-base-url",
                "http://127.0.0.1:4443",
                "--data-dir",
            ])
            .arg(&data.0)
            .args(["--auth-mode", "strict", "--identity-file"])
            .arg(identities)
            .args(["--xml-virtual-host-domains", "storage.example.invalid"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).expect("ready event");
        assert_eq!(ready["event"], "ready");
        Self {
            child,
            address: ready["listen"].as_str().unwrap().into(),
            stdout,
        }
    }

    fn url(&self, virtual_host: bool, name: &str) -> String {
        if virtual_host {
            let port = self.address.rsplit_once(':').unwrap().1;
            format!("http://browser-bucket.storage.example.invalid:{port}/{name}")
        } else {
            format!("http://{}/browser-bucket/{name}", self.address)
        }
    }

    fn sign(&self, virtual_host: bool, method: &str, name: &str) -> String {
        signed(
            &self.url(virtual_host, name),
            method,
            "admin",
            SystemTime::now(),
            HeaderMap::new(),
        )
    }

    fn request(&self, method: &str, url: &str, headers: &str, body: &[u8]) -> Reply {
        let uri: Uri = url.parse().unwrap();
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write!(stream, "{method} {} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
            uri.path_and_query().unwrap(), uri.authority().unwrap(), body.len()).unwrap();
        stream.write_all(body).unwrap();
        Reply::read(stream, method == "HEAD")
    }

    fn json(&self, method: &str, path: &str, value: Value) -> Value {
        let body = if method == "GET" {
            String::new()
        } else {
            value.to_string()
        };
        let reply = self.request(
            method,
            &format!("http://{}{path}", self.address),
            &format!("{ADMIN}Content-Type: application/json\r\n"),
            body.as_bytes(),
        );
        reply.status(200);
        serde_json::from_slice(&reply.body).unwrap()
    }

    fn rules(&self, rules: Value) {
        self.json("PATCH", BUCKET, json!({"cors":rules}));
    }

    fn metadata(&self) -> Value {
        self.json("GET", &format!("{BUCKET}/o/object"), Value::Null)
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
    fn read(stream: TcpStream, head: bool) -> Self {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let mut headers = BTreeMap::new();
        loop {
            line.clear();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            let (key, value) = line.trim_end().split_once(':').unwrap();
            assert!(
                headers
                    .insert(key.to_ascii_lowercase(), value.trim().to_owned())
                    .is_none()
            );
        }
        let mut body = Vec::new();
        if head {
            reader.read_to_end(&mut body).unwrap();
            assert!(body.is_empty());
        } else if headers
            .get("transfer-encoding")
            .is_some_and(|value| value == "chunked")
        {
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                let size = usize::from_str_radix(line.trim(), 16).unwrap();
                if size == 0 {
                    break;
                }
                let start = body.len();
                body.resize(start + size, 0);
                reader.read_exact(&mut body[start..]).unwrap();
                let mut crlf = [0; 2];
                reader.read_exact(&mut crlf).unwrap();
                assert_eq!(&crlf, b"\r\n");
            }
        } else {
            reader.read_to_end(&mut body).unwrap();
        }
        Self {
            status,
            headers,
            body,
        }
    }

    fn status(&self, expected: u16) {
        assert_eq!(
            self.status,
            expected,
            "{}",
            String::from_utf8_lossy(&self.body)
        );
    }

    fn error(&self, status: u16, code: &str, head: bool) {
        self.status(status);
        if !head {
            assert!(String::from_utf8_lossy(&self.body).contains(&format!("<Code>{code}</Code>")));
        }
    }

    fn cors(&self, allowed: bool, preflight: bool) {
        assert!(
            !self
                .headers
                .contains_key("access-control-allow-credentials")
        );
        assert_eq!(
            self.headers["vary"],
            if preflight {
                "Origin, Access-Control-Request-Method, Access-Control-Request-Headers"
            } else {
                "Origin"
            }
        );
        if allowed {
            assert_eq!(self.headers["access-control-allow-origin"], ORIGIN);
            if preflight {
                assert_eq!(
                    self.headers["access-control-allow-methods"],
                    "GET, HEAD, PUT"
                );
                assert_eq!(self.headers["access-control-max-age"], "0");
                assert!(!self.headers.contains_key("access-control-expose-headers"));
            } else {
                assert_eq!(self.headers["access-control-expose-headers"], EXPOSED);
                assert!(!self.headers.contains_key("access-control-allow-methods"));
                assert!(!self.headers.contains_key("access-control-max-age"));
            }
        } else {
            assert!(
                !self
                    .headers
                    .keys()
                    .any(|key| key.starts_with("access-control-"))
            );
        }
    }
}

fn rule(origin: &str) -> Value {
    json!({"origin":[origin],"method":["GET","HEAD","PUT"],
        "responseHeader":["Content-Type","ETag","X-Goog-Generation","X-Goog-If-Generation-Match"],
        "maxAgeSeconds":0})
}

fn fixture() -> (TestDir, TestDir, Server) {
    let config = TestDir::new();
    let data = TestDir::new();
    let server = Server::start(&data, &config);
    server.json(
        "POST",
        "/storage/v1/b?project=local",
        json!({"name":"browser-bucket","cors":[rule(ORIGIN)]}),
    );
    let url = server.sign(false, "PUT", "object");
    server
        .request("PUT", &url, "Content-Type: text/plain\r\n", b"original")
        .status(200);
    (config, data, server)
}

fn signed(url: &str, method: &str, signer: &str, time: SystemTime, headers: HeaderMap) -> String {
    sign_url(
        url,
        &method.parse::<Method>().unwrap(),
        &format!("{signer}@example.invalid"),
        PRIVATE,
        300,
        time,
        headers,
    )
    .unwrap()
}

fn origin(origin: &str) -> String {
    format!("Origin: {origin}\r\n")
}

fn preflight(server: &Server, url: &str, source: &str, method: &str, headers: &str) -> Reply {
    server.request(
        "OPTIONS",
        url,
        &format!("Origin: {source}\r\nAccess-Control-Request-Method: {method}\r\n{headers}"),
        b"",
    )
}

#[test]
fn signed_put_preflight_ignores_signature_and_never_publishes() {
    let (_config, _data, server) = fixture();
    let before = server.metadata();
    for virtual_host in [false, true] {
        let url = server.sign(virtual_host, "PUT", "new");
        let headers =
            "Access-Control-Request-Headers: content-type, X-Goog-If-Generation-Match\r\n";
        for candidate in [
            url.clone(),
            format!("{url}&X-Goog-Signature=duplicate"),
            format!("{url}&unknown=1"),
            format!("{url}&generation=invalid"),
            format!("{url}&response-content-type="),
            format!("{url}&malformed=%ZZ"),
            url.replace(
                "X-Goog-Algorithm=GOOG4-RSA-SHA256",
                "X-Goog-Algorithm=invalid",
            ),
            signed(
                &server.url(virtual_host, "new"),
                "PUT",
                "denied",
                SystemTime::now() - Duration::from_secs(600),
                HeaderMap::new(),
            ),
        ] {
            let reply = preflight(&server, &candidate, ORIGIN, "PUT", headers);
            reply.status(200);
            reply.cors(true, true);
            assert_eq!(
                reply.headers["access-control-allow-headers"],
                "content-type, X-Goog-If-Generation-Match"
            );
            assert!(reply.body.is_empty());
        }
        for (source, method, headers) in [
            (DENIED, "PUT", headers),
            (ORIGIN, "DELETE", headers),
            (
                ORIGIN,
                "PUT",
                "Access-Control-Request-Headers: content-type, x-denied\r\n",
            ),
            (ORIGIN, "put", ""),
            (ORIGIN, "", ""),
            (
                ORIGIN,
                "PUT",
                "Access-Control-Request-Headers: invalid header\r\n",
            ),
        ] {
            let reply = preflight(&server, &url, source, method, headers);
            reply.status(200);
            reply.cors(false, true);
        }
        let absent = server.request("OPTIONS", &url, &origin(ORIGIN), b"");
        absent.status(200);
        absent.cors(false, true);
        server
            .request("GET", &server.url(virtual_host, "new"), ADMIN, b"")
            .status(404);
        assert_eq!(server.metadata(), before);
        for _ in 0..2 {
            server
                .request("PUT", &url, &origin(ORIGIN), b"uploaded")
                .status(200);
        }
        server
            .request("GET", &server.url(virtual_host, "new"), ADMIN, b"")
            .status(200);
        server
            .request(
                "DELETE",
                &format!("http://{}{BUCKET}/o/new", server.address),
                ADMIN,
                b"",
            )
            .status(204);
    }
}

#[test]
fn signed_cors_selects_the_host_bucket_before_the_path_bucket() {
    let (_config, _data, server) = fixture();
    server.json(
        "POST",
        "/storage/v1/b?project=local",
        json!({
            "name":"other-bucket","cors":[rule(DENIED)]
        }),
    );
    let virtual_url = server.url(true, "other-bucket/object");
    let path_url = format!("http://{}/other-bucket/object", server.address);
    for (base, source, bytes) in [
        (&virtual_url, ORIGIN, &b"virtual bucket"[..]),
        (&path_url, DENIED, &b"path bucket"[..]),
    ] {
        let put = signed(base, "PUT", "admin", SystemTime::now(), HeaderMap::new());
        for candidate in [ORIGIN, DENIED] {
            let reply = preflight(&server, &put, candidate, "PUT", "");
            reply.status(200);
            if candidate == source {
                assert_eq!(reply.headers["access-control-allow-origin"], source);
            } else {
                reply.cors(false, true);
            }
        }
        let reply = server.request("PUT", &put, &origin(source), bytes);
        reply.status(200);
        assert_eq!(reply.headers["access-control-allow-origin"], source);
        let get = signed(base, "GET", "admin", SystemTime::now(), HeaderMap::new());
        let reply = server.request("GET", &get, &origin(source), b"");
        reply.status(200);
        assert_eq!(reply.headers["access-control-allow-origin"], source);
        assert_eq!(reply.body, bytes);
    }
}

#[test]
fn signed_encoded_paths_keep_preflight_transfer_and_error_cors() {
    let (_config, _data, server) = fixture();
    for virtual_host in [false, true] {
        let put = server.sign(virtual_host, "PUT", "folder%2Fa%20b");
        let reply = preflight(&server, &put, ORIGIN, "PUT", "");
        reply.status(200);
        reply.cors(true, true);
        let reply = server.request("PUT", &put, &origin(ORIGIN), b"encoded bytes");
        reply.status(200);
        reply.cors(true, false);
        let get = server.sign(virtual_host, "GET", "folder%2Fa%20b");
        let reply = server.request("GET", &get, &origin(ORIGIN), b"");
        reply.status(200);
        reply.cors(true, false);
        assert_eq!(reply.body, b"encoded bytes");
        assert_eq!(
            server.json("GET", &format!("{BUCKET}/o/folder%2Fa%20b"), Value::Null)["name"],
            "folder/a b"
        );
        let reply = server.request("GET", &get.replace("%20b", "%20c"), &origin(ORIGIN), b"");
        reply.error(403, "SignatureDoesNotMatch", false);
        reply.cors(true, false);
    }
}

#[test]
fn signed_invalid_object_names_keep_error_cors_without_bypassing_validation() {
    let (_config, _data, server) = fixture();
    let before = server.metadata();
    for virtual_host in [false, true] {
        for method in ["GET", "HEAD", "PUT"] {
            let url = server.sign(virtual_host, method, ".");
            for source in [ORIGIN, DENIED] {
                for candidate in [&url, &server.url(virtual_host, "%ZZ")] {
                    let reply = preflight(&server, candidate, source, method, "");
                    reply.status(200);
                    reply.cors(source == ORIGIN, true);
                }
                let reply = server.request(method, &url, &origin(source), b"");
                reply.error(400, "InvalidArgument", method == "HEAD");
                reply.cors(source == ORIGIN, false);
                let malformed = format!(
                    "{}?X-Goog-Signature=invalid",
                    server.url(virtual_host, "%ZZ")
                );
                let reply = server.request(method, &malformed, &origin(source), b"");
                reply.error(400, "InvalidAuthentication", method == "HEAD");
                reply.cors(source == ORIGIN, false);
                assert_eq!(server.metadata(), before);
            }
        }
    }
}

#[test]
fn invalid_virtual_host_bucket_names_reject_preflight_without_cors() {
    let (_config, _data, server) = fixture();
    let url = server
        .url(true, "object")
        .replace("browser-bucket.", "bad_.");
    for source in [ORIGIN, DENIED] {
        let reply = preflight(&server, &url, source, "PUT", "");
        reply.error(400, "InvalidArgument", false);
        assert!(!reply.headers.contains_key("vary"));
        assert!(
            reply
                .headers
                .keys()
                .all(|name| !name.starts_with("access-control-"))
        );
        let reply = server.request("GET", &url, &origin(source), b"");
        reply.error(400, "InvalidArgument", false);
        assert!(!reply.headers.contains_key("vary"));
    }
}

#[test]
fn signed_transfers_expose_bucket_headers_without_granting_credentials() {
    let (_config, _data, server) = fixture();
    for virtual_host in [false, true] {
        for method in ["PUT", "GET", "HEAD"] {
            let mut required = HeaderMap::new();
            required.insert("content-type", "text/plain".parse().unwrap());
            let url = signed(
                &server.url(virtual_host, "object"),
                method,
                "admin",
                SystemTime::now(),
                required,
            );
            for source in [ORIGIN, DENIED] {
                let reply = server.request(
                    method,
                    &url,
                    &format!("{}Content-Type: text/plain\r\n", origin(source)),
                    if method == "PUT" {
                        b"browser bytes"
                    } else {
                        b""
                    },
                );
                reply.status(200);
                reply.cors(source == ORIGIN, false);
                assert!(!reply.headers["etag"].is_empty());
                assert!(!reply.headers["x-goog-generation"].is_empty());
                if method == "GET" {
                    assert_eq!(reply.body, b"browser bytes");
                }
                if method == "HEAD" {
                    assert_eq!(reply.headers["content-length"], "13");
                }
            }
            let plain = server.request(
                method,
                &url,
                "Content-Type: text/plain\r\n",
                if method == "PUT" {
                    b"browser bytes"
                } else {
                    b""
                },
            );
            plain.status(200);
            assert!(!plain.headers.contains_key("access-control-allow-origin"));
        }
    }
}

#[test]
fn signed_failures_keep_cors_and_preserve_object() {
    let (_config, _data, server) = fixture();
    let before = server.metadata();
    for virtual_host in [false, true] {
        for method in ["PUT", "GET", "HEAD"] {
            let base = server.url(virtual_host, "object");
            let url = server.sign(virtual_host, method, "object");
            for (candidate, status, code) in [
                (format!("{url}&tampered=1"), 403, "SignatureDoesNotMatch"),
                (
                    url.replace(
                        &format!(":{}/", server.address.rsplit_once(':').unwrap().1),
                        ":1/",
                    ),
                    403,
                    "SignatureDoesNotMatch",
                ),
                (
                    signed(
                        &base,
                        method,
                        "admin",
                        SystemTime::now() - Duration::from_secs(600),
                        HeaderMap::new(),
                    ),
                    400,
                    "ExpiredToken",
                ),
                (
                    signed(&base, method, "denied", SystemTime::now(), HeaderMap::new()),
                    403,
                    "AccessDenied",
                ),
                (
                    format!("{url}&X-Goog-Signature=duplicate"),
                    400,
                    "InvalidAuthentication",
                ),
                (
                    format!("{base}?X-Goog-Date=invalid"),
                    400,
                    "InvalidAuthentication",
                ),
                (base, 401, "AuthenticationRequired"),
            ] {
                for source in [ORIGIN, DENIED] {
                    let reply = server.request(
                        method,
                        &candidate,
                        &origin(source),
                        if method == "PUT" {
                            b"must not publish"
                        } else {
                            b""
                        },
                    );
                    reply.error(status, code, method == "HEAD");
                    reply.cors(source == ORIGIN, false);
                    assert_eq!(server.metadata(), before);
                }
            }
        }
        let mut headers = HeaderMap::new();
        headers.insert("x-goog-if-generation-match", "0".parse().unwrap());
        let conditional = signed(
            &server.url(virtual_host, "object"),
            "PUT",
            "admin",
            SystemTime::now(),
            headers,
        );
        let bad_digest = server.sign(virtual_host, "PUT", "object");
        for source in [ORIGIN, DENIED] {
            for (url, headers, status, code) in [
                (
                    &conditional,
                    "x-goog-if-generation-match: 0\r\n",
                    412,
                    "PreconditionFailed",
                ),
                (
                    &bad_digest,
                    "Content-MD5: invalid\r\n",
                    400,
                    "InvalidDigest",
                ),
            ] {
                let reply = server.request(
                    "PUT",
                    url,
                    &format!("{}{headers}", origin(source)),
                    b"must not publish",
                );
                reply.error(status, code, false);
                reply.cors(source == ORIGIN, false);
                assert_eq!(server.metadata(), before);
            }
        }
    }
    assert_eq!(
        server
            .request("GET", &server.url(false, "object"), ADMIN, b"")
            .body,
        b"original"
    );
}

#[test]
fn signed_rules_change_between_preflight_and_actual_and_survive_restart() {
    let (config, data, mut server) = fixture();
    for virtual_host in [false, true] {
        let url = server.sign(virtual_host, "PUT", "object");
        preflight(&server, &url, ORIGIN, "PUT", "").cors(true, true);
        server.rules(json!([rule(DENIED)]));
        let reply = server.request("PUT", &url, &origin(ORIGIN), b"changed after preflight");
        reply.status(200);
        reply.cors(false, false);
        server.rules(json!([]));
        let get = server.sign(virtual_host, "GET", "object");
        let reply = server.request("GET", &get, &origin(ORIGIN), b"");
        reply.status(200);
        reply.cors(false, false);
        let json = server.request(
            "GET",
            &format!("http://{}{BUCKET}/o/object?alt=media", server.address),
            &format!("{ADMIN}{}", origin(ORIGIN)),
            b"",
        );
        json.status(200);
        assert_eq!(json.headers["access-control-allow-origin"], ORIGIN);
        assert_eq!(json.headers["access-control-allow-credentials"], "true");
        server.rules(json!([rule(ORIGIN)]));
    }
    let before = server.metadata();
    server.stop();
    drop(server);
    let server = Server::start(&data, &config);
    assert_eq!(server.metadata(), before);
    for virtual_host in [false, true] {
        let get = server.sign(virtual_host, "GET", "object");
        let reply = server.request("GET", &get, &origin(ORIGIN), b"");
        reply.status(200);
        reply.cors(true, false);
        assert_eq!(reply.body, b"changed after preflight");
        let put = server.sign(virtual_host, "PUT", "object");
        preflight(&server, &put, ORIGIN, "PUT", "").cors(true, true);
    }
}

#[test]
fn signed_preflight_never_grants_permission_and_pap_preserves_signer_access() {
    let (_config, _data, server) = fixture();
    server.json(
        "PUT",
        &format!("{BUCKET}/iam"),
        json!({"bindings":[
            {"role":"roles/storage.objectViewer","members":["allUsers"]}
        ]}),
    );
    server.json(
        "PATCH",
        BUCKET,
        json!({"iamConfiguration":{"publicAccessPrevention":"enforced"}}),
    );
    for virtual_host in [false, true] {
        let before = server.metadata();
        let base = server.url(virtual_host, "object");
        for method in ["GET", "HEAD", "PUT"] {
            let denied = signed(&base, method, "denied", SystemTime::now(), HeaderMap::new());
            preflight(&server, &denied, ORIGIN, method, "").cors(true, true);
            let reply = server.request(method, &denied, &origin(ORIGIN), b"");
            reply.error(403, "AccessDenied", method == "HEAD");
            reply.cors(true, false);
            let unsigned = server.request(method, &base, &origin(ORIGIN), b"");
            unsigned.error(401, "AuthenticationRequired", method == "HEAD");
            unsigned.cors(true, false);
            assert_eq!(server.metadata(), before);
        }
        for method in ["GET", "HEAD", "PUT"] {
            let url = server.sign(virtual_host, method, "object");
            let reply = server.request(
                method,
                &url,
                &origin(ORIGIN),
                if method == "PUT" { b"original" } else { b"" },
            );
            reply.status(200);
            reply.cors(true, false);
        }
    }
}
