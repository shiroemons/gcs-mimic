mod admin_fixture_metadata;
mod admin_reseed;
mod admin_reset;
mod object_effects_support;

use common::TestDir;
use gcs_mimic::auth::signature::sign_url;
use gcs_mimic::config::Config;
use hyper::{HeaderMap, Method, Uri};
use object_effects_support::{Reply, common, request};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    process::{Child, ChildStdout, Stdio},
    time::{Duration, SystemTime},
};

const ADMIN: &str = "Authorization: Bearer admin\r\n";
const OBJECT: &str = "/storage/v1/b/test-bucket/o/value";

struct Server {
    child: Child,
    address: String,
    stdout: BufReader<ChildStdout>,
    ready: Value,
}

impl Server {
    fn fixture(&self, metadata: Value, body: &[u8], auth: &str) -> Reply {
        let mut wire = format!("--fixture\r\nContent-Type: application/json\r\n\r\n{metadata}\r\n--fixture\r\nContent-Type: application/octet-stream\r\n\r\n").into_bytes();
        wire.extend_from_slice(body);
        wire.extend_from_slice(b"\r\n--fixture--\r\n");
        self.fixture_wire(&wire, auth)
    }

    fn fixture_wire(&self, wire: &[u8], auth: &str) -> Reply {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream, "POST /_internal/objects HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=fixture\r\nContent-Length: {}\r\n{auth}Connection: close\r\n\r\n", wire.len()).unwrap();
        stream.write_all(wire).unwrap();
        let response = common::read_http_response(&mut BufReader::new(stream), false).unwrap();
        let start = response
            .windows(4)
            .position(|value| value == b"\r\n\r\n")
            .unwrap();
        let head = std::str::from_utf8(&response[..start]).unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        let headers = head
            .lines()
            .skip(1)
            .map(|line| {
                let (name, value) = line.split_once(':').unwrap();
                (name.to_ascii_lowercase(), value.trim().to_owned())
            })
            .collect();
        Reply {
            status,
            headers,
            body: response[start + 4..].to_vec(),
        }
    }
    fn start(dir: &TestDir, options: &[&str]) -> Self {
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--seed-project",
                "local",
                "--data-dir",
            ])
            .arg(&dir.0)
            .args(options)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "ready");
        Self {
            child,
            address: ready["listen"].as_str().unwrap().into(),
            stdout,
            ready,
        }
    }

    fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        request(&self.address, method, path, headers, body)
    }

    fn configure(&self, external: &str, public: &str) -> Reply {
        self.request(
            "PUT",
            "/_internal/config",
            ADMIN,
            json!({"externalUrl":external,"publicHost":public})
                .to_string()
                .as_bytes(),
        )
    }

    fn populate(&self) {
        self.request(
            "POST",
            "/storage/v1/b?project=local",
            ADMIN,
            br#"{"name":"test-bucket"}"#,
        )
        .json();
        self.request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            ADMIN,
            b"original",
        )
        .json();
    }

    fn status_with_host(&self, path: &str, host: &str, headers: &str) -> u16 {
        self.method_status_with_host("GET", path, host, headers, b"")
    }

    fn method_status_with_host(
        &self,
        method: &str,
        path: &str,
        host: &str,
        headers: &str,
        body: &[u8],
    ) -> u16 {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n", body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
        let response = common::read_http_response(&mut BufReader::new(stream), false).unwrap();
        String::from_utf8_lossy(&response)
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    }
}

