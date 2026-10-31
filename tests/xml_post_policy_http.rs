mod object_effects_support;

use gcs_mimic::auth::signature::{encode_query_component, sign_policy, timestamp};
use object_effects_support::{
    Reply,
    common::{self, TestDir},
};
use ring::hmac;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    process::{Child, ChildStdout, Stdio},
    time::{Duration, SystemTime},
};

const PRIVATE: &[u8] = include_bytes!("../fixtures/signing/test-only-private.pem");
const PUBLIC: &str = include_str!("../fixtures/signing/test-only-public.pem");
const ADMIN: &str = "Authorization: Bearer admin\r\n";
const KEYS: &str = "/storage/v1/projects/local/hmacKeys";
const BOUNDARY: &str = "policy-boundary";

struct Server {
    child: Child,
    stdout: BufReader<ChildStdout>,
    address: String,
}
struct Key {
    id: String,
    secret: String,
}

impl Server {
    fn start(dir: &TestDir, backend: &str) -> Self {
        let path = dir.0.join("identities.json");
        std::fs::write(&path,json!({"identities":[
            {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
            {"id":"signer@example.invalid","tokens":[],"public_keys":[PUBLIC],"project_roles":{"local":["roles/storage.admin"]}},
            {"id":"creator@example.invalid","tokens":[],"public_keys":[PUBLIC],"project_roles":{"local":["roles/storage.objectCreator"]}},
            {"id":"reader@example.invalid","tokens":[],"public_keys":[PUBLIC],"project_roles":{"local":["roles/storage.objectViewer"]}},
            {"id":"keys@example.invalid","tokens":["keys"],"project_roles":{"local":["roles/storage.hmacKeyAdmin"]}},
            {"id":"scoped@example.invalid","tokens":[],"public_keys":[PUBLIC]},
            {"id":"stranger@example.invalid","tokens":[],"public_keys":[PUBLIC],"project_roles":{"other":["roles/storage.admin"]}}
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
            .args(["--api-base-url", "http://127.0.0.1:4443"])
            .args(["--auth-mode", "strict", "--identity-file"])
            .arg(path)
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
    fn post(&self, bucket: &str, fields: &[(String, String)], bytes: &[u8]) -> Reply {
        self.post_with_headers(bucket, fields, bytes, "")
    }
    fn post_with_headers(
        &self,
        bucket: &str,
        fields: &[(String, String)],
        bytes: &[u8],
        extra_headers: &str,
    ) -> Reply {
        let mut body = Vec::new();
        for (name, value) in fields {
            body.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
        }
        body.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"file.bin\"\r\n\r\n").as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
        // The shared raw client supplies JSON Content-Type, so send an explicit wire request.
        use std::{
            io::{Read, Write},
            net::TcpStream,
        };
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        write!(stream,"POST /{bucket} HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/form-data; boundary={BOUNDARY}\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n",body.len()).unwrap();
        stream.write_all(&body).unwrap();
        let mut reader = BufReader::new(stream);
        let mut raw = common::read_http_response(&mut reader, false).unwrap();
        reader.read_to_end(&mut raw).unwrap();
        let split = raw.windows(4).position(|part| part == b"\r\n\r\n").unwrap();
        let head = std::str::from_utf8(&raw[..split]).unwrap();
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
            body: raw[split + 4..].to_vec(),
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

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |value, (index, byte)| {
            value | u32::from(*byte) << (16 - index * 8)
        });
        for index in 0..4 {
            result.push(if index <= chunk.len() {
                ALPHABET[(value >> (18 - index * 6) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    result
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, Copy)]
enum Signer<'a> {
    Rsa(&'a str),
    Hmac(&'a Key),
}

fn fields(
    signer: Signer<'_>,
    bucket: &str,
    name: &str,
    extras: &[(&str, &str)],
    extra_conditions: &[Value],
) -> Vec<(String, String)> {
    let date = timestamp(SystemTime::now()).unwrap();
    let scope = format!("{}/auto/storage/goog4_request", &date[..8]);
    let (id, algorithm) = match signer {
        Signer::Rsa(id) => (id, "GOOG4-RSA-SHA256"),
        Signer::Hmac(key) => (key.id.as_str(), "GOOG4-HMAC-SHA256"),
    };
    let mut fields: Vec<(String, String)> = vec![
        ("key".into(), name.into()),
        ("x-goog-algorithm".into(), algorithm.into()),
        ("x-goog-credential".into(), format!("{id}/{scope}")),
        ("x-goog-date".into(), date),
    ];
    fields.extend(
        extras
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into())),
    );
    let mut conditions = fields
        .iter()
        .map(|(name, value)| json!({name:value}))
        .collect::<Vec<_>>();
    conditions.push(json!({"bucket":bucket}));
    conditions.extend_from_slice(extra_conditions);
    let policy=base64(json!({"expiration":timestamp(SystemTime::now()+Duration::from_secs(3600)).unwrap(),"conditions":conditions}).to_string().as_bytes());
    let signature = match signer {
        Signer::Rsa(_) => sign_policy(&policy, PRIVATE).unwrap(),
        Signer::Hmac(key) => {
            let mut derived =
                hmac::Key::new(hmac::HMAC_SHA256, format!("GOOG4{}", key.secret).as_bytes());
            for part in scope.split('/') {
                derived = hmac::Key::new(
                    hmac::HMAC_SHA256,
                    hmac::sign(&derived, part.as_bytes()).as_ref(),
                );
            }
            hex(hmac::sign(&derived, policy.as_bytes()).as_ref())
        }
    };
    fields.push(("policy".into(), policy));
    fields.push(("x-goog-signature".into(), signature));
    fields
}

fn v2_fields(
    signer: Signer<'_>,
    bucket: Option<&str>,
    name: &str,
    expires: SystemTime,
) -> Vec<(String, String)> {
    v2_fields_with(signer, bucket, name, expires, &[], &[])
}

fn v2_fields_with(
    signer: Signer<'_>,
    bucket: Option<&str>,
    name: &str,
    expires: SystemTime,
    extras: &[(&str, &str)],
    extra_conditions: &[Value],
) -> Vec<(String, String)> {
    let mut fields = vec![("key".to_owned(), name.to_owned())];
    fields.extend(
        extras
            .iter()
            .map(|(name, value)| ((*name).into(), (*value).into())),
    );
    let mut conditions = fields
        .iter()
        .map(|(name, value)| json!({name:value}))
        .collect::<Vec<_>>();
    if let Some(bucket) = bucket {
        conditions.push(json!({"bucket":bucket}));
    }
    conditions.extend_from_slice(extra_conditions);
    let policy = base64(
        json!({"expiration":timestamp(expires).unwrap(),"conditions":conditions})
            .to_string()
            .as_bytes(),
    );
    let (id, signature) = match signer {
        Signer::Rsa(id) => {
            let hex = sign_policy(&policy, PRIVATE).unwrap();
            (
                id,
                (0..hex.len())
                    .step_by(2)
                    .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap())
                    .collect::<Vec<_>>(),
            )
        }
        Signer::Hmac(key) => (
            key.id.as_str(),
            hmac::sign(
                &hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, key.secret.as_bytes()),
                policy.as_bytes(),
            )
            .as_ref()
            .to_vec(),
        ),
    };
    fields.extend([
        ("GoogleAccessId".into(), id.into()),
        ("policy".into(), policy),
        ("signature".into(), base64(&signature)),
    ]);
    fields
}

#[derive(Clone, Copy)]
enum FormVersion {
    V2,
    V4,
}

fn variant_fields(
    version: FormVersion,
    signer: Signer<'_>,
    name: &str,
    extras: &[(&str, &str)],
    conditions: &[Value],
) -> Vec<(String, String)> {
    match version {
        FormVersion::V4 => fields(signer, "policy-bucket", name, extras, conditions),
        FormVersion::V2 => v2_fields_with(
            signer,
            Some("policy-bucket"),
            name,
            SystemTime::now() + Duration::from_secs(3600),
            extras,
            conditions,
        ),
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
fn v4_rsa_and_hmac_policy_upload_exact_binary_on_disk_and_ram() {
    for backend in ["disk", "memory"] {
        let dir = TestDir::new();
        let server = Server::start(&dir, backend);
        server.bucket("policy-bucket");
        let key = server.key("signer@example.invalid");
        for signer in [Signer::Rsa("signer@example.invalid"), Signer::Hmac(&key)] {
            let form = fields(
                signer,
                "policy-bucket",
                "object",
                &[
                    ("Content-Type", "application/octet-stream"),
                    ("x-goog-meta-version", "policy"),
                ],
                &[],
            );
            let reply = server.post("policy-bucket", &form, b"\0\xff\x80policy\r\nbytes");
            assert_eq!(
                reply.status,
                204,
                "{}",
                String::from_utf8_lossy(&reply.body)
            );
            assert_eq!(
                server
                    .request("GET", "/policy-bucket/object", ADMIN, b"")
                    .body,
                b"\0\xff\x80policy\r\nbytes"
            );
            let metadata = server
                .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
                .json();
            assert_eq!(metadata["metadata"]["version"], "policy");
        }
    }
}

#[test]
fn v4_hmac_policy_keys_revoke_restart_and_never_borrow_key_admin_or_project_permissions() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, "disk");
    server.bucket("policy-bucket");
    let key = server.key("signer@example.invalid");
    let form = fields(Signer::Hmac(&key), "policy-bucket", "object", &[], &[]);
    assert_eq!(server.post("policy-bucket", &form, b"original").status, 204);
    let before = server
        .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
        .json();
    let list = server
        .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
        .json();
    for email in [
        "reader@example.invalid",
        "keys@example.invalid",
        "scoped@example.invalid",
        "stranger@example.invalid",
        "unknown@example.invalid",
    ] {
        let other = server.key(email);
        let form = fields(Signer::Hmac(&other), "policy-bucket", "object", &[], &[]);
        error(
            &server.post("policy-bucket", &form, b"denied"),
            403,
            if email.starts_with("unknown") {
                "InvalidAccessKeyId"
            } else {
                "AccessDenied"
            },
        );
    }
    let path = format!("{KEYS}/{}", key.id);
    let keys = "Authorization: Bearer keys\r\n";
    server
        .request("PUT", &path, keys, br#"{"state":"INACTIVE"}"#)
        .json();
    error(
        &server.post("policy-bucket", &form, b"denied"),
        403,
        "InvalidAccessKeyId",
    );
    server.stop();
    server = Server::start(&dir, "disk");
    error(
        &server.post("policy-bucket", &form, b"denied"),
        403,
        "InvalidAccessKeyId",
    );
    server
        .request("PUT", &path, keys, br#"{"state":"ACTIVE"}"#)
        .json();
    assert_eq!(server.post("policy-bucket", &form, b"original").status, 204);
    let current = server
        .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
        .json();
    assert_ne!(current["generation"], before["generation"]);
    server
        .request("PUT", &path, keys, br#"{"state":"INACTIVE"}"#)
        .json();
    assert_eq!(server.request("DELETE", &path, keys, b"").status, 204);
    error(
        &server.post("policy-bucket", &form, b"denied"),
        403,
        "InvalidAccessKeyId",
    );
    assert_eq!(
        server
            .request("GET", "/policy-bucket/object", ADMIN, b"")
            .body,
        b"original"
    );
    let after = server
        .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
        .json();
    assert_eq!(after, current);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
            .json()["items"]
            .as_array()
            .unwrap()
            .len(),
        list["items"].as_array().unwrap().len()
    );
}

#[test]
fn v4_hmac_policy_rejections_preserve_the_complete_old_resource_even_when_public() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("policy-bucket");
    let key = server.key("signer@example.invalid");
    let valid = fields(
        Signer::Hmac(&key),
        "policy-bucket",
        "object",
        &[("x-goog-meta-version", "old")],
        &[],
    );
    assert_eq!(
        server.post("policy-bucket", &valid, b"original").status,
        204
    );
    server
        .request(
            "PUT",
            "/storage/v1/b/policy-bucket/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectAdmin","members":["allUsers"]}]}"#,
        )
        .json();
    let before = server
        .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
        .json();
    let list = server
        .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
        .json();
    let replace = |name: &str, value: &str| {
        valid
            .iter()
            .map(|(key, old)| {
                (
                    key.clone(),
                    if key == name {
                        value.into()
                    } else {
                        old.clone()
                    },
                )
            })
            .collect::<Vec<_>>()
    };
    let changed_policy = format!(
        "{}A",
        valid.iter().find(|(name, _)| name == "policy").unwrap().1
    );
    let duplicate = [
        valid.clone(),
        vec![("X-Goog-Signature".into(), "00".into())],
    ]
    .concat();
    let uncovered = [
        valid.clone(),
        vec![("x-goog-meta-extra".into(), "unconditioned".into())],
    ]
    .concat();
    for (form, status, code) in [
        (
            replace("x-goog-signature", "00"),
            403,
            "SignatureDoesNotMatch",
        ),
        (
            replace("policy", &changed_policy),
            403,
            "SignatureDoesNotMatch",
        ),
        (replace("key", "different"), 400, "InvalidPolicyDocument"),
        (
            replace("x-goog-algorithm", "GOOG4-HMAC-SHA512"),
            400,
            "InvalidArgument",
        ),
        (
            replace("x-goog-date", "20000101T000000Z"),
            400,
            "InvalidArgument",
        ),
        (duplicate, 400, "InvalidArgument"),
        (uncovered, 400, "InvalidPolicyDocument"),
    ] {
        error(
            &server.post("policy-bucket", &form, b"changed"),
            status,
            code,
        );
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
                .json(),
            before
        );
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
                .json(),
            list
        );
        assert_eq!(
            server
                .request("GET", "/policy-bucket/object", ADMIN, b"")
                .body,
            b"original"
        );
    }
}

#[test]
fn v2_rsa_policy_uses_base64_signature_and_its_own_bucket_coverage_and_expiration() {
    for backend in ["disk", "memory"] {
        let dir = TestDir::new();
        let server = Server::start(&dir, backend);
        server.bucket("policy-bucket");
        server.bucket("second-bucket");
        let expires = SystemTime::now() + Duration::from_secs(8 * 86400);
        let unbound = v2_fields(
            Signer::Rsa("signer@example.invalid"),
            None,
            "object",
            expires,
        );
        for bucket in ["policy-bucket", "second-bucket"] {
            assert_eq!(
                server.post(bucket, &unbound, b"\0\xffv2\r\nbytes").status,
                204
            );
            assert_eq!(
                server
                    .request("GET", &format!("/{bucket}/object"), ADMIN, b"")
                    .body,
                b"\0\xffv2\r\nbytes"
            );
        }
        let bound = v2_fields(
            Signer::Rsa("signer@example.invalid"),
            Some("policy-bucket"),
            "object",
            expires,
        );
        error(
            &server.post("second-bucket", &bound, b"changed"),
            400,
            "InvalidPolicyDocument",
        );
        let wrong = [
            bound.clone(),
            vec![("x-goog-algorithm".into(), "GOOG4-RSA-SHA256".into())],
        ]
        .concat();
        error(
            &server.post("policy-bucket", &wrong, b"changed"),
            400,
            "InvalidArgument",
        );
        let before = server
            .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
            .json();
        for name in ["GoogleAccessId", "signature", "policy"] {
            let partial = bound
                .iter()
                .filter(|(field, _)| field != name)
                .cloned()
                .collect::<Vec<_>>();
            error(
                &server.post("policy-bucket", &partial, b"changed"),
                400,
                "InvalidArgument",
            );
        }
        let mut invalid = bound.clone();
        invalid
            .iter_mut()
            .find(|(name, _)| name == "signature")
            .unwrap()
            .1 = base64(b"invalid signature");
        error(
            &server.post("policy-bucket", &invalid, b"changed"),
            403,
            "SignatureDoesNotMatch",
        );
        let expired = v2_fields(
            Signer::Rsa("signer@example.invalid"),
            None,
            "object",
            SystemTime::now() - Duration::from_secs(1),
        );
        error(
            &server.post("policy-bucket", &expired, b"changed"),
            400,
            "InvalidPolicyDocument",
        );
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
                .json(),
            before
        );
        assert_eq!(
            server
                .request("GET", "/policy-bucket/object", ADMIN, b"")
                .body,
            b"\0\xffv2\r\nbytes"
        );
    }
}

#[test]
fn v2_hmac_policy_uses_active_literal_secret_sha1_and_preserves_rejected_overwrites() {
    for backend in ["disk", "memory"] {
        let dir = TestDir::new();
        let mut server = Server::start(&dir, backend);
        server.bucket("policy-bucket");
        let key = server.key("signer@example.invalid");
        let expires = SystemTime::now() + Duration::from_secs(8 * 86400);
        let valid = v2_fields(Signer::Hmac(&key), None, "object", expires);
        assert_eq!(
            server
                .post("policy-bucket", &valid, b"\0\xffv2 hmac bytes")
                .status,
            204
        );
        let before = server
            .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
            .json();
        let list = server
            .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
            .json();
        let mut tampered = valid.clone();
        let signature = &mut tampered
            .iter_mut()
            .find(|(name, _)| name == "signature")
            .unwrap()
            .1;
        signature.replace_range(..1, if signature.starts_with('A') { "B" } else { "A" });
        error(
            &server.post("policy-bucket", &tampered, b"changed"),
            403,
            "SignatureDoesNotMatch",
        );
        for email in [
            "reader@example.invalid",
            "keys@example.invalid",
            "stranger@example.invalid",
            "unknown@example.invalid",
        ] {
            let other = server.key(email);
            let form = v2_fields(
                Signer::Hmac(&other),
                Some("policy-bucket"),
                "object",
                expires,
            );
            error(
                &server.post("policy-bucket", &form, b"changed"),
                403,
                if email.starts_with("unknown") {
                    "InvalidAccessKeyId"
                } else {
                    "AccessDenied"
                },
            );
        }
        let path = format!("{KEYS}/{}", key.id);
        let admin = "Authorization: Bearer keys\r\n";
        server
            .request("PUT", &path, admin, br#"{"state":"INACTIVE"}"#)
            .json();
        error(
            &server.post("policy-bucket", &valid, b"changed"),
            403,
            "InvalidAccessKeyId",
        );
        if backend == "disk" {
            server.stop();
            server = Server::start(&dir, backend);
            error(
                &server.post("policy-bucket", &valid, b"changed"),
                403,
                "InvalidAccessKeyId",
            );
        }
        server
            .request("PUT", &path, admin, br#"{"state":"ACTIVE"}"#)
            .json();
        server
            .request("PUT", &path, admin, br#"{"state":"INACTIVE"}"#)
            .json();
        assert_eq!(server.request("DELETE", &path, admin, b"").status, 204);
        error(
            &server.post("policy-bucket", &valid, b"changed"),
            403,
            "InvalidAccessKeyId",
        );
        assert_eq!(
            server
                .request("GET", "/policy-bucket/object", ADMIN, b"")
                .body,
            b"\0\xffv2 hmac bytes"
        );
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
                .json(),
            before
        );
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
                .json(),
            list
        );
    }
}

#[test]
fn every_policy_variant_checks_inclusive_ranges_and_creator_only_overwrites() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("policy-bucket");
    let admin = server.key("signer@example.invalid");
    let creator = server.key("creator@example.invalid");
    for (index, (version, signer, creator)) in [
        (
            FormVersion::V4,
            Signer::Rsa("signer@example.invalid"),
            Signer::Rsa("creator@example.invalid"),
        ),
        (
            FormVersion::V4,
            Signer::Hmac(&admin),
            Signer::Hmac(&creator),
        ),
        (
            FormVersion::V2,
            Signer::Rsa("signer@example.invalid"),
            Signer::Rsa("creator@example.invalid"),
        ),
        (
            FormVersion::V2,
            Signer::Hmac(&admin),
            Signer::Hmac(&creator),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("creator-{index}");
        let form = variant_fields(
            version,
            creator,
            &name,
            &[("x-goog-meta-version", "old")],
            &[],
        );
        assert_eq!(server.post("policy-bucket", &form, b"original").status, 204);
        let before = server
            .request(
                "GET",
                &format!("/storage/v1/b/policy-bucket/o/{name}"),
                ADMIN,
                b"",
            )
            .json();
        let list = server
            .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
            .json();
        error(
            &server.post("policy-bucket", &form, b"changed"),
            403,
            "AccessDenied",
        );
        assert_eq!(
            server
                .request(
                    "GET",
                    &format!("/storage/v1/b/policy-bucket/o/{name}"),
                    ADMIN,
                    b""
                )
                .json(),
            before
        );
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
                .json(),
            list
        );
        assert_eq!(
            server
                .request("GET", &format!("/policy-bucket/{name}"), ADMIN, b"")
                .body,
            b"original"
        );
        let name = format!("range-{index}");
        let form = variant_fields(
            version,
            signer,
            &name,
            &[("x-goog-meta-version", "bounded")],
            &[json!(["content-length-range", 3, 5])],
        );
        for bytes in [b"abc".as_slice(), b"abcde"] {
            assert_eq!(server.post("policy-bucket", &form, bytes).status, 204);
            assert_eq!(
                server
                    .request("GET", &format!("/policy-bucket/{name}"), ADMIN, b"")
                    .body,
                bytes
            );
        }
        let before = server
            .request(
                "GET",
                &format!("/storage/v1/b/policy-bucket/o/{name}"),
                ADMIN,
                b"",
            )
            .json();
        let list = server
            .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
            .json();
        for (bytes, code) in [
            (b"ab".as_slice(), "EntityTooSmall"),
            (b"abcdef".as_slice(), "EntityTooLarge"),
        ] {
            error(&server.post("policy-bucket", &form, bytes), 400, code);
            assert_eq!(
                server
                    .request(
                        "GET",
                        &format!("/storage/v1/b/policy-bucket/o/{name}"),
                        ADMIN,
                        b""
                    )
                    .json(),
                before
            );
            assert_eq!(
                server
                    .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
                    .json(),
                list
            );
            assert_eq!(
                server
                    .request("GET", &format!("/policy-bucket/{name}"), ADMIN, b"")
                    .body,
                b"abcde"
            );
        }
    }
}

#[test]
fn every_policy_variant_preserves_created_xml_and_redirect_responses() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("policy-bucket");
    let key = server.key("signer@example.invalid");
    for (version, signer) in [
        (FormVersion::V4, Signer::Rsa("signer@example.invalid")),
        (FormVersion::V4, Signer::Hmac(&key)),
        (FormVersion::V2, Signer::Rsa("signer@example.invalid")),
        (FormVersion::V2, Signer::Hmac(&key)),
    ] {
        let form = variant_fields(
            version,
            signer,
            "response",
            &[("success_action_status", "201")],
            &[],
        );
        let reply = server.post("policy-bucket", &form, b"created");
        assert_eq!(reply.status, 201);
        let xml = String::from_utf8(reply.body).unwrap();
        assert!(xml.contains("<Bucket>policy-bucket</Bucket><Key>response</Key>"));
        assert!(xml.contains("<ETag>"));
        let form = variant_fields(
            version,
            signer,
            "response",
            &[(
                "success_action_redirect",
                "http://localhost/complete?done=yes",
            )],
            &[],
        );
        let reply = server.post("policy-bucket", &form, b"redirect");
        assert_eq!(reply.status, 303);
        assert!(reply.body.is_empty());
        assert_eq!(
            reply.headers["location"],
            "http://localhost/complete?done=yes"
        );
        assert_eq!(
            server
                .request("GET", "/policy-bucket/response", ADMIN, b"")
                .body,
            b"redirect"
        );
    }
}

