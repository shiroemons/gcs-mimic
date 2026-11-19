#![cfg(any(target_os = "linux", target_os = "macos"))]
//! Strict mode confers synchronization authority only through an explicit project-level
//! `roles/storage.admin` bearer identity. Every other credential that works against the Storage
//! API must still be refused by every synchronization control, and before any body or query is
//! parsed so that a refused caller learns nothing from the control's validation errors.

use crate::common::{
    self, TestDir,
    effects::Reply,
    process::{Server, http_command},
};
use gcs_mimic::auth::signature::{SignUrlRequest, sign_url, timestamp};
use hyper::HeaderMap;
use ring::{digest, hmac};
use serde_json::json;
use std::time::SystemTime;

const PUBLIC: &str = include_str!("../../fixtures/signing/test-only-public.pem");
const PRIVATE: &[u8] = include_bytes!("../../fixtures/signing/test-only-private.pem");
const BUCKET: &str = "test-bucket";
const OBJECT: &str = "probe";
const BODY_LIMIT: usize = 64 * 1024;

/// Every synchronization control with the method it is documented with.
const CONTROLS: [(&str, &str); 7] = [
    ("GET", "/_internal/sync"),
    ("GET", "/_internal/sync/conflicts"),
    ("POST", "/_internal/sync/approve"),
    ("POST", "/_internal/sync/reject"),
    ("POST", "/_internal/sync/rescan"),
    ("POST", "/_internal/sync/resolve"),
    ("POST", "/_internal/sync/wait"),
];

/// A credential that is valid for the Storage API but must never reach a control.
#[derive(Clone, Copy, Debug)]
enum Credential {
    /// An identity with no project role whose only authority is a bucket IAM binding.
    BucketIam,
    /// A V4 signed URL whose signer holds project `roles/storage.admin`.
    SignedUrl,
    /// A V4 HMAC header signature of a key owned by a project `roles/storage.admin` identity.
    Hmac,
    /// No credential, relying on a bucket IAM binding for `allUsers`.
    AllUsers,
    /// A role-less identity relying on a bucket `roles/storage.admin` binding for
    /// `allAuthenticatedUsers`.
    AllAuthenticatedUsers,
}

const CREDENTIALS: [Credential; 5] = [
    Credential::BucketIam,
    Credential::SignedUrl,
    Credential::Hmac,
    Credential::AllUsers,
    Credential::AllAuthenticatedUsers,
];

/// A request target and the credential header lines that go with it.
struct Signed {
    target: String,
    headers: String,
}

struct Hmac {
    id: String,
    secret: String,
}

struct Fixture {
    server: Server,
    _data: TestDir,
    _tree: TestDir,
    hmac: Hmac,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn bearer(token: &str) -> String {
    format!("Authorization: Bearer {token}\r\n")
}

impl Fixture {
    fn start() -> Self {
        let data = TestDir::new();
        let tree = TestDir::new();
        let inputs = TestDir::new();
        let registry = common::process::write_identities(
            &inputs.0,
            &json!({"identities":[
                {"id":"admin@example.invalid","tokens":["admin"],"project_roles":{"local":["roles/storage.admin"]}},
                {"id":"keys@example.invalid","tokens":["keys"],"project_roles":{"local":["roles/storage.hmacKeyAdmin"]}},
                {"id":"hmac-owner@example.invalid","tokens":[],"project_roles":{"local":["roles/storage.admin"]}},
                {"id":"signer@example.invalid","tokens":[],"public_keys":[PUBLIC],"project_roles":{"local":["roles/storage.admin"]}},
                {"id":"bucket-admin@example.invalid","tokens":["bucket-admin"]},
                {"id":"member@example.invalid","tokens":["member"]}
            ]}),
        );
        let server = Server::launch(
            http_command(&data.0)
                .args([
                    "--seed-project",
                    "local",
                    "--api-base-url",
                    "http://127.0.0.1:4443",
                    "--auth-mode",
                    "strict",
                    "--identity-file",
                ])
                .arg(&registry)
                .arg("--sync-dir")
                .arg(&tree.0),
        );
        let key = send(
            &server,
            "POST",
            "/storage/v1/projects/local/hmacKeys?serviceAccountEmail=hmac-owner%40example.invalid",
            &bearer("keys"),
            b"",
        )
        .json_ok();
        let hmac = Hmac {
            id: key["metadata"]["accessId"].as_str().unwrap().into(),
            secret: key["secret"].as_str().unwrap().into(),
        };
        let fixture = Self {
            server,
            _data: data,
            _tree: tree,
            hmac,
        };
        fixture.prepare_bucket();
        fixture
    }