#[test]
fn direct_fixtures_preserve_metadata_generation_and_times_across_restart() {
    let dir = TestDir::new();
    let options = [
        "--admin-enabled",
        "true",
        "--api-base-url",
        "http://fixture.invalid",
    ];
    let server = Server::start(&dir, &options);
    server.populate();
    let metadata = json!({"bucket":"test-bucket", "name":"fixture/猫", "generation":"8000000000000000000",
        "created":"2001-02-03T04:05:06.123456Z", "updated":"2002-03-04T05:06:07.654321Z",
        "contentType":"text/fixture", "contentEncoding":"identity", "contentLanguage":"en",
        "contentDisposition":"attachment", "cacheControl":"no-store", "metadata":{"fixture":"yes"},
        "storageClass":"NEARLINE", "customTime":"2000-01-02T03:04:05.123456Z"});
    let reply = server.fixture(metadata.clone(), b"fixture bytes", "");
    assert_eq!(
        reply.status,
        200,
        "{}",
        String::from_utf8_lossy(&reply.body)
    );
    let resource = reply.json();
    for key in [
        "generation",
        "contentType",
        "contentEncoding",
        "contentLanguage",
        "contentDisposition",
        "cacheControl",
        "metadata",
        "storageClass",
        "customTime",
        "updated",
    ] {
        assert_eq!(resource[key], metadata[key], "{key}");
    }
    assert_eq!(resource["timeCreated"], metadata["created"]);
    assert_eq!(resource["size"], "13");
    assert!(resource["md5Hash"].is_string());
    assert!(resource["crc32c"].is_string());
    assert_eq!(server.fixture(metadata, b"replacement", "").status, 409);
    let path = "/storage/v1/b/test-bucket/o/fixture%2F%E7%8C%AB";
    assert_eq!(
        server
            .request("GET", &format!("{path}?alt=media"), "", b"")
            .body,
        b"fixture bytes"
    );
    let next = server
        .fixture(
            json!({"bucket":"test-bucket","name":"automatic", "created":"1999-01-01T00:00:00Z"}),
            b"next",
            "",
        )
        .json();
    assert_eq!(next["generation"], "8000000000000000001");
    assert_eq!(next["timeCreated"], "1999-01-01T00:00:00.000Z");
    drop(server);
    let server = Server::start(&dir, &options);
    assert_eq!(server.request("GET", path, "", b"").json(), resource);
    assert_eq!(
        server
            .request("GET", &format!("{path}?alt=media"), "", b"")
            .body,
        b"fixture bytes"
    );
}

#[test]
fn direct_fixture_rejections_preserve_existing_bytes() {
    let dir = TestDir::new();
    let server = Server::start(
        &dir,
        &["--admin-enabled", "true", "--max-object-bytes", "16"],
    );
    server.populate();
    let original = server.request("GET", OBJECT, "", b"").json();
    let listing = server
        .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
        .json();
    let base = json!({"bucket":"test-bucket", "name":"value"});
    for (field, value) in [
        ("generation", json!("0")),
        ("generation", json!("9223372036854775807")),
        ("created", json!("2001-01-01T00:00:00.1234567Z")),
        ("updated", json!("invalid")),
        ("created", json!("9999-12-31T23:59:59-01:00")),
        ("created", json!("0000-01-01T00:00:00+01:00")),
        ("updated", json!("9999-12-31T23:59:59-01:00")),
        ("updated", json!("0000-01-01T00:00:00+01:00")),
        ("customTime", json!("9999-12-31T23:59:59-01:00")),
        ("storageClass", json!("INVALID")),
        ("acl", json!([{"entity":"allUsers", "role":"WRITER"}])),
        (
            "acl",
            json!([{"entity":"allUsers", "role":"READER", "entityId":"unpreserved"}]),
        ),
        (
            "acl",
            json!([{"entity":"allUsers", "role":"READER", "unknownField":true}]),
        ),
        ("retention", json!({"mode":"Locked"})),
        (
            "retention",
            json!({"mode":"Unlocked", "retainUntilTime":"9999-12-31T23:59:59-01:00"}),
        ),
        ("temporaryHold", json!("true")),
        ("eventBasedHold", Value::Null),
        ("deleted", json!("2000-01-01T00:00:00Z")),
        ("md5Hash", json!("AAAAAAAAAAAAAAAAAAAAAA==")),
        ("crc32c", json!("AAAAAA==")),
        ("unexpected", json!(true)),
        ("notificationMode", json!(true)),
        ("notificationMode", json!("unknown")),
    ] {
        let mut metadata = base.clone();
        metadata[field] = value;
        assert_eq!(
            server.fixture(metadata, b"rejected", "").status,
            400,
            "{field}"
        );
    }
    assert_eq!(server.fixture(base.clone(), &[1; 17], "").status, 413);
    let wire = format!(
        "--fixture\r\nContent-Type: application/json\r\n\r\n{base}\r\n--fixture\r\nContent-Type: application/octet-stream\r\n\r\ninvalid\r\n--fixture--broken"
    );
    assert_eq!(server.fixture_wire(wire.as_bytes(), "").status, 400);
    assert_eq!(server.request("GET", OBJECT, "", b"").json(), original);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
            .json(),
        listing
    );
    assert_eq!(
        server
            .request("GET", &format!("{OBJECT}?alt=media"), "", b"")
            .body,
        b"original"
    );
}