#[test]
fn every_policy_variant_rechecks_scoped_iam_under_pap_and_ubla() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("policy-bucket");
    let key = server.key("scoped@example.invalid");
    server.request("PUT","/storage/v1/b/policy-bucket/iam",ADMIN,br#"{"bindings":[{"role":"roles/storage.objectAdmin","members":["serviceAccount:scoped@example.invalid"]}]}"#).json();
    server.request("PATCH","/storage/v1/b/policy-bucket",ADMIN,br#"{"iamConfiguration":{"publicAccessPrevention":"enforced","uniformBucketLevelAccess":{"enabled":true}}}"#).json();
    let variants = [
        (FormVersion::V4, Signer::Rsa("scoped@example.invalid")),
        (FormVersion::V4, Signer::Hmac(&key)),
        (FormVersion::V2, Signer::Rsa("scoped@example.invalid")),
        (FormVersion::V2, Signer::Hmac(&key)),
    ];
    for (index, (version, signer)) in variants.into_iter().enumerate() {
        let form = variant_fields(version, signer, &format!("scoped-{index}"), &[], &[]);
        assert_eq!(server.post("policy-bucket", &form, b"scoped").status, 204);
    }
    let before = server
        .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
        .json();
    server
        .request(
            "PUT",
            "/storage/v1/b/policy-bucket/iam",
            ADMIN,
            br#"{"bindings":[]}"#,
        )
        .json();
    for (index, (version, signer)) in variants.into_iter().enumerate() {
        let form = variant_fields(version, signer, &format!("scoped-{index}"), &[], &[]);
        error(
            &server.post("policy-bucket", &form, b"denied"),
            403,
            "AccessDenied",
        );
        assert_eq!(
            server
                .request("GET", &format!("/policy-bucket/scoped-{index}"), ADMIN, b"")
                .body,
            b"scoped"
        );
    }
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
            .json(),
        before
    );
}