    fn prepare_bucket(&self) {
        let admin = bearer("admin");
        self.request(
            "POST",
            "/storage/v1/b?project=local",
            &admin,
            br#"{"name":"test-bucket"}"#,
        )
        .json_ok();
        self.request(
            "POST",
            &format!("/upload/storage/v1/b/{BUCKET}/o?uploadType=media&name={OBJECT}"),
            &admin,
            b"probe bytes",
        )
        .json_ok();
        // Before the bindings exist, the credentials below are refused by the Storage API, so a
        // later success is the binding's doing and not a lenient test setup.
        self.bucket_metadata("bucket-admin").expect(403);
        self.bucket_metadata("member").expect(403);
        self.object_media(None).expect_either(&[401, 403]);
        let policy = json!({"bindings":[
            {"role":"roles/storage.admin","members":["user:bucket-admin@example.invalid"]},
            {"role":"roles/storage.admin","members":["allAuthenticatedUsers"]},
            {"role":"roles/storage.objectViewer","members":["allUsers"]}
        ]});
        self.request(
            "PUT",
            &format!("/storage/v1/b/{BUCKET}/iam"),
            &admin,
            policy.to_string().as_bytes(),
        )
        .json_ok();
    }

    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        send(&self.server, method, path, headers, body)
    }

    fn bucket_metadata(&self, token: &str) -> Reply {
        self.request(
            "GET",
            &format!("/storage/v1/b/{BUCKET}"),
            &bearer(token),
            b"",
        )
    }

    fn object_media(&self, token: Option<&str>) -> Reply {
        self.request(
            "GET",
            &format!("/storage/v1/b/{BUCKET}/o/{OBJECT}?alt=media"),
            &token.map_or_default(bearer),
            b"",
        )
    }

    /// The same credential applied to a different request.
    fn sign(&self, credential: Credential, method: &str, target: &str) -> Signed {
        match credential {
            Credential::BucketIam => Signed {
                target: target.into(),
                headers: bearer("bucket-admin"),
            },
            Credential::AllUsers => Signed {
                target: target.into(),
                headers: String::new(),
            },
            Credential::AllAuthenticatedUsers => Signed {
                target: target.into(),
                headers: bearer("member"),
            },
            Credential::SignedUrl => {
                let (path, query) = target.split_once('?').unwrap_or((target, ""));
                // The signer only signs a POST that starts a resumable upload, so POST controls
                // carry that header; authentication ignores it, which is what the test asserts.
                let resumable = method == "POST";
                let mut signed_headers = HeaderMap::new();
                if resumable {
                    signed_headers.insert("x-goog-resumable", "start".parse().unwrap());
                }
                let url = sign_url(SignUrlRequest {
                    url: &format!("http://localhost{path}"),
                    method: &method.parse().unwrap(),
                    signer: "signer@example.invalid",
                    private_key: PRIVATE,
                    expires: 60,
                    now: SystemTime::now(),
                    headers: signed_headers,
                })
                .unwrap();
                let signed = url.strip_prefix("http://localhost").unwrap();
                Signed {
                    target: if query.is_empty() {
                        signed.into()
                    } else {
                        format!("{signed}&{query}")
                    },
                    headers: if resumable {
                        "x-goog-resumable: start\r\n".into()
                    } else {
                        String::new()
                    },
                }
            }
            Credential::Hmac => self.hmac_header(method, target),
        }
    }