#[test]
fn initial_fixture_notification_mode_is_request_local_and_preserves_runtime_events() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    server
        .request(
            "POST",
            "/storage/v1/b/test-bucket/notificationConfigs",
            "",
            br#"{"topic":"projects/local/topics/events"}"#,
        )
        .json();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let count = || {
        connection
            .query_row("SELECT count(*) FROM notification_outbox", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    assert_eq!(count(), 0);
    server
        .fixture(
            json!({"bucket":"test-bucket","name":"value","notificationMode":"suppress"}),
            b"initial",
            "",
        )
        .json();
    assert_eq!(count(), 0);
    server
        .fixture(
            json!({"bucket":"test-bucket","name":"value"}),
            b"ordinary",
            "",
        )
        .json();
    assert_eq!(count(), 2);
    assert_eq!(
        server
            .fixture(
                json!({"bucket":"test-bucket","name":"value","notificationMode":"invalid"}),
                b"rejected",
                ""
            )
            .status,
        400
    );
    assert_eq!(count(), 2);
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/value?alt=media",
                "",
                b""
            )
            .body,
        b"ordinary"
    );
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            "",
            b"API write",
        )
        .json();
    assert_eq!(count(), 4);
    drop(connection);
}

#[test]
fn direct_fixture_retention_expiry_range_is_validated_in_the_publication_transaction() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            "",
            br#"{"name":"retained-fixtures","retentionPolicy":{"retentionPeriod":"60"}}"#,
        )
        .json();
    let original = server.fixture(json!({"bucket":"retained-fixtures", "name":"value", "generation":"1", "created":"2000-01-01T00:00:00Z"}), b"original", "").json();
    let listing = server
        .request("GET", "/storage/v1/b/retained-fixtures/o", "", b"")
        .json();
    let rejected = server.fixture(json!({"bucket":"retained-fixtures", "name":"value", "generation":"8000000000000000000", "created":"9999-12-31T23:59:59Z"}), b"rejected", "");
    assert_eq!(
        rejected.status,
        400,
        "{}",
        String::from_utf8_lossy(&rejected.body)
    );
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/retained-fixtures/o/value", "", b"")
            .json(),
        original
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/retained-fixtures/o/value?alt=media",
                "",
                b""
            )
            .body,
        b"original"
    );
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/retained-fixtures/o", "", b"")
            .json(),
        listing
    );
    let next = server
        .fixture(
            json!({"bucket":"retained-fixtures", "name":"next"}),
            b"valid",
            "",
        )
        .json();
    assert!(
        next["generation"].as_str().unwrap().parse::<i64>().unwrap() < 8_000_000_000_000_000_000
    );
    let bucket = server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            "",
            br#"{"name":"future-fixtures"}"#,
        )
        .json();
    server
        .fixture(
            json!({"bucket":"future-fixtures", "name":"value", "created":"9999-12-31T23:59:59Z"}),
            b"valid",
            "",
        )
        .json();
    assert_eq!(
        server
            .request(
                "PATCH",
                "/storage/v1/b/future-fixtures",
                "",
                br#"{"retentionPolicy":{"retentionPeriod":"60"}}"#
            )
            .status,
        400
    );
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/future-fixtures", "", b"")
            .json(),
        bucket
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/future-fixtures/o/value?alt=media",
                "",
                b""
            )
            .body,
        b"valid"
    );
}

