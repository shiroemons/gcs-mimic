use crate::common;
use crate::common::process::declare_server;
use common::TestDir;
use common::http::Body;

use common::effects::Reply;
use common::process::{http_command, write_request};
use gcs_mimic::auth::signature::{encode_query_component, sign_policy, timestamp};
use ring::hmac;
use serde_json::{Value, json};
use std::{
    io::BufReader,
    time::{Duration, SystemTime},
};

const PRIVATE: &[u8] = include_bytes!("../../fixtures/signing/test-only-private.pem");
const PUBLIC: &str = include_str!("../../fixtures/signing/test-only-public.pem");
const ADMIN: &str = "Authorization: Bearer admin\r\n";
const KEYS: &str = "/storage/v1/projects/local/hmacKeys";
const BOUNDARY: &str = "policy-boundary";

struct Key {
    id: String,
    secret: String,
}

impl Server {
    fn start(dir: &TestDir, backend: &str) -> Self {
        let path = common::process::write_identities(
            &dir.0,
            &json!({"identities":[
                {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
                {"id":"signer@example.invalid","tokens":[],"public_keys":[PUBLIC],"project_roles":{"local":["roles/storage.admin"]}},
                {"id":"creator@example.invalid","tokens":[],"public_keys":[PUBLIC],"project_roles":{"local":["roles/storage.objectCreator"]}},
                {"id":"reader@example.invalid","tokens":[],"public_keys":[PUBLIC],"project_roles":{"local":["roles/storage.objectViewer"]}},
                {"id":"keys@example.invalid","tokens":["keys"],"project_roles":{"local":["roles/storage.hmacKeyAdmin"]}},
                {"id":"scoped@example.invalid","tokens":[],"public_keys":[PUBLIC]},
                {"id":"stranger@example.invalid","tokens":[],"public_keys":[PUBLIC],"project_roles":{"other":["roles/storage.admin"]}}
            ]}),
        );
        Self::launch(
            http_command(&dir.0.join("data"))
                .args(["--backend", backend])
                .args(["--api-base-url", "http://127.0.0.1:4443"])
                .args(["--auth-mode", "strict", "--identity-file"])
                .arg(path),
        )
    }
    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        common::effects::request(&self.address, method, path, headers, body)
    }
    fn bucket(&self, name: &str) {
        self.request(
            "POST",
            "/storage/v1/b?project=local",
            ADMIN,
            json!({"name":name}).to_string().as_bytes(),
        )
        .json_ok();
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
            .json_ok();
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
        use std::{io::Read, net::TcpStream};
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        write_request(
            &mut stream,
            "POST",
            &format!("/{bucket}"),
            "localhost",
            &format!(
                "Content-Type: multipart/form-data; boundary={BOUNDARY}\r\n{extra_headers}Connection: close\r\n"
            ),
            &body,
        );
        let mut reader = BufReader::new(stream);
        let mut raw = common::read_http_response(&mut reader, Body::Expected).unwrap();
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
        self.0.stop();
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
    fields_signed_at(
        signer,
        bucket,
        name,
        extras,
        extra_conditions,
        SystemTime::now(),
    )
}

/// V4 form fields whose `x-goog-date` is `signed_at`; the policy still expires an hour from now.
fn fields_signed_at(
    signer: Signer<'_>,
    bucket: &str,
    name: &str,
    extras: &[(&str, &str)],
    extra_conditions: &[Value],
    signed_at: SystemTime,
) -> Vec<(String, String)> {
    let date = timestamp(signed_at).unwrap();
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
use crate::common::errors::xml_error as error;

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
            reply.expect(204);
            assert_eq!(
                server
                    .request("GET", "/policy-bucket/object", ADMIN, b"")
                    .body,
                b"\0\xff\x80policy\r\nbytes"
            );
            let metadata = server
                .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
                .json_ok();
            assert_eq!(metadata["metadata"]["version"], "policy");
        }
    }
}