    /// A GOOG4-HMAC-SHA256 header signature over `path` with the unsigned payload.
    fn hmac_header(&self, method: &str, target: &str) -> Signed {
        let path = target.split_once('?').map_or(target, |(path, _)| path);
        let query = target.split_once('?').map_or("", |(_, query)| query);
        let date = timestamp(SystemTime::now()).unwrap();
        let scope = format!("{}/auto/storage/goog4_request", &date[..8]);
        let names = "host;x-goog-content-sha256;x-goog-date";
        let canonical = format!(
            "{method}\n{path}\n{query}\nhost:localhost\nx-goog-content-sha256:UNSIGNED-PAYLOAD\nx-goog-date:{date}\n\n{names}\nUNSIGNED-PAYLOAD"
        );
        let message = format!(
            "GOOG4-HMAC-SHA256\n{date}\n{scope}\n{}",
            hex(digest::digest(&digest::SHA256, canonical.as_bytes()).as_ref())
        );
        let mut key = hmac::Key::new(
            hmac::HMAC_SHA256,
            format!("GOOG4{}", self.hmac.secret).as_bytes(),
        );
        for part in scope.split('/') {
            key = hmac::Key::new(
                hmac::HMAC_SHA256,
                hmac::sign(&key, part.as_bytes()).as_ref(),
            );
        }
        let signature = hex(hmac::sign(&key, message.as_bytes()).as_ref());
        Signed {
            target: target.into(),
            headers: format!(
                "x-goog-content-sha256: UNSIGNED-PAYLOAD\r\nx-goog-date: {date}\r\nAuthorization: GOOG4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={names}, Signature={signature}\r\n",
                self.hmac.id
            ),
        }
    }

    /// Sends a control request the way `gcs-mimic sync` does, with this release's version.
    fn control(&self, credential: Credential, method: &str, target: &str, body: &[u8]) -> Reply {
        let signed = self.sign(credential, method, target);
        self.request(
            method,
            &signed.target,
            &format!("{}{}", common::sync_version_header(), signed.headers),
            body,
        )
    }

    /// Proves the credential is accepted by the Storage API for the same identity and request
    /// shape, so a refusal from a control cannot come from an invalid credential.
    fn assert_storage_access(&self, credential: Credential) {
        let (method, target, expected) = match credential {
            Credential::BucketIam | Credential::AllAuthenticatedUsers => {
                ("GET", format!("/storage/v1/b/{BUCKET}"), 200)
            }
            Credential::AllUsers => (
                "GET",
                format!("/storage/v1/b/{BUCKET}/o/{OBJECT}?alt=media"),
                200,
            ),
            Credential::SignedUrl | Credential::Hmac => ("GET", format!("/{BUCKET}/{OBJECT}"), 200),
        };
        let signed = self.sign(credential, method, &target);
        let reply = self.request(method, &signed.target, &signed.headers, b"");
        reply.expect_with(expected, format_args!("{credential:?} {target}"));
    }
}

fn send(server: &Server, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
    common::effects::request(&server.address, method, path, headers, body)
}

trait ExpectEither {
    fn expect_either(&self, statuses: &[u16]);
}

impl ExpectEither for Reply {
    #[track_caller]
    fn expect_either(&self, statuses: &[u16]) {
        assert!(
            statuses.contains(&self.status),
            "expected one of {statuses:?}, got {}: {}",
            self.status,
            String::from_utf8_lossy(&self.body)
        );
    }
}

/// A body each control accepts as well-formed, so a refusal cannot come from parsing it.
fn valid_body(path: &str) -> &'static [u8] {
    match path {
        "/_internal/sync/rescan" | "/_internal/sync/wait" => b"{}",
        _ => b"",
    }
}