#[test]
fn direct_fixture_management_is_opt_in_and_independent_of_google_iam() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &[]);
    assert_eq!(server.fixture(json!({}), b"bytes", "").status, 404);
    drop(server);
    let registry = dir.0.join("identities.json");
    std::fs::write(&registry, json!({"identities":[
        {"id":"manager@example.invalid", "tokens":["manager"], "project_roles":{}},
        {"id":"cloud-admin@example.invalid", "tokens":["cloud-admin"], "project_roles":{"local":["roles/storage.admin"]}}
    ]}).to_string()).unwrap();
    let server = Server::start(
        &dir,
        &[
            "--admin-enabled",
            "true",
            "--admin-principal",
            "manager@example.invalid",
            "--auth-mode",
            "strict",
            "--identity-file",
            registry.to_str().unwrap(),
        ],
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/storage/v1/b?project=local",
                "Authorization: Bearer cloud-admin\r\n",
                br#"{"name":"test-bucket"}"#
            )
            .status,
        200
    );
    let fixture = json!({"bucket":"test-bucket","name":"fixture"});
    for mode in ["emit", "suppress"] {
        let mut denied = fixture.clone();
        denied["notificationMode"] = json!(mode);
        for (auth, status) in [
            ("", 401),
            ("Authorization: Bearer unknown\r\n", 401),
            ("Authorization: Bearer cloud-admin\r\n", 403),
        ] {
            assert_eq!(
                server.fixture(denied.clone(), b"rejected", auth).status,
                status
            );
        }
    }
    assert_eq!(
        server
            .fixture(fixture, b"fixture", "Authorization: Bearer manager\r\n")
            .status,
        200
    );
    assert_eq!(
        server
            .request(
                "POST",
                "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=forbidden",
                "Authorization: Bearer manager\r\n",
                b"denied"
            )
            .status,
        403
    );
}

#[test]
fn direct_fixture_progress_renews_inactivity_without_disabling_the_deadline() {
    let dir = TestDir::new();
    let server = Server::start(
        &dir,
        &[
            "--admin-enabled",
            "true",
            "--connection-timeout-ms",
            "200",
            "--object-request-timeout-ms",
            "5000",
        ],
    );
    server.populate();
    let prefix = b"--fixture\r\nContent-Type: application/json\r\n\r\n{\"bucket\":\"test-bucket\",\"name\":\"slow\"}\r\n--fixture\r\nContent-Type: application/octet-stream\r\n\r\n";
    let suffix = b"\r\n--fixture--\r\n";
    let mut stream = TcpStream::connect(&server.address).unwrap();
    stream.set_nodelay(true).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(stream, "POST /_internal/objects HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", prefix.len() + 20 * 5 + suffix.len()).unwrap();
    stream.write_all(prefix).unwrap();
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(30));
        stream.write_all(b"chunk").unwrap();
    }
    stream.write_all(suffix).unwrap();
    let response = common::read_http_response(&mut BufReader::new(stream), false).unwrap();
    assert!(
        response.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&response)
    );
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/slow?alt=media", "", b"")
            .body,
        b"chunk".repeat(20)
    );
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            common::stop_server(&mut self.child, &mut self.stdout);
        }
    }
}

#[test]
fn virtual_host_objects_named_like_management_routes_remain_readable_and_writable() {
    for profile in ["native", "legacy-emulator"] {
        for enabled in ["false", "true"] {
            let dir = TestDir::new();
            let server = Server::start(
                &dir,
                &[
                    "--admin-enabled",
                    enabled,
                    "--compat-profile",
                    profile,
                    "--xml-virtual-host-domains",
                    "storage.invalid",
                ],
            );
            server.populate();
            let host = "test-bucket.storage.invalid";
            assert_eq!(
                server.method_status_with_host(
                    "PUT",
                    "/_internal/config",
                    host,
                    "",
                    b"object bytes"
                ),
                200
            );
            assert_eq!(server.status_with_host("/_internal/config", host, ""), 200);
            assert_eq!(
                server
                    .request(
                        "GET",
                        "/storage/v1/b/test-bucket/o/_internal%2Fconfig?alt=media",
                        "",
                        b""
                    )
                    .body,
                b"object bytes"
            );
            assert_eq!(
                server
                    .configure("http://api.invalid", "public.invalid")
                    .status,
                if enabled == "true" { 200 } else { 404 }
            );
            assert_eq!(server.status_with_host("/_internal/config", host, ""), 200);
        }
    }
}

