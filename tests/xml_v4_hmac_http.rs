mod object_effects_support;
mod xml_v2_hmac;
mod xml_v4_streaming;

use gcs_mimic::auth::signature::{encode_query_component, timestamp};
use object_effects_support::{
    Reply,
    common::{self, TestDir},
};
use ring::{digest, hmac};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Command, Stdio},
    time::{Duration, SystemTime},
};

const PUBLIC: &str = include_str!("../fixtures/signing/test-only-public.pem");
const PRIVATE: &[u8] = include_bytes!("../fixtures/signing/test-only-private.pem");
const KEYS: &str = "/storage/v1/projects/local/hmacKeys";
const ADMIN: &str = "Authorization: Bearer admin\r\n";

struct Server {
    child: Child,
    stdout: BufReader<ChildStdout>,
    address: String,
}
struct Key {
    id: String,
    secret: String,
}
struct Signed {
    path: String,
    headers: String,
}

impl Server {
    fn raw(&self, wire: &str, method: &str) -> Reply {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream.write_all(wire.as_bytes()).unwrap();
        let mut reader = BufReader::new(stream);
        let mut bytes = common::read_http_response(&mut reader, method == "HEAD").unwrap();
        reader.read_to_end(&mut bytes).unwrap();
        let boundary = bytes
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap();
        let head = std::str::from_utf8(&bytes[..boundary]).unwrap();
        Reply {
            status: head.split_whitespace().nth(1).unwrap().parse().unwrap(),
            headers: head
                .lines()
                .skip(1)
                .map(|line| {
                    let (name, value) = line.split_once(':').unwrap();
                    (name.to_ascii_lowercase(), value.trim().into())
                })
                .collect(),
            body: bytes[boundary + 4..].to_vec(),
        }
    }
    fn start(dir: &TestDir, backend: &str) -> Self {
        Self::start_with_limit(dir, backend, 64 * 1024 * 1024)
    }
    fn start_with_limit(dir: &TestDir, backend: &str, max_bytes: u64) -> Self {
        let identities = dir.0.join("identities.json");
        std::fs::write(&identities, json!({"identities":[
            {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
            {"id":"signer@example.invalid","tokens":[],"public_keys":[PUBLIC],"project_roles":{"local":["roles/storage.admin"]}},
            {"id":"reader@example.invalid","tokens":[],"project_roles":{"local":["roles/storage.objectViewer"]}},
            {"id":"creator@example.invalid","tokens":[],"project_roles":{"local":["roles/storage.objectCreator"]}},
            {"id":"keys@example.invalid","tokens":["keys"],"project_roles":{"local":["roles/storage.hmacKeyAdmin"]}},
            {"id":"scoped@example.invalid","tokens":[]},
            {"id":"stranger@example.invalid","tokens":[],"project_roles":{"other":["roles/storage.admin"]}}
        ]}).to_string()).unwrap();
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--backend",
                backend,
                "--data-dir",
            ])
            .arg(dir.0.join("data"))
            .args([
                "--api-base-url",
                "http://127.0.0.1:4443",
                "--max-object-bytes",
                &max_bytes.to_string(),
            ])
            .args([
                "--public-host",
                "storage.example.invalid",
                "--xml-virtual-host-domains",
                "storage.example.invalid",
                "--auth-mode",
                "strict",
                "--identity-file",
            ])
            .arg(identities)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut server = Self {
            child,
            stdout,
            address: String::new(),
        };
        let mut line = String::new();
        server.stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready");
        server.address = ready["listen"].as_str().unwrap().into();
        server
    }
    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        object_effects_support::request(&self.address, method, path, headers, body)
    }
    fn signed(&self, method: &str, signed: &Signed, body: &[u8]) -> Reply {
        self.request(method, &signed.path, &signed.headers, body)
    }
    fn bucket(&self, name: &str) {
        self.request(
            "POST",
            "/storage/v1/b?project=local",
            ADMIN,
            json!({"name":name}).to_string().as_bytes(),
        )
        .json();
    }
    fn key(&self, email: &str) -> Key {
        let value = self
            .request(
                "POST",
                &format!(
                    "{KEYS}?serviceAccountEmail={}",
                    encode_query_component(email)
                ),
                "Authorization: Bearer keys\r\n",
                b"",
            )
            .json();
        Key {
            id: value["metadata"]["accessId"].as_str().unwrap().into(),
            secret: value["secret"].as_str().unwrap().into(),
        }
    }
    fn stop(&mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => {
                if let Err(error) = self.child.kill() {
                    eprintln!("fixture kill failed: {error}");
                }
                if let Err(error) = self.child.wait() {
                    eprintln!("fixture wait failed: {error}");
                }
            }
            Err(error) => eprintln!("fixture status failed: {error}"),
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn signature(key: &Key, aws: bool, scope: &str, message: &str) -> String {
    let prefix = if aws { "AWS4" } else { "GOOG4" };
    let mut signing = hmac::Key::new(
        hmac::HMAC_SHA256,
        format!("{prefix}{}", key.secret).as_bytes(),
    );
    for part in scope.split('/') {
        signing = hmac::Key::new(
            hmac::HMAC_SHA256,
            hmac::sign(&signing, part.as_bytes()).as_ref(),
        );
    }
    hex(hmac::sign(&signing, message.as_bytes()).as_ref())
}

fn sign(
    key: &Key,
    method: &str,
    target: &str,
    aws: bool,
    header: bool,
    extras: &[(&str, &str)],
    now: SystemTime,
) -> Signed {
    let date = timestamp(now).unwrap();
    let scope = format!(
        "{}/auto/{}/{}",
        &date[..8],
        if aws { "s3" } else { "storage" },
        if aws { "aws4_request" } else { "goog4_request" }
    );
    let algorithm = if aws {
        "AWS4-HMAC-SHA256"
    } else {
        "GOOG4-HMAC-SHA256"
    };
    let prefix = if aws { "X-Amz-" } else { "X-Goog-" };
    let (path, raw_query) = target.split_once('?').unwrap_or((target, ""));
    let mut query: BTreeMap<String, String> = raw_query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (name.into(), value.into())
        })
        .collect();
    let mut headers: BTreeMap<String, Vec<String>> =
        BTreeMap::from([("host".into(), vec!["localhost".into()])]);
    for (name, value) in extras {
        if *name == "host" {
            headers.insert("host".into(), vec![(*value).into()]);
            continue;
        }
        headers
            .entry((*name).into())
            .or_default()
            .push(value.split_ascii_whitespace().collect::<Vec<_>>().join(" "));
    }
    let hash_name = if aws {
        "x-amz-content-sha256"
    } else {
        "x-goog-content-sha256"
    };
    let default_payload = header && !headers.contains_key(hash_name);
    if default_payload {
        headers.insert(hash_name.into(), vec!["UNSIGNED-PAYLOAD".into()]);
    }
    if header {
        headers.insert(
            if aws { "x-amz-date" } else { "x-goog-date" }.into(),
            vec![date.clone()],
        );
    }
    let names = headers.keys().cloned().collect::<Vec<_>>().join(";");
    if !header {
        for (name, value) in [
            ("Algorithm", algorithm.into()),
            ("Credential", format!("{}/{scope}", key.id)),
            ("Date", date.clone()),
            ("Expires", "60".into()),
            ("SignedHeaders", names.clone()),
        ] {
            query.insert(format!("{prefix}{name}"), value);
        }
    }
    let encoded = |query: &BTreeMap<String, String>| {
        query
            .iter()
            .map(|(name, value)| {
                format!(
                    "{}={}",
                    encode_query_component(name),
                    encode_query_component(value)
                )
            })
            .collect::<Vec<_>>()
            .join("&")
    };
    let canonical_headers = headers
        .iter()
        .map(|(name, values)| format!("{name}:{}\n", values.join(",")))
        .collect::<String>();
    let payload = if header {
        extras
            .iter()
            .find(|(name, _)| name.ends_with("content-sha256"))
            .map_or("UNSIGNED-PAYLOAD", |(_, value)| *value)
    } else {
        "UNSIGNED-PAYLOAD"
    };
    let canonical = format!(
        "{method}\n{path}\n{}\n{canonical_headers}\n{names}\n{payload}",
        encoded(&query)
    );
    let message = format!(
        "{algorithm}\n{date}\n{scope}\n{}",
        hex(digest::digest(&digest::SHA256, canonical.as_bytes()).as_ref())
    );
    let sig = signature(key, aws, &scope, &message);
    let mut wire = extras
        .iter()
        .filter(|(name, _)| *name != "host")
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect::<String>();
    if default_payload {
        wire.push_str(&format!("{hash_name}: UNSIGNED-PAYLOAD\r\n"));
    }
    if header {
        wire.push_str(&format!("{}: {date}\r\nAuthorization: {algorithm} Credential={}/{scope}, SignedHeaders={names}, Signature={sig}\r\n",if aws { "x-amz-date" } else { "x-goog-date" },key.id));
    } else {
        query.insert(format!("{prefix}Signature"), sig);
    }
    let query = encoded(&query);
    Signed {
        path: if query.is_empty() {
            path.into()
        } else {
            format!("{path}?{query}")
        },
        headers: wire,
    }
}