/// The form has no limit on its number of fields, so custom metadata may arrive in many small
/// ones; the documented 8 KiB total is what still bounds it.
#[test]
fn a_policy_form_may_split_its_custom_metadata_over_many_small_fields() {
    let dir = TestDir::new();
    let server = Server::start(&dir, "disk");
    server.bucket("policy-bucket");
    let key = server.key("signer@example.invalid");
    let names: Vec<_> = (0..1000)
        .map(|index| format!("x-goog-meta-k{index:03}"))
        .collect();
    let mut extras: Vec<_> = names.iter().map(|name| (name.as_str(), "vvvv")).collect();
    let form = fields(Signer::Hmac(&key), "policy-bucket", "object", &extras, &[]);
    assert!(form.len() > 1000);
    server.post("policy-bucket", &form, b"data").expect(204);
    let stored = server
        .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
        .json_ok();
    assert_eq!(stored["metadata"].as_object().unwrap().len(), 1000);
    // Each field adds eight bytes, so this value passes the documented total by a single byte.
    let over = "v".repeat(8192 - 8000 - "big".len() + 1);
    extras.push(("x-goog-meta-big", over.as_str()));
    let form = fields(Signer::Hmac(&key), "policy-bucket", "over", &extras, &[]);
    let reply = server.post("policy-bucket", &form, b"data");
    reply.expect(400);
    assert!(String::from_utf8_lossy(&reply.body).contains("MetadataTooLarge"));
}