#[test]
fn endpoint_changes_preserve_media_multipart_and_copy_replay_identity() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    let media_path = "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=replayed";
    let media_headers = "X-Goog-Gcs-Idempotency-Token: media-token\r\n";
    let first_media = server
        .request("POST", media_path, media_headers, b"original")
        .json();
    let copy_path = "/storage/v1/b/test-bucket/o/value/copyTo/b/test-bucket/o/copied";
    let copy_headers = "X-Goog-Gcs-Idempotency-Token: copy-token\r\n";
    let first_copy = server
        .request("POST", copy_path, copy_headers, b"null")
        .json();
    let multipart = || {
        let body = b"--boundary\r\nContent-Type: application/json\r\n\r\n{\"name\":\"multipart-replay\"}\r\n--boundary\r\nContent-Type: text/plain\r\n\r\noriginal\r\n--boundary--\r\n";
        let mut stream = TcpStream::connect(&server.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream, "POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=boundary\r\nX-Goog-Gcs-Idempotency-Token: multipart-token\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        stream.write_all(body).unwrap();
        let response = common::read_http_response(&mut BufReader::new(stream), false).unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        serde_json::from_str::<Value>(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    };
    let first_multipart = multipart();
    assert_eq!(
        server
            .configure("http://changed.invalid", "public.invalid")
            .status,
        200
    );
    let second_media = server
        .request("POST", media_path, media_headers, b"original")
        .json();
    let second_copy = server
        .request("POST", copy_path, copy_headers, b"null")
        .json();
    let second_multipart = multipart();
    for (first, replayed) in [
        (&first_media, &second_media),
        (&first_copy, &second_copy),
        (&first_multipart, &second_multipart),
    ] {
        assert_eq!(first["generation"], replayed["generation"]);
        assert_eq!(first["metageneration"], replayed["metageneration"]);
        assert!(
            replayed["selfLink"]
                .as_str()
                .unwrap()
                .starts_with("http://changed.invalid/")
        );
    }
    let current = server
        .request("GET", "/storage/v1/b/test-bucket/o/replayed", "", b"")
        .json();
    assert_eq!(current["generation"], first_media["generation"]);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/copied", "", b"")
            .json()["generation"],
        first_copy["generation"]
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/multipart-replay",
                "",
                b""
            )
            .json()["generation"],
        first_multipart["generation"]
    );
}