fn error(reply: &Reply, status: u16, code: &str) {
    assert_eq!(
        reply.status,
        status,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    assert!(
        String::from_utf8_lossy(&reply.body).contains(&format!("<Code>{code}</Code>")),
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
}

#[test]
fn hmac_query_and_header_transfer_on_disk_and_memory() {
    for backend in ["disk", "memory"] {
        let dir = TestDir::new();
        let server = Server::start(&dir, backend);
        server.bucket("test-bucket");
        let key = server.key("signer@example.invalid");
        for aws in [false, true] {
            for header in [false, true] {
                let path = format!("/test-bucket/bytes-{aws}-{header}%252Fraw");
                let put = sign(&key, "PUT", &path, aws, header, &[], SystemTime::now());
                assert_eq!(server.signed("PUT", &put, b"payload").status, 200);
                for method in ["GET", "HEAD"] {
                    let signed = sign(&key, method, &path, aws, header, &[], SystemTime::now());
                    let reply = server.signed(method, &signed, b"");
                    assert_eq!(
                        reply.status,
                        200,
                        "{}",
                        String::from_utf8_lossy(&reply.body)
                    );
                    assert_eq!(
                        reply.body,
                        if method == "HEAD" {
                            b"".as_slice()
                        } else {
                            b"payload".as_slice()
                        }
                    );
                }
                let signed = sign(&key, "DELETE", &path, aws, header, &[], SystemTime::now());
                assert_eq!(server.signed("DELETE", &signed, b"").status, 204);
            }
        }
        let reader = server.key("reader@example.invalid");
        let put = sign(
            &reader,
            "PUT",
            "/test-bucket/denied",
            false,
            true,
            &[],
            SystemTime::now(),
        );
        error(&server.signed("PUT", &put, b"denied"), 403, "AccessDenied");
    }
}

#[test]
fn hmac_revocation_restart_and_identity_permissions_do_not_follow_key_project() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    let path = "/test-bucket/object";
    assert_eq!(
        server
            .signed(
                "PUT",
                &sign(&key, "PUT", path, false, true, &[], SystemTime::now()),
                b"durable"
            )
            .status,
        200
    );
    server.stop();
    server = Server::start(&dir, "disk");
    let get = sign(&key, "GET", path, true, false, &[], SystemTime::now());
    assert_eq!(server.signed("GET", &get, b"").body, b"durable");
    for email in [
        "unregistered@example.invalid",
        "keys@example.invalid",
        "stranger@example.invalid",
        "scoped@example.invalid",
    ] {
        let other = server.key(email);
        let signed = sign(&other, "GET", path, false, true, &[], SystemTime::now());
        error(
            &server.signed("GET", &signed, b""),
            403,
            if email.starts_with("unregistered") {
                "InvalidAccessKeyId"
            } else {
                "AccessDenied"
            },
        );
        let signed = sign(&other, "PUT", path, true, false, &[], SystemTime::now());
        error(
            &server.signed("PUT", &signed, b"not allowed"),
            403,
            if email.starts_with("unregistered") {
                "InvalidAccessKeyId"
            } else {
                "AccessDenied"
            },
        );
    }
    let key_path = format!("{KEYS}/{}", key.id);
    let admin = "Authorization: Bearer keys\r\n";
    server
        .request("PUT", &key_path, admin, br#"{"state":"INACTIVE"}"#)
        .json();
    error(&server.signed("GET", &get, b""), 403, "InvalidAccessKeyId");
    server
        .request("PUT", &key_path, admin, br#"{"state":"ACTIVE"}"#)
        .json();
    assert_eq!(server.signed("GET", &get, b"").body, b"durable");
    server
        .request("PUT", &key_path, admin, br#"{"state":"INACTIVE"}"#)
        .json();
    assert_eq!(server.request("DELETE", &key_path, admin, b"").status, 204);
    error(&server.signed("GET", &get, b""), 403, "InvalidAccessKeyId");
    server.stop();
    server = Server::start(&dir, "disk");
    error(&server.signed("GET", &get, b""), 403, "InvalidAccessKeyId");
    assert_eq!(server.request("GET", path, ADMIN, b"").body, b"durable");
}

#[test]
fn signed_payload_hash_mismatch_preserves_bytes_metadata_generation_and_count() {
    for backend in ["disk", "memory"] {
        let dir = TestDir::new();
        let server = Server::start(&dir, backend);
        server.bucket("test-bucket");
        let key = server.key("signer@example.invalid");
        let path = "/test-bucket/object";
        assert_eq!(
            server
                .signed(
                    "PUT",
                    &sign(
                        &key,
                        "PUT",
                        path,
                        false,
                        true,
                        &[("x-goog-meta-version", "old")],
                        SystemTime::now()
                    ),
                    b"old"
                )
                .status,
            200
        );
        let before = server
            .request("GET", "/storage/v1/b/test-bucket/o/object", ADMIN, b"")
            .json();
        let list = server
            .request("GET", "/storage/v1/b/test-bucket/o", ADMIN, b"")
            .json();
        let digest = hex(digest::digest(&digest::SHA256, b"correct").as_ref());
        for aws in [false, true] {
            for header in [false, true] {
                let name = if aws {
                    "x-amz-content-sha256"
                } else {
                    "x-goog-content-sha256"
                };
                let signed = sign(
                    &key,
                    "PUT",
                    path,
                    aws,
                    header,
                    &[(name, &digest), ("x-goog-meta-version", "new")],
                    SystemTime::now(),
                );
                error(&server.signed("PUT", &signed, b"changed"), 400, "BadDigest");
                assert_eq!(
                    server
                        .request("GET", "/storage/v1/b/test-bucket/o/object", ADMIN, b"")
                        .json(),
                    before
                );
                assert_eq!(
                    server
                        .request("GET", "/storage/v1/b/test-bucket/o", ADMIN, b"")
                        .json(),
                    list
                );
                assert_eq!(server.request("GET", path, ADMIN, b"").body, b"old");
            }
        }
        let signed = sign(
            &key,
            "PUT",
            path,
            true,
            true,
            &[("x-amz-content-sha256", &digest)],
            SystemTime::now(),
        );
        assert_eq!(server.signed("PUT", &signed, b"correct").status, 200);
    }
}

#[test]
fn hmac_bucket_and_multipart_operations_reuse_shared_catalog() {
    for backend in ["disk", "memory"] {
        let dir = TestDir::new();
        let server = Server::start(&dir, backend);
        let key = server.key("signer@example.invalid");
        let now = SystemTime::now();
        let create = sign(
            &key,
            "PUT",
            "/test-bucket",
            false,
            true,
            &[("x-goog-project-id", "local")],
            now,
        );
        assert_eq!(server.signed("PUT", &create, b"").status, 200);
        let list = sign(
            &key,
            "GET",
            "/",
            true,
            true,
            &[("x-goog-project-id", "local")],
            now,
        );
        assert!(
            String::from_utf8_lossy(&server.signed("GET", &list, b"").body)
                .contains("<Name>test-bucket</Name>")
        );
        for (aws, header) in [(false, false), (true, true)] {
            assert_eq!(
                server
                    .signed(
                        "HEAD",
                        &sign(&key, "HEAD", "/test-bucket", aws, header, &[], now),
                        b""
                    )
                    .status,
                200
            );
        }
        let path = "/test-bucket/multipart";
        let begin = server.signed(
            "POST",
            &sign(
                &key,
                "POST",
                &format!("{path}?uploads="),
                true,
                true,
                &[],
                now,
            ),
            b"",
        );
        assert_eq!(
            begin.status,
            200,
            "{}",
            String::from_utf8_lossy(&begin.body)
        );
        let body = String::from_utf8(begin.body).unwrap();
        let id = body
            .split_once("<UploadId>")
            .unwrap()
            .1
            .split_once("</UploadId>")
            .unwrap()
            .0;
        let part_path = format!("{path}?partNumber=1&uploadId={id}");
        let part = server.signed(
            "PUT",
            &sign(&key, "PUT", &part_path, false, false, &[], now),
            b"one part",
        );
        assert_eq!(part.status, 200);
        let manifest = format!(
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>{}</ETag></Part></CompleteMultipartUpload>",
            part.headers["etag"]
        );
        let finish = format!("{path}?uploadId={id}");
        assert_eq!(
            server
                .signed(
                    "POST",
                    &sign(&key, "POST", &finish, false, true, &[], now),
                    manifest.as_bytes()
                )
                .status,
            200
        );
        assert_eq!(server.request("GET", path, ADMIN, b"").body, b"one part");
        error(
            &server.signed(
                "DELETE",
                &sign(&key, "DELETE", "/test-bucket", true, true, &[], now),
                b"",
            ),
            409,
            "BucketNotEmpty",
        );
        assert_eq!(
            server
                .signed(
                    "DELETE",
                    &sign(&key, "DELETE", path, false, true, &[], now),
                    b""
                )
                .status,
            204
        );
        assert_eq!(
            server
                .signed(
                    "DELETE",
                    &sign(&key, "DELETE", "/test-bucket", true, false, &[], now),
                    b""
                )
                .status,
            204
        );
    }
}

#[test]
fn curl_aws_sigv4_independently_verifies_header_get_and_hashed_put() {
    let capability = Command::new("curl")
        .args(["--help", "all"])
        .output()
        .unwrap();
    assert!(capability.status.success());
    assert!(String::from_utf8_lossy(&capability.stdout).contains("--aws-sigv4"));
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    for provider in ["aws:amz:auto:s3", "goog:goog:auto:storage"] {
        for (method, body) in [("PUT", "independent curl payload"), ("GET", "")] {
            let digest = hex(digest::digest(&digest::SHA256, body.as_bytes()).as_ref());
            let hash_header = format!(
                "{}-content-sha256: {digest}",
                if provider.starts_with("aws:") {
                    "x-amz"
                } else {
                    "x-goog"
                }
            );
            let mut child = Command::new("curl")
                .args([
                    "--disable",
                    "--silent",
                    "--show-error",
                    "--fail-with-body",
                    "--noproxy",
                    "*",
                    "--retry",
                    "0",
                    "--max-time",
                    "15",
                    "--aws-sigv4",
                    provider,
                    "--config",
                    "-",
                    "--header",
                    "Host: localhost",
                    "--request",
                    method,
                    "--data-binary",
                    body,
                ])
                .arg(format!("http://{}/test-bucket/curl", server.address))
                .args(["--header", &hash_header])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            writeln!(
                child.stdin.take().unwrap(),
                "user = \"{}:{}\"",
                key.id,
                key.secret
            )
            .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stdout)
            );
            if method == "GET" {
                assert_eq!(output.stdout, b"independent curl payload");
            }
        }
    }
}

#[test]
fn rsa_authorization_headers_and_existing_query_helpers_transfer_the_same_object() {
    use gcs_mimic::auth::signature::{sign_policy, sign_url};
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let path = "/test-bucket/rsa%252Fheader";
    let hash = hex(digest::digest(&digest::SHA256, b"rsa bytes").as_ref());
    for method in ["PUT", "GET", "HEAD", "DELETE"] {
        let date = timestamp(SystemTime::now()).unwrap();
        let scope = format!("{}/auto/storage/goog4_request", &date[..8]);
        let payload = if method == "PUT" {
            hash.as_str()
        } else {
            "UNSIGNED-PAYLOAD"
        };
        let signed_headers = "host;x-goog-content-sha256;x-goog-date";
        let canonical = format!(
            "{method}\n{path}\n\nhost:localhost\nx-goog-content-sha256:{payload}\nx-goog-date:{date}\n\n{signed_headers}\n{payload}"
        );
        let message = format!(
            "GOOG4-RSA-SHA256\n{date}\n{scope}\n{}",
            hex(digest::digest(&digest::SHA256, canonical.as_bytes()).as_ref())
        );
        let signature = sign_policy(&message, PRIVATE).unwrap();
        let headers = format!(
            "x-goog-date: {date}\r\nx-goog-content-sha256: {payload}\r\nAuthorization: GOOG4-RSA-SHA256 Credential=signer@example.invalid/{scope}, SignedHeaders={signed_headers}, Signature={signature}\r\n"
        );
        let reply = server.request(
            method,
            path,
            &headers,
            if method == "PUT" { b"rsa bytes" } else { b"" },
        );
        assert_eq!(
            reply.status,
            if method == "DELETE" { 204 } else { 200 },
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
        if method == "PUT" {
            let headers = hyper::HeaderMap::new();
            let query = sign_url(
                &format!("http://localhost{path}"),
                &hyper::Method::GET,
                "signer@example.invalid",
                PRIVATE,
                60,
                SystemTime::now(),
                headers,
            )
            .unwrap();
            let uri: hyper::Uri = query.parse().unwrap();
            assert_eq!(
                server
                    .request("GET", uri.path_and_query().unwrap().as_str(), "", b"")
                    .body,
                b"rsa bytes"
            );
        }
    }
}

#[test]
fn malformed_mixed_tampered_and_expired_signatures_never_fall_back_or_reach_json() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    let path = "/test-bucket/object";
    let now = SystemTime::now();
    assert_eq!(
        server
            .signed(
                "PUT",
                &sign(&key, "PUT", path, false, true, &[], now),
                b"original"
            )
            .status,
        200
    );
    for aws in [false, true] {
        for header in [false, true] {
            let good = sign(&key, "GET", path, aws, header, &[], now);
            assert_eq!(server.signed("GET", &good, b"").status, 200);
            error(
                &server.signed("DELETE", &good, b""),
                403,
                "SignatureDoesNotMatch",
            );
            let moved = Signed {
                path: good.path.replace("/object", "/other"),
                headers: good.headers.clone(),
            };
            error(
                &server.signed("GET", &moved, b""),
                403,
                "SignatureDoesNotMatch",
            );
            let extra_query = Signed {
                path: format!(
                    "{}{}generation=1",
                    good.path,
                    if good.path.contains('?') { "&" } else { "?" }
                ),
                headers: good.headers.clone(),
            };
            error(
                &server.signed("GET", &extra_query, b""),
                403,
                "SignatureDoesNotMatch",
            );
            let stale = sign(
                &key,
                "GET",
                path,
                aws,
                header,
                &[],
                now - Duration::from_secs(3600),
            );
            error(&server.signed("GET", &stale, b""), 400, "ExpiredToken");
            let future = sign(
                &key,
                "GET",
                path,
                aws,
                header,
                &[],
                now + Duration::from_secs(3600),
            );
            error(
                &server.signed("GET", &future, b""),
                403,
                "RequestTimeTooSkewed",
            );
            let json = sign(
                &key,
                "GET",
                "/storage/v1/b/test-bucket/o/object",
                aws,
                header,
                &[],
                now,
            );
            server.signed("GET", &json, b"").error(401, "authError");
            let mut duplicate = Signed {
                path: good.path.clone(),
                headers: good.headers.clone(),
            };
            if header {
                duplicate.headers.push_str(&good.headers);
            } else {
                duplicate.path.push_str(if aws {
                    "&X-Amz-Credential=duplicate"
                } else {
                    "&%58-Goog-Credential=duplicate"
                });
            }
            error(
                &server.signed("GET", &duplicate, b""),
                400,
                "InvalidAuthentication",
            );
            let mut mixed = Signed {
                path: good.path.clone(),
                headers: good.headers.clone(),
            };
            if header {
                mixed.path.push_str("?X-Goog-Signature=00");
            } else {
                mixed.headers.push_str("Authorization: Bearer admin\r\n");
            }
            error(
                &server.signed("GET", &mixed, b""),
                400,
                "InvalidAuthentication",
            );
        }
    }
    assert_eq!(server.request("GET", path, ADMIN, b"").body, b"original");
}

#[test]
fn signed_empty_body_operations_reject_nonempty_hashes_and_actual_payloads() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    let path = "/test-bucket/object";
    assert_eq!(
        server
            .signed(
                "PUT",
                &sign(&key, "PUT", path, false, true, &[], SystemTime::now()),
                b"preserved"
            )
            .status,
        200
    );
    let hash = hex(digest::digest(&digest::SHA256, b"unexpected").as_ref());
    for (method, target) in [
        ("GET", path),
        ("HEAD", path),
        ("DELETE", path),
        ("GET", "/test-bucket/"),
        ("HEAD", "/test-bucket"),
        ("DELETE", "/test-bucket"),
        ("GET", "/"),
    ] {
        let mut extras = vec![("x-goog-content-sha256", hash.as_str())];
        if target == "/" {
            extras.push(("x-goog-project-id", "local"));
        }
        let signed = sign(
            &key,
            method,
            target,
            false,
            true,
            &extras,
            SystemTime::now(),
        );
        let reply = server.signed(method, &signed, b"");
        assert_eq!(reply.status, 400);
        if method != "HEAD" {
            error(&reply, 400, "BadDigest");
        }
        extras.retain(|(name, _)| *name != "x-goog-content-sha256");
        let signed = sign(&key, method, target, true, true, &extras, SystemTime::now());
        let reply = server.signed(method, &signed, b"unexpected");
        assert_eq!(
            reply.status,
            400,
            "{method} {target}: {}",
            String::from_utf8_lossy(&reply.body)
        );
    }
    assert_eq!(server.request("GET", path, ADMIN, b"").body, b"preserved");
}

#[test]
fn hmac_acl_reader_ubla_pap_and_public_invalid_signature_preserve_authorization() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let admin = server.key("signer@example.invalid");
    let path = "/test-bucket/object";
    assert_eq!(
        server
            .signed(
                "PUT",
                &sign(&admin, "PUT", path, false, true, &[], SystemTime::now()),
                b"protected"
            )
            .status,
        200
    );
    let scoped = server.key("scoped@example.invalid");
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/object/acl",
            ADMIN,
            br#"{"entity":"user-scoped@example.invalid","role":"READER"}"#,
        )
        .json();
    let read = sign(&scoped, "GET", path, true, false, &[], SystemTime::now());
    assert_eq!(server.signed("GET", &read, b"").body, b"protected");
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/o/object/acl",
            ADMIN,
            br#"{"entity":"allUsers","role":"READER"}"#,
        )
        .json();
    assert_eq!(server.request("GET", path, "", b"").status, 200);
    let bad = Signed {
        path: read.path.replace("X-Amz-Signature=", "X-Amz-Signature=00"),
        headers: read.headers.clone(),
    };
    error(
        &server.signed("GET", &bad, b""),
        403,
        "SignatureDoesNotMatch",
    );
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            ADMIN,
            br#"{"iamConfiguration":{"publicAccessPrevention":"enforced"}}"#,
        )
        .json();
    assert_ne!(server.request("GET", path, "", b"").status, 200);
    assert_eq!(server.signed("GET", &read, b"").body, b"protected");
    server
        .request(
            "PATCH",
            "/storage/v1/b/test-bucket",
            ADMIN,
            br#"{"iamConfiguration":{"uniformBucketLevelAccess":{"enabled":true}}}"#,
        )
        .json();
    error(&server.signed("GET", &read, b""), 403, "AccessDenied");
    assert_eq!(
        server
            .signed(
                "GET",
                &sign(&admin, "GET", path, false, true, &[], SystemTime::now()),
                b""
            )
            .body,
        b"protected"
    );
    let policy = json!({"bindings":[{"role":"roles/storage.objectViewer","members":["serviceAccount:scoped@example.invalid"]}]});
    server
        .request(
            "PUT",
            "/storage/v1/b/test-bucket/iam",
            ADMIN,
            policy.to_string().as_bytes(),
        )
        .json();
    for (aws, header) in [(false, false), (true, true)] {
        assert_eq!(
            server
                .signed(
                    "GET",
                    &sign(&scoped, "GET", path, aws, header, &[], SystemTime::now()),
                    b""
                )
                .body,
            b"protected"
        );
    }
    server
        .request(
            "PUT",
            "/storage/v1/b/test-bucket/iam",
            ADMIN,
            br#"{"bindings":[]}"#,
        )
        .json();
    error(&server.signed("GET", &read, b""), 403, "AccessDenied");
}