#[test]
fn v4_hmac_policy_keys_revoke_restart_and_never_borrow_key_admin_or_project_permissions() {
    let dir = TestDir::new();
    let mut server = Server::start(&dir, "disk");
    server.bucket("policy-bucket");
    let key = server.key("signer@example.invalid");
    let form = fields(Signer::Hmac(&key), "policy-bucket", "object", &[], &[]);
    server.post("policy-bucket", &form, b"original").expect(204);
    let before = server
        .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
        .json_ok();
    let list = server
        .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
        .json_ok();
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
        .json_ok();
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
        .json_ok();
    server.post("policy-bucket", &form, b"original").expect(204);
    let current = server
        .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
        .json_ok();
    assert_ne!(current["generation"], before["generation"]);
    server
        .request("PUT", &path, keys, br#"{"state":"INACTIVE"}"#)
        .json_ok();
    server.request("DELETE", &path, keys, b"").expect(204);
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
        .json_ok();
    assert_eq!(after, current);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
            .json_ok()["items"]
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
    server
        .post("policy-bucket", &valid, b"original")
        .expect(204);
    server
        .request(
            "PUT",
            "/storage/v1/b/policy-bucket/iam",
            ADMIN,
            br#"{"bindings":[{"role":"roles/storage.objectAdmin","members":["allUsers"]}]}"#,
        )
        .json_ok();
    let before = server
        .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
        .json_ok();
    let list = server
        .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
        .json_ok();
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
                .json_ok(),
            before
        );
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
                .json_ok(),
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
            server
                .post(bucket, &unbound, b"\0\xffv2\r\nbytes")
                .expect(204);
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
            .json_ok();
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
                .json_ok(),
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
        server
            .post("policy-bucket", &valid, b"\0\xffv2 hmac bytes")
            .expect(204);
        let before = server
            .request("GET", "/storage/v1/b/policy-bucket/o/object", ADMIN, b"")
            .json_ok();
        let list = server
            .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
            .json_ok();
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
            .json_ok();
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
            .json_ok();
        server
            .request("PUT", &path, admin, br#"{"state":"INACTIVE"}"#)
            .json_ok();
        server.request("DELETE", &path, admin, b"").expect(204);
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
                .json_ok(),
            before
        );
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
                .json_ok(),
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
        server.post("policy-bucket", &form, b"original").expect(204);
        let before = server
            .request(
                "GET",
                &format!("/storage/v1/b/policy-bucket/o/{name}"),
                ADMIN,
                b"",
            )
            .json_ok();
        let list = server
            .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
            .json_ok();
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
                .json_ok(),
            before
        );
        assert_eq!(
            server
                .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
                .json_ok(),
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
            server.post("policy-bucket", &form, bytes).expect(204);
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
            .json_ok();
        let list = server
            .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
            .json_ok();
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
                    .json_ok(),
                before
            );
            assert_eq!(
                server
                    .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
                    .json_ok(),
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
        reply.expect(201);
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
        reply.expect(303);
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
    server.request("PUT","/storage/v1/b/policy-bucket/iam",ADMIN,br#"{"bindings":[{"role":"roles/storage.objectAdmin","members":["serviceAccount:scoped@example.invalid"]}]}"#).json_ok();
    server.request("PATCH","/storage/v1/b/policy-bucket",ADMIN,br#"{"iamConfiguration":{"publicAccessPrevention":"enforced","uniformBucketLevelAccess":{"enabled":true}}}"#).json_ok();
    let variants = [
        (FormVersion::V4, Signer::Rsa("scoped@example.invalid")),
        (FormVersion::V4, Signer::Hmac(&key)),
        (FormVersion::V2, Signer::Rsa("scoped@example.invalid")),
        (FormVersion::V2, Signer::Hmac(&key)),
    ];
    for (index, (version, signer)) in variants.into_iter().enumerate() {
        let form = variant_fields(version, signer, &format!("scoped-{index}"), &[], &[]);
        server.post("policy-bucket", &form, b"scoped").expect(204);
    }
    let before = server
        .request("GET", "/storage/v1/b/policy-bucket/o", ADMIN, b"")
        .json_ok();
    server
        .request(
            "PUT",
            "/storage/v1/b/policy-bucket/iam",
            ADMIN,
            br#"{"bindings":[]}"#,
        )
        .json_ok();
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
            .json_ok(),
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
    server
        .post_with_headers("policy-bucket", &plain, b"local bearer extension", ADMIN)
        .expect(204);
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
        .json_ok();
    server
        .post("policy-bucket", &plain, b"public anonymous")
        .expect(204);
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

/// Permissive `signed_urls=verify` checks policy signatures and their x-goog-date window as
/// strict mode does, without any signer permission; the default profile ignores both.
#[test]
fn permissive_verification_checks_policy_signatures_and_their_date_window() {
    use gcs_mimic::auth::test_signer;
    let launch = |dir: &TestDir, options: &[&str]| {
        Server::launch(
            http_command(&dir.0.join("data"))
                .args(["--api-base-url", "http://127.0.0.1:4443"])
                .args(options),
        )
    };
    let early = SystemTime::now() + Duration::from_secs(1200);
    let stale = SystemTime::now() - Duration::from_secs(8 * 86400);
    let dir = TestDir::new();
    let server = launch(&dir, &["--signed-urls", "verify"]);
    server.bucket("policy-bucket");
    let key = server.key("unregistered@example.invalid");
    let rsa = Signer::Rsa(test_signer::ID);
    for (name, form) in [
        ("rsa", fields(rsa, "policy-bucket", "rsa", &[], &[])),
        (
            "hmac",
            fields(Signer::Hmac(&key), "policy-bucket", "hmac", &[], &[]),
        ),
        (
            "v2",
            v2_fields(
                rsa,
                Some("policy-bucket"),
                "v2",
                SystemTime::now() + Duration::from_secs(3600),
            ),
        ),
    ] {
        server
            .post("policy-bucket", &form, name.as_bytes())
            .expect(204);
        assert_eq!(
            server
                .request("GET", &format!("/policy-bucket/{name}"), "", b"")
                .body,
            name.as_bytes()
        );
    }
    let unknown = fields(
        Signer::Rsa("unknown@example.invalid"),
        "policy-bucket",
        "denied",
        &[],
        &[],
    );
    let mut tampered = fields(rsa, "policy-bucket", "denied", &[], &[]);
    for (field, value) in &mut tampered {
        if field == "x-goog-signature" {
            value.replace_range(..2, if value.starts_with("00") { "11" } else { "00" });
        }
    }
    let rejected = [
        (unknown, 403, "InvalidAccessKeyId"),
        (tampered.clone(), 403, "SignatureDoesNotMatch"),
        (
            fields_signed_at(rsa, "policy-bucket", "denied", &[], &[], early),
            400,
            "InvalidPolicyDocument",
        ),
        (
            fields_signed_at(rsa, "policy-bucket", "denied", &[], &[], stale),
            400,
            "InvalidPolicyDocument",
        ),
    ];
    for (form, status, code) in &rejected {
        error(
            &server.post("policy-bucket", form, b"denied"),
            *status,
            code,
        );
    }
    server
        .request("GET", "/policy-bucket/denied", "", b"")
        .expect(404);
    let ignoring_dir = TestDir::new();
    let ignoring = launch(&ignoring_dir, &[]);
    ignoring.bucket("policy-bucket");
    for (form, _, _) in rejected {
        ignoring
            .post("policy-bucket", &form, b"ignored")
            .expect(204);
    }
}

declare_server!();