#[test]
fn local_admin_configuration_is_explicit_and_obeys_native_precedence() {
    let dir = TestDir::new();
    let path = dir.0.join("settings.json");
    std::fs::write(&path, br#"{"admin_enabled":true}"#).unwrap();
    let args = ["--config".into(), path.to_str().unwrap().into()];
    let mut environment = std::collections::BTreeMap::new();
    assert!(!Config::default().admin_enabled);
    assert!(Config::load(&args, &environment).unwrap().admin_enabled);
    environment.insert("GCS_MIMIC_ADMIN_ENABLED".into(), "false".into());
    assert!(!Config::load(&args, &environment).unwrap().admin_enabled);
    let mut args = args.to_vec();
    args.extend(["--admin-enabled".into(), "true".into()]);
    assert!(Config::load(&args, &environment).unwrap().admin_enabled);
    assert!(Config::load(&["--admin-enabled".into(), "yes".into()], &environment).is_err());
}

#[test]
fn legacy_healthcheck_and_startup_event_support_container_waits() {
    for profile in ["native", "legacy-emulator"] {
        let dir = TestDir::new();
        let server = Server::start(&dir, &["--compat-profile", profile]);
        assert_eq!(server.ready["executable"], "gcs-mimic");
        assert_eq!(server.ready["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(server.ready["healthcheck_path"], "/_internal/healthcheck");
        assert_eq!(server.ready["admin_enabled"], false);
        assert!(
            server.ready["message"]
                .as_str()
                .unwrap()
                .starts_with("server started at http://")
        );
        let health = server.request("GET", "/_internal/healthcheck", "", b"");
        assert_eq!(health.status, 200);
        let head = server.request("HEAD", "/_internal/healthcheck", "", b"");
        assert_eq!(head.status, 200);
        assert!(head.body.is_empty());
        assert_eq!(
            server
                .request("POST", "/_internal/healthcheck", "", b"")
                .status,
            405
        );
        assert_eq!(
            server
                .request("GET", "/_internal/healthcheck", "", b"body")
                .status,
            413
        );
        assert!(
            common::server_command()
                .args([
                    "healthcheck",
                    "--url",
                    &format!("http://{}/_internal/healthcheck", server.address)
                ])
                .status()
                .unwrap()
                .success()
        );
    }
}

#[test]
fn public_host_updates_replace_implicit_routing_and_preserve_explicit_domains() {
    for profile in ["native", "legacy-emulator"] {
        let dir = TestDir::new();
        let server = Server::start(
            &dir,
            &[
                "--admin-enabled",
                "true",
                "--compat-profile",
                profile,
                "--public-base-url",
                "http://old.invalid",
                "--xml-virtual-host-domains",
                "explicit.invalid",
            ],
        );
        server.populate();
        assert_eq!(
            server.status_with_host("/value", "test-bucket.old.invalid", ""),
            200
        );
        assert_eq!(
            server.configure("http://api.invalid", "new.invalid").status,
            200
        );
        assert_eq!(
            server.status_with_host("/value", "test-bucket.new.invalid", ""),
            200
        );
        assert_eq!(
            server.status_with_host("/value", "test-bucket.explicit.invalid", ""),
            200
        );
        assert_eq!(
            server.status_with_host("/value", "test-bucket.old.invalid", ""),
            404
        );
    }
}

#[test]
fn config_route_is_disabled_by_default_and_changes_only_future_http_links() {
    let dir = TestDir::new();
    {
        let server = Server::start(&dir, &[]);
        assert_eq!(
            server
                .configure("http://mapped.invalid:9876", "public.invalid")
                .status,
            404
        );
    }
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    let original = server.request("GET", OBJECT, "", b"").json();
    assert_eq!(
        server
            .configure("http://mapped.invalid:9876/proxy/", "public.invalid:9876")
            .status,
        200
    );
    let changed = server.request("GET", OBJECT, "", b"").json();
    assert_eq!(original["generation"], changed["generation"]);
    assert_eq!(original["metageneration"], changed["metageneration"]);
    assert!(
        changed["selfLink"]
            .as_str()
            .unwrap()
            .starts_with("http://mapped.invalid:9876/proxy/")
    );
    let bucket = server
        .request("GET", "/storage/v1/b/test-bucket?projection=full", "", b"")
        .json();
    assert!(
        bucket["selfLink"]
            .as_str()
            .unwrap()
            .starts_with("http://mapped.invalid:9876/proxy/")
    );
    assert!(
        bucket["acl"][0]["selfLink"]
            .as_str()
            .unwrap()
            .starts_with("http://mapped.invalid:9876/proxy/")
    );
    for body in [
        br#"{"externalUrl":"http://another.invalid","publicHost":"bad.invalid/path"}"#.as_slice(),
        br#"{"externalUrl":"http://user:password@bad.invalid","publicHost":"new.invalid"}"#,
        br#"{"externalUrl":"http://bucket.public.invalid","publicHost":"public.invalid"}"#,
        br#"{"externalUrl":123}"#,
        br#"[]"#,
        br#"{} {}"#,
    ] {
        assert_eq!(
            server.request("PUT", "/_internal/config", "", body).status,
            400
        );
        assert_eq!(server.request("GET", OBJECT, "", b"").json(), changed);
    }
    assert_eq!(
        server
            .request("PUT", "/_internal/config?externalUrl=ignored", "", b"{}")
            .status,
        400
    );
    assert_eq!(
        server
            .request("POST", "/_internal/config", "", b"{}")
            .status,
        405
    );
    assert_eq!(
        server
            .request("PUT", "/_internal/config", "", &vec![b' '; 65537])
            .status,
        413
    );
    assert_eq!(
        server
            .request(
                "PUT",
                "/_internal/config",
                "",
                br#"{"externalUrl":"","publicHost":"","ignored":true}"#
            )
            .status,
        200
    );
    assert_eq!(server.request("GET", OBJECT, "", b"").json(), changed);
    drop(server);
    let restarted = Server::start(&dir, &["--admin-enabled", "true"]);
    let resource = restarted.request("GET", OBJECT, "", b"").json();
    assert_eq!(resource["generation"], original["generation"]);
    assert!(
        !resource["selfLink"]
            .as_str()
            .unwrap()
            .contains("mapped.invalid")
    );
}

#[test]
fn changed_mapped_endpoint_reaches_new_resumable_locations_and_keeps_old_sessions_usable() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    let initiate = || {
        server.request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=resumable&name=resumed",
            "",
            b"{}",
        )
    };
    let old = initiate();
    assert_eq!(old.status, 200);
    assert_eq!(
        server
            .configure(
                &format!(
                    "http://localhost:{}",
                    server.address.rsplit_once(':').unwrap().1
                ),
                "public.invalid"
            )
            .status,
        200
    );
    let new = initiate();
    let location = &new.headers["location"];
    assert!(location.starts_with("http://localhost:"));
    for session in [&old.headers["location"], location] {
        let uri: Uri = session.parse().unwrap();
        let complete = server.request(
            "PUT",
            uri.path_and_query().unwrap().as_str(),
            "Content-Range: bytes 0-5/6\r\n",
            b"resume",
        );
        assert_eq!(complete.status, 200);
        assert!(
            complete.json()["selfLink"]
                .as_str()
                .unwrap()
                .starts_with("http://localhost:")
        );
    }
    assert_eq!(
        server.request("GET", "/test-bucket/resumed", "", b"").body,
        b"resume"
    );
}

#[test]
fn config_update_during_upload_retains_the_requests_original_endpoint_snapshot() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    let mut upload = TcpStream::connect(&server.address).unwrap();
    upload
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(upload, "POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name=slow HTTP/1.1\r\nHost: localhost\r\nContent-Length: 6\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n").unwrap();
    let mut upload = BufReader::new(upload);
    let mut interim = String::new();
    upload.read_line(&mut interim).unwrap();
    assert!(interim.starts_with("HTTP/1.1 100"), "{interim}");
    interim.clear();
    upload.read_line(&mut interim).unwrap();
    assert_eq!(interim, "\r\n");
    assert_eq!(
        server
            .configure("http://new.invalid:9876", "public.invalid")
            .status,
        200
    );
    upload.get_mut().write_all(b"stream").unwrap();
    let response = common::read_http_response(&mut upload, false).unwrap();
    let response = String::from_utf8_lossy(&response);
    let (_, body) = response.split_once("\r\n\r\n").unwrap();
    let value: Value = serde_json::from_str(body).unwrap();
    assert!(
        value["selfLink"]
            .as_str()
            .unwrap()
            .starts_with(&format!("http://{}/", server.address))
    );
    assert!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/slow", "", b"")
            .json()["selfLink"]
            .as_str()
            .unwrap()
            .starts_with("http://new.invalid:9876/")
    );
}