#[test]
fn credentials_that_work_for_storage_never_confer_sync_administration() {
    let fixture = Fixture::start();
    for credential in CREDENTIALS {
        fixture.assert_storage_access(credential);
        for (method, path) in CONTROLS {
            let reply = fixture.control(credential, method, path, valid_body(path));
            reply.expect_either(&[401, 403]);
        }
    }
}

#[test]
fn every_credential_class_gets_the_documented_refusal() {
    let fixture = Fixture::start();
    for (credential, expected) in [
        (Credential::BucketIam, 403),
        (Credential::AllAuthenticatedUsers, 403),
        (Credential::AllUsers, 401),
    ] {
        for (method, path) in CONTROLS {
            fixture
                .control(credential, method, path, valid_body(path))
                .expect_with(expected, format_args!("{credential:?} {method} {path}"));
        }
    }
}

/// A refused caller must not see a 400, 405, 404 or 413 produced by the control's own
/// validation: those answers leak which routes, fields and limits exist.
#[test]
fn refusal_precedes_body_query_method_and_route_validation() {
    let fixture = Fixture::start();
    let oversized = vec![b' '; BODY_LIMIT + 1];
    let admin = format!("{}{}", common::sync_version_header(), bearer("admin"));
    for (method, path) in CONTROLS {
        let other = if method == "GET" { "POST" } else { "GET" };
        let probes: [(&str, &str, String, &[u8]); 4] = [
            ("invalid body", method, path.into(), b"{not json"),
            ("unknown query", method, format!("{path}?unknown=1"), b"{}"),
            ("oversized body", method, path.into(), oversized.as_slice()),
            ("wrong method", other, path.into(), b""),
        ];
        for (label, probe_method, target, body) in probes {
            // The privileged control reaches validation, so the probe is not vacuous.
            let reply = fixture.request(probe_method, &target, &admin, body);
            assert!(
                matches!(reply.status, 400 | 405 | 413)
                    // The conflict listing ignores a small body; its other validations still answer.
                    || (path == "/_internal/sync/conflicts" && label == "invalid body" && reply.status == 200),
                "admin {label} {probe_method} {target}: {} {}",
                reply.status,
                String::from_utf8_lossy(&reply.body)
            );
            for credential in CREDENTIALS {
                let reply = fixture.control(credential, probe_method, &target, body);
                reply.expect_either(&[401, 403]);
            }
            for token in [None, Some("invalid")] {
                let headers = format!(
                    "{}{}",
                    common::sync_version_header(),
                    token.map_or_default(bearer)
                );
                fixture
                    .request(probe_method, &target, &headers, body)
                    .expect_either(&[401]);
            }
        }
        // Unknown routes are reported only to a caller that may use the controls.
        for credential in CREDENTIALS {
            fixture
                .control(credential, method, "/_internal/sync/unknown", b"")
                .expect_either(&[401, 403]);
        }
    }
}

#[test]
fn a_declared_project_admin_still_reaches_every_control() {
    let fixture = Fixture::start();
    let admin = format!("{}{}", common::sync_version_header(), bearer("admin"));
    for (method, path, body, expected) in [
        ("GET", "/_internal/sync", &b""[..], 200),
        ("GET", "/_internal/sync/conflicts", b"", 200),
        ("POST", "/_internal/sync/rescan", b"{}", 202),
        (
            "POST",
            "/_internal/sync/wait",
            b"{\"timeout_ms\":5000}",
            200,
        ),
        (
            "POST",
            "/_internal/sync/approve",
            br#"{"deletion_set":"00000000000000000000000000000000"}"#,
            409,
        ),
        (
            "POST",
            "/_internal/sync/reject",
            br#"{"deletion_set":"00000000000000000000000000000000"}"#,
            409,
        ),
        ("POST", "/_internal/sync/resolve", b"{}", 400),
    ] {
        fixture
            .request(method, path, &admin, body)
            .expect_with(expected, format_args!("{method} {path}"));
    }
}