#[test]
fn empty_chunked_frames_and_zero_lengths_match_payload_while_host_and_body_tampering_fail() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    let path = "/test-bucket/object";
    assert_eq!(
        server
            .signed(
                "PUT",
                &sign(&key, "PUT", path, false, true, &[], SystemTime::now()),
                b"kept"
            )
            .status,
        200
    );
    let hash = hex(digest::digest(&digest::SHA256, b"").as_ref());
    for method in ["GET", "HEAD", "DELETE"] {
        let signed = sign(
            &key,
            method,
            path,
            true,
            true,
            &[("x-amz-content-sha256", &hash)],
            SystemTime::now(),
        );
        let raw = |host: &str, framing: &str, body: &str| {
            format!(
                "{method} {} HTTP/1.1\r\nHost: {host}\r\n{}Connection: close\r\n{framing}\r\n{body}",
                signed.path, signed.headers
            )
        };
        let bad = server.raw(&raw("changed", "Content-Length: 0\r\n", ""), method);
        assert_eq!(bad.status, 403);
        let bad = server.raw(
            &raw(
                "localhost",
                "Transfer-Encoding: chunked\r\n",
                "1\r\nx\r\n0\r\n\r\n",
            ),
            method,
        );
        assert_eq!(
            bad.status,
            400,
            "{method}: {}",
            String::from_utf8_lossy(&bad.body)
        );
        assert_eq!(server.request("GET", path, ADMIN, b"").body, b"kept");
        if method != "DELETE" {
            assert_eq!(
                server
                    .raw(&raw("localhost", "Content-Length: 00\r\n", ""), method)
                    .status,
                200
            );
        }
        assert_eq!(
            server
                .raw(
                    &raw("localhost", "Transfer-Encoding: chunked\r\n", "0\r\n\r\n"),
                    method
                )
                .status,
            if method == "DELETE" { 204 } else { 200 }
        );
    }
}