#[test]
fn strict_config_authorization_and_raw_signed_host_survive_public_host_updates() {
    for profile in ["native", "legacy-emulator"] {
        let dir = TestDir::new();
        let identity_file = dir.0.join("identities.json");
        std::fs::write(&identity_file, json!({"identities":[
            {"id":"admin@example.invalid","tokens":["admin"],"public_keys":[include_str!("../fixtures/signing/test-only-public.pem")],"project_roles":{"local":["roles/storage.admin"]}},
            {"id":"reader@example.invalid","tokens":["reader"],"project_roles":{"local":["roles/storage.objectViewer"]}}
        ]}).to_string()).unwrap();
        let server = Server::start(
            &dir,
            &[
                "--admin-enabled",
                "true",
                "--auth-mode",
                "strict",
                "--identity-file",
                identity_file.to_str().unwrap(),
                "--compat-profile",
                profile,
            ],
        );
        server.populate();
        assert_eq!(
            server.request("PUT", "/_internal/config", "", b"{}").status,
            401
        );
        assert_eq!(
            server
                .request(
                    "PUT",
                    "/_internal/config",
                    "Authorization: Bearer reader\r\n",
                    b"{}"
                )
                .status,
            403
        );
        assert_eq!(
            server
                .request("GET", "/_internal/healthcheck", "", b"")
                .status,
            200
        );
        assert_eq!(
            server
                .configure("http://api.invalid:9876", "localhost")
                .status,
            200
        );
        assert_eq!(
            server.request("GET", "/test-bucket/value", "", b"").status,
            401
        );
        let url = sign_url(
            "http://localhost/test-bucket/value",
            &Method::GET,
            "admin@example.invalid",
            include_bytes!("../fixtures/signing/test-only-private.pem"),
            60,
            SystemTime::now(),
            HeaderMap::new(),
        )
        .unwrap();
        let uri: Uri = url.parse().unwrap();
        assert_eq!(
            server
                .request("GET", uri.path_and_query().unwrap().as_str(), "", b"")
                .body,
            b"original"
        );
        assert_eq!(
            server.status_with_host(uri.path_and_query().unwrap().as_str(), "other.invalid", ""),
            403
        );
        let altered = uri
            .path_and_query()
            .unwrap()
            .as_str()
            .replace("/test-bucket/value", "/test-bucket/other");
        assert_eq!(server.request("GET", &altered, "", b"").status, 403);
    }
}
