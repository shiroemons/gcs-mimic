use crate::common::process::declare_server;
mod admin_fixture_metadata;
mod admin_reseed;
mod admin_reset;
use crate::common;
use common::http::Body;

use common::TestDir;
use common::effects::{Reply, request};
use common::process::{http_command, write_request};
use gcs_mimic::auth::signature::{SignUrlRequest, sign_url};
use gcs_mimic::config::Config;
use hyper::{HeaderMap, Method, Uri};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    time::{Duration, SystemTime},
};

const ADMIN: &str = "Authorization: Bearer admin\r\n";
const OBJECT: &str = "/storage/v1/b/test-bucket/o/value";
/// Past the second that used to end a reset's wait, so a test that holds an upload back this long
/// also shows the reset did not give up.
const HELD_BACK_AFTER: Duration = Duration::from_millis(1_200);

#[test]
fn management_routes_refuse_page_initiated_requests_without_mutation() {
    let dir = TestDir::new();
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    let before = server.request("GET", OBJECT, "", b"").json_ok();
    for (method, path, body) in [
        (
            "PUT",
            "/_internal/config",
            br#"{"externalUrl":"http://changed.invalid"}"#.as_slice(),
        ),
        ("POST", "/_internal/objects", b""),
        ("POST", "/_internal/reset", br#"{"confirm":"gcs-mimic"}"#),
        ("POST", "/_internal/delete_all", b""),
        ("POST", "/_internal/reseed", b""),
        ("POST", "/_internal/lifecycle/run", b""),
        ("POST", "/_internal/sync/approve", b"{}"),
    ] {
        for headers in [
            "Origin: https://evil.example\r\n",
            "Origin: null\r\n",
            "Sec-Fetch-Site: cross-site\r\n",
            "Sec-Fetch-Site: same-site\r\n",
            "Sec-Fetch-Site: same-origin\r\n",
        ] {
            server.request(method, path, headers, body).expect(403);
            assert_eq!(server.request("GET", OBJECT, "", b"").json_ok(), before);
        }
    }
    assert_eq!(
        server.method_status_with_host("PUT", "/_internal/config", "evil.example.com", "", b"{}"),
        421
    );
    for host in [
        "localhost:49152",
        "127.0.0.1",
        "[::1]",
        "gcs:4443",
        "host.docker.internal",
    ] {
        assert_eq!(
            server.method_status_with_host("PUT", "/_internal/config", host, "", b"{}"),
            200
        );
    }
    server
        .request(
            "POST",
            "/_internal/delete_all",
            "Sec-Fetch-Site: none\r\n",
            b"",
        )
        .expect(200);
    server.request("GET", OBJECT, "", b"").expect(404);
}

#[test]
fn lifecycle_run_uses_admin_guards_and_refuses_time_travel_or_invalid_methods() {
    let data = TestDir::new();
    let disabled = Server::start(&data, &[]);
    disabled
        .request("POST", "/_internal/lifecycle/run", "", b"")
        .expect(404);
    drop(disabled);
    let server = Server::start(&data, &["--admin-enabled", "true"]);
    assert_eq!(server.ready["lifecycle_execution"], "enabled");
    for method in ["GET", "PUT", "DELETE"] {
        server
            .request(method, "/_internal/lifecycle/run", "", b"")
            .expect(405);
    }
    server
        .request("POST", "/_internal/lifecycle/run?now=2000", "", b"")
        .expect(400);
    server
        .request("POST", "/_internal/lifecycle/run", "", b"{}")
        .expect(400);
    let counts = server
        .request("POST", "/_internal/lifecycle/run", "", b"")
        .json_ok();
    assert_eq!(
        counts,
        json!({"enabled":true,"scanned":0,"deleted":0,"archived":0,"transitioned":0,"blocked":0,"skipped":0,"aborted":0})
    );
    drop(server);
    let server = Server::start(
        &data,
        &[
            "--admin-enabled",
            "true",
            "--lifecycle-execution",
            "disabled",
        ],
    );
    assert_eq!(server.ready["lifecycle_execution"], "disabled");
    assert_eq!(
        server
            .request("POST", "/_internal/lifecycle/run", "", b"")
            .json_ok(),
        json!({"enabled":false,"scanned":0,"deleted":0,"archived":0,"transitioned":0,"blocked":0,"skipped":0,"aborted":0})
    );
}

#[test]
fn server_drop_preserves_original_panic_for_running_and_exited_children() {
    for graceful in [false, true] {
        for exited in [false, true] {
            let dir = TestDir::new();
            let mut server = Server::launch(&mut http_command(&dir.0));
            if graceful {
                server = server.graceful();
            }
            if exited {
                server.child.kill().unwrap();
                server.child.wait().unwrap();
            }
            let panic = std::panic::catch_unwind(move || {
                let _server = server;
                panic!("original test failure");
            })
            .unwrap_err();
            assert_eq!(panic.downcast_ref::<&str>(), Some(&"original test failure"));
        }
    }
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
        write_request(
            &mut stream,
            "POST",
            "/_internal/objects",
            "localhost",
            &format!(
                "Content-Type: multipart/related; boundary=fixture\r\n{auth}Connection: close\r\n"
            ),
            wire,
        );
        let response =
            common::read_http_response(&mut BufReader::new(stream), Body::Expected).unwrap();
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
        Self::launch(
            http_command(&dir.0)
                .args(["--seed-project", "local"])
                .args(options),
        )
        .graceful()
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
        .json_ok();
        self.request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=value",
            ADMIN,
            b"original",
        )
        .json_ok();
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
        write_request(
            &mut stream,
            method,
            path,
            host,
            &format!("{headers}Connection: close\r\n"),
            body,
        );
        let response =
            common::read_http_response(&mut BufReader::new(stream), Body::Expected).unwrap();
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
    reply.expect(200);
    let resource = reply.json_ok();
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
    ] {
        assert_eq!(resource[key], metadata[key], "{key}");
    }
    // The fixture keeps its microseconds; resource times render with milliseconds.
    assert_eq!(resource["timeCreated"], "2001-02-03T04:05:06.123Z");
    assert_eq!(resource["updated"], "2002-03-04T05:06:07.654Z");
    assert_eq!(resource["size"], "13");
    assert!(resource["md5Hash"].is_string());
    assert!(resource["crc32c"].is_string());
    server.fixture(metadata, b"replacement", "").expect(409);
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
        .json_ok();
    assert_eq!(next["generation"], "8000000000000000001");
    assert_eq!(next["timeCreated"], "1999-01-01T00:00:00.000Z");
    drop(server);
    let server = Server::start(&dir, &options);
    assert_eq!(server.request("GET", path, "", b"").json_ok(), resource);
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
    let original = server.request("GET", OBJECT, "", b"").json_ok();
    let listing = server
        .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
        .json_ok();
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
        server
            .fixture(metadata, b"rejected", "")
            .expect_with(400, format_args!("{field}"));
    }
    server.fixture(base.clone(), &[1; 17], "").expect(413);
    let wire = format!(
        "--fixture\r\nContent-Type: application/json\r\n\r\n{base}\r\n--fixture\r\nContent-Type: application/octet-stream\r\n\r\ninvalid\r\n--fixture--broken"
    );
    server.fixture_wire(wire.as_bytes(), "").expect(400);
    assert_eq!(server.request("GET", OBJECT, "", b"").json_ok(), original);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o", "", b"")
            .json_ok(),
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
        .json_ok();
    let connection = rusqlite::Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    let count = || common::catalog::count(&connection, "notification_outbox");
    assert_eq!(count(), 0);
    server
        .fixture(
            json!({"bucket":"test-bucket","name":"value","notificationMode":"suppress"}),
            b"initial",
            "",
        )
        .json_ok();
    assert_eq!(count(), 0);
    server
        .fixture(
            json!({"bucket":"test-bucket","name":"value"}),
            b"ordinary",
            "",
        )
        .json_ok();
    assert_eq!(count(), 2);
    server
        .fixture(
            json!({"bucket":"test-bucket","name":"value","notificationMode":"invalid"}),
            b"rejected",
            "",
        )
        .expect(400);
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
        .json_ok();
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
        .json_ok();
    let original = server.fixture(json!({"bucket":"retained-fixtures", "name":"value", "generation":"1", "created":"2000-01-01T00:00:00Z"}), b"original", "").json_ok();
    let listing = server
        .request("GET", "/storage/v1/b/retained-fixtures/o", "", b"")
        .json_ok();
    let rejected = server.fixture(json!({"bucket":"retained-fixtures", "name":"value", "generation":"8000000000000000000", "created":"9999-12-31T23:59:59Z"}), b"rejected", "");
    rejected.expect(400);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/retained-fixtures/o/value", "", b"")
            .json_ok(),
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
            .json_ok(),
        listing
    );
    let next = server
        .fixture(
            json!({"bucket":"retained-fixtures", "name":"next"}),
            b"valid",
            "",
        )
        .json_ok();
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
        .json_ok();
    server
        .fixture(
            json!({"bucket":"future-fixtures", "name":"value", "created":"9999-12-31T23:59:59Z"}),
            b"valid",
            "",
        )
        .json_ok();
    server
        .request(
            "PATCH",
            "/storage/v1/b/future-fixtures",
            "",
            br#"{"retentionPolicy":{"retentionPeriod":"60"}}"#,
        )
        .expect(400);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/future-fixtures", "", b"")
            .json_ok(),
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
    server.fixture(json!({}), b"bytes", "").expect(404);
    drop(server);
    let registry = common::process::write_identities(
        &dir.0,
        &json!({"identities":[
            {"id":"manager@example.invalid", "tokens":["manager"], "project_roles":{}},
            {"id":"cloud-admin@example.invalid", "tokens":["cloud-admin"], "project_roles":{"local":["roles/storage.admin"]}}
        ]}),
    );
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
    server
        .request(
            "POST",
            "/storage/v1/b?project=local",
            "Authorization: Bearer cloud-admin\r\n",
            br#"{"name":"test-bucket"}"#,
        )
        .expect(200);
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
    server
        .fixture(fixture, b"fixture", "Authorization: Bearer manager\r\n")
        .expect(200);
    server
        .request(
            "POST",
            "/upload/storage/v1/b/test-bucket/o?uploadType=media&name=forbidden",
            "Authorization: Bearer manager\r\n",
            b"denied",
        )
        .expect(403);
}