#[test]
fn hmac_virtual_hosts_raw_paths_and_repeated_signed_header_wire_order_are_bound() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("test-bucket");
    let key = server.key("signer@example.invalid");
    let host = "test-bucket.storage.example.invalid";
    let path = "/literal%252Fname";
    for method in ["PUT", "GET", "HEAD", "DELETE"] {
        let signed = sign(
            &key,
            method,
            path,
            true,
            true,
            &[("host", host)],
            SystemTime::now(),
        );
        let body = if method == "PUT" { "virtual bytes" } else { "" };
        let wire = format!(
            "{method} {} HTTP/1.1\r\nHost: {host}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            signed.path,
            signed.headers,
            body.len()
        );
        let reply = server.raw(&wire, method);
        assert_eq!(
            reply.status,
            if method == "DELETE" { 204 } else { 200 },
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
        if method == "GET" {
            assert_eq!(reply.body, b"virtual bytes");
        }
    }
    let path = "/test-bucket/order";
    assert_eq!(
        server
            .signed(
                "PUT",
                &sign(&key, "PUT", path, false, false, &[], SystemTime::now()),
                b"headers"
            )
            .status,
        200
    );
    let mut signed = sign(
        &key,
        "GET",
        path,
        false,
        true,
        &[("x-reviewer", " jane  smith "), ("x-reviewer", "john")],
        SystemTime::now(),
    );
    assert_eq!(server.signed("GET", &signed, b"").body, b"headers");
    signed.headers = signed.headers.replace(
        "x-reviewer:  jane  smith \r\nx-reviewer: john",
        "x-reviewer: john\r\nx-reviewer: jane smith",
    );
    error(
        &server.signed("GET", &signed, b""),
        403,
        "SignatureDoesNotMatch",
    );
}