#[test]
fn policy_credentials_never_mix_with_headers_or_partial_versions_and_anonymous_extension_is_explicit()
 {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("policy-bucket");
    let key = server.key("signer@example.invalid");
    for (version, signer) in [
        (FormVersion::V4, Signer::Rsa("signer@example.invalid")),
        (FormVersion::V4, Signer::Hmac(&key)),
        (FormVersion::V2, Signer::Rsa("signer@example.invalid")),
        (FormVersion::V2, Signer::Hmac(&key)),
    ] {
        let form = variant_fields(version, signer, "mixed", &[], &[]);
        error(
            &server.post_with_headers("policy-bucket", &form, b"denied", ADMIN),
            400,
            "InvalidArgument",
        );
    }
    let plain = vec![("key".into(), "anonymous".into())];
    error(
        &server.post("policy-bucket", &plain, b"private"),
        401,
        "AuthenticationRequired",
    );
    assert_eq!(
        server
            .post_with_headers("policy-bucket", &plain, b"local bearer extension", ADMIN)
            .status,
        204
    );
    assert_eq!(
        server
            .request("GET", "/policy-bucket/anonymous", ADMIN, b"")
            .body,
        b"local bearer extension"
    );
    server
        .request(
            "PUT",
            "/storage/v1/b/policy-bucket/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectAdmin","members":["allUsers"]}]}"#,
        )
        .json();
    assert_eq!(
        server
            .post("policy-bucket", &plain, b"public anonymous")
            .status,
        204
    );
    let invalid = vec![
        ("key".into(), "anonymous".into()),
        ("GoogleAccessId".into(), key.id),
    ];
    error(
        &server.post("policy-bucket", &invalid, b"partial"),
        400,
        "InvalidArgument",
    );
    assert_eq!(
        server
            .request("GET", "/policy-bucket/anonymous", ADMIN, b"")
            .body,
        b"public anonymous"
    );
}