#[test]
fn direct_fixture_progress_renews_inactivity_without_disabling_the_deadline() {
    let dir = TestDir::new();
    let server = Server::start(
        &dir,
        &[
            "--admin-enabled",
            "true",
            "--body-inactivity-timeout-ms",
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
    let response = common::read_http_response(&mut BufReader::new(stream), Body::Expected).unwrap();
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
        .json_ok();
    let copy_path = "/storage/v1/b/test-bucket/o/value/copyTo/b/test-bucket/o/copied";
    let copy_headers = "X-Goog-Gcs-Idempotency-Token: copy-token\r\n";
    let first_copy = server
        .request("POST", copy_path, copy_headers, b"null")
        .json_ok();
    let multipart = || {
        let body = b"--boundary\r\nContent-Type: application/json\r\n\r\n{\"name\":\"multipart-replay\"}\r\n--boundary\r\nContent-Type: text/plain\r\n\r\noriginal\r\n--boundary--\r\n";
        let mut stream = TcpStream::connect(&server.address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream, "POST /upload/storage/v1/b/test-bucket/o?uploadType=multipart HTTP/1.1\r\nHost: localhost\r\nContent-Type: multipart/related; boundary=boundary\r\nX-Goog-Gcs-Idempotency-Token: multipart-token\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        stream.write_all(body).unwrap();
        let response =
            common::read_http_response(&mut BufReader::new(stream), Body::Expected).unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        serde_json::from_str::<Value>(response.split_once("\r\n\r\n").unwrap().1).unwrap()
    };
    let first_multipart = multipart();
    server
        .configure("http://changed.invalid", "public.invalid")
        .expect(200);
    let second_media = server
        .request("POST", media_path, media_headers, b"original")
        .json_ok();
    let second_copy = server
        .request("POST", copy_path, copy_headers, b"null")
        .json_ok();
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
        .json_ok();
    assert_eq!(current["generation"], first_media["generation"]);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/copied", "", b"")
            .json_ok()["generation"],
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
            .json_ok()["generation"],
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
        health.expect(200);
        let head = server.request("HEAD", "/_internal/healthcheck", "", b"");
        head.expect(200);
        assert!(head.body.is_empty());
        server
            .request("POST", "/_internal/healthcheck", "", b"")
            .expect(405);
        server
            .request("GET", "/_internal/healthcheck", "", b"body")
            .expect(413);
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
        server
            .configure("http://api.invalid", "new.invalid")
            .expect(200);
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
        server
            .configure("http://mapped.invalid:9876", "public.invalid")
            .expect(404);
    }
    let server = Server::start(&dir, &["--admin-enabled", "true"]);
    server.populate();
    let original = server.request("GET", OBJECT, "", b"").json_ok();
    server
        .configure("http://mapped.invalid:9876/proxy/", "public.invalid:9876")
        .expect(200);
    let changed = server.request("GET", OBJECT, "", b"").json_ok();
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
        .json_ok();
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
        server
            .request("PUT", "/_internal/config", "", body)
            .expect(400);
        assert_eq!(server.request("GET", OBJECT, "", b"").json_ok(), changed);
    }
    server
        .request("PUT", "/_internal/config?externalUrl=ignored", "", b"{}")
        .expect(400);
    server
        .request("POST", "/_internal/config", "", b"{}")
        .expect(405);
    server
        .request(
            "PUT",
            "/_internal/config",
            "",
            &vec![b' '; 16 * 1024 * 1024 + 1],
        )
        .expect(413);
    server
        .request(
            "PUT",
            "/_internal/config",
            "",
            br#"{"externalUrl":"","publicHost":"","ignored":true}"#,
        )
        .expect(200);
    assert_eq!(server.request("GET", OBJECT, "", b"").json_ok(), changed);
    drop(server);
    let restarted = Server::start(&dir, &["--admin-enabled", "true"]);
    let resource = restarted.request("GET", OBJECT, "", b"").json_ok();
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
    old.expect(200);
    server
        .configure(
            &format!(
                "http://localhost:{}",
                server.address.rsplit_once(':').unwrap().1
            ),
            "public.invalid",
        )
        .expect(200);
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
        complete.expect(200);
        assert!(
            complete.json_ok()["selfLink"]
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
    for (args, original) in [
        (
            ["--admin-enabled", "true"].as_slice(),
            // `pending_media_upload` addresses this Host.
            "http://localhost/",
        ),
        (
            &[
                "--admin-enabled",
                "true",
                "--api-base-url",
                "http://before.invalid:1234",
            ],
            "http://before.invalid:1234/",
        ),
    ] {
        let dir = TestDir::new();
        let server = Server::start(&dir, args);
        server.populate();
        let mut upload = pending_media_upload(&server, "slow", b"stream".len());
        server
            .configure("http://new.invalid:9876", "public.invalid")
            .expect(200);
        upload.get_mut().write_all(b"stream").unwrap();
        let response = common::read_http_response(&mut upload, Body::Expected).unwrap();
        let response = String::from_utf8_lossy(&response);
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        let value: Value = serde_json::from_str(body).unwrap();
        let link = value["selfLink"].as_str().unwrap();
        assert!(link.starts_with(original), "{link}");
        assert!(
            server
                .request("GET", "/storage/v1/b/test-bucket/o/slow", "", b"")
                .json_ok()["selfLink"]
                .as_str()
                .unwrap()
                .starts_with("http://new.invalid:9876/")
        );
    }
}

#[test]
fn strict_config_authorization_and_raw_signed_host_survive_public_host_updates() {
    for profile in ["native", "legacy-emulator"] {
        let dir = TestDir::new();
        let identity_file = common::process::write_identities(
            &dir.0,
            &json!({"identities":[
                {"id":"admin@example.invalid","tokens":["admin"],"public_keys":[include_str!("../../fixtures/signing/test-only-public.pem")],"project_roles":{"local":["roles/storage.admin"]}},
                {"id":"reader@example.invalid","tokens":["reader"],"project_roles":{"local":["roles/storage.objectViewer"]}}
            ]}),
        );
        let server = Server::start(
            &dir,
            &[
                "--admin-enabled",
                "true",
                "--admin-principal",
                "admin@example.invalid",
                "--auth-mode",
                "strict",
                "--identity-file",
                identity_file.to_str().unwrap(),
                "--compat-profile",
                profile,
            ],
        );
        server.populate();
        server
            .request("PUT", "/_internal/config", "", b"{}")
            .expect(401);
        server
            .request(
                "PUT",
                "/_internal/config",
                "Authorization: Bearer reader\r\n",
                b"{}",
            )
            .expect(403);
        server
            .request("GET", "/_internal/healthcheck", "", b"")
            .expect(200);
        server
            .configure("http://api.invalid:9876", "localhost")
            .expect(200);
        server
            .request("GET", "/test-bucket/value", "", b"")
            .expect(401);
        let url = sign_url(SignUrlRequest {
            url: "http://localhost/test-bucket/value",
            method: &Method::GET,
            signer: "admin@example.invalid",
            private_key: include_bytes!("../../fixtures/signing/test-only-private.pem"),
            expires: 60,
            now: SystemTime::now(),
            headers: HeaderMap::new(),
        })
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
        server.request("GET", &altered, "", b"").expect(403);
    }
}

declare_server!(graceful);

/// Opens a media upload to `test-bucket` and stops after `100 Continue`, so the server holds
/// an admitted upload lease until the caller writes `length` body bytes.
fn start_media_upload(server: &Server, name: &str, length: usize) -> BufReader<TcpStream> {
    let mut upload = TcpStream::connect(&server.address).unwrap();
    upload
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    write!(upload, "POST /upload/storage/v1/b/test-bucket/o?uploadType=media&name={name} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {length}\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n").unwrap();
    BufReader::new(upload)
}

fn pending_media_upload(server: &Server, name: &str, length: usize) -> BufReader<TcpStream> {
    let mut upload = start_media_upload(server, name, length);
    read_upload_go_ahead(&mut upload);
    upload
}

fn read_upload_go_ahead(upload: &mut BufReader<TcpStream>) {
    let mut interim = String::new();
    upload.read_line(&mut interim).unwrap();
    assert!(interim.starts_with("HTTP/1.1 100"), "{interim}");
    interim.clear();
    upload.read_line(&mut interim).unwrap();
    assert_eq!(interim, "\r\n");
}

/// Starts an upload and tells whether the server is holding it back, which shows that an exclusive
/// job has reserved the catalog: the server asks for the body only after admitting the upload.
fn upload_is_held_back(mut upload: BufReader<TcpStream>) -> Option<BufReader<TcpStream>> {
    upload
        .get_ref()
        .set_read_timeout(Some(HELD_BACK_AFTER))
        .unwrap();
    let mut interim = String::new();
    match upload.read_line(&mut interim) {
        Ok(_) => {
            assert!(interim.starts_with("HTTP/1.1 100"), "{interim}");
            None
        }
        Err(error) => {
            assert!(
                matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ),
                "{error}"
            );
            upload
                .get_ref()
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            Some(upload)
        }
    }
}

/// An upload that was let through is abandoned, and the next attempt tries again.
fn held_back_media_upload(server: &Server, name: &str, length: usize) -> BufReader<TcpStream> {
    common::input_fixtures::eventually_some("an exclusive job to hold back uploads", || {
        upload_is_held_back(start_media_upload(server, name, length))
    })
}
