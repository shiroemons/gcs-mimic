//! The trusted listener serves the application's own servers: a request there without
//! credentials acts as `trusted_principal`, and every other request authenticates and is
//! authorized exactly as on the browser-facing listener.

use crate::common;
use common::TestDir;
use common::process::{Reply, http_command, request_head, write_identities};
use serde_json::{Value, json};
use std::{
    io::Write,
    net::{SocketAddr, TcpStream},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const APP: &str = "app@example.invalid";
/// The name application containers reach the trusted listener by on their private network.
const TRUSTED_HOST: &str = "gcs:4444";
const JSON: &str = "Content-Type: application/json\r\n";
const VIEWER: &str = "Authorization: Bearer viewer-token\r\n";
const OUTSIDER: &str = "Authorization: Bearer outsider-token\r\n";
const PRIVATE: &str = "/storage/v1/b/private-bucket/o/object.txt";
/// A dotted service alias the fixture lists in `trusted_allowed_hosts`.
const ALIAS: &str = "storage.compose.example";

fn identities() -> Value {
    json!({"identities": [
        {"id": APP, "tokens": ["app-token"],
         "project_roles": {"project-a": ["roles/storage.admin"]}},
        {"id": "viewer@example.invalid", "tokens": ["viewer-token"],
         "project_roles": {"project-a": ["roles/storage.objectViewer"]}},
        {"id": "outsider@example.invalid", "tokens": ["outsider-token"]}
    ]})
}

fn strict_args(identities: &std::path::Path) -> Vec<String> {
    let identities = identities.to_str().unwrap().to_owned();
    ["--auth-mode", "strict", "--identity-file"]
        .into_iter()
        .map(str::to_owned)
        .chain([identities])
        .collect()
}

struct Fixture {
    server: common::process::Server,
    trusted: String,
    _config: TestDir,
    _data: TestDir,
}

impl Fixture {
    /// A strict server whose trusted principal is also the instance administrator.
    fn start() -> Self {
        let config = TestDir::new();
        let data = TestDir::new();
        let identities = write_identities(&config.0, &identities());
        let server = common::process::Server::launch(
            http_command(&data.0)
                .args(strict_args(&identities))
                .args([
                    "--trusted-listen",
                    "127.0.0.1:0",
                    "--trusted-principal",
                    APP,
                    "--trusted-allowed-hosts",
                    ALIAS,
                ])
                .args(["--admin-enabled", "true", "--admin-principal", APP]),
        );
        let trusted = server.ready["trusted_listen"].as_str().unwrap().to_owned();
        Self {
            server,
            trusted,
            _config: config,
            _data: data,
        }
    }

    fn send(
        address: &str,
        host: &str,
        method: &str,
        path: &str,
        headers: &str,
        body: &[u8],
    ) -> Reply {
        let head = request_head(
            method,
            path,
            host,
            &format!("{headers}Connection: close\r\n"),
            body.len(),
        );
        Self::raw(address, &[head.as_bytes(), body].concat())
    }

    fn raw(address: &str, request: &[u8]) -> Reply {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream.write_all(request).unwrap();
        Reply::framed(stream)
    }

    /// A request to the trusted listener's socket that names `host`.
    fn trusted_as(&self, host: &str, method: &str, path: &str, headers: &str) -> Reply {
        Self::send(&self.trusted, host, method, path, headers, b"")
    }

    /// A request to the browser-facing listener.
    fn main(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        let address = &self.server.address;
        Self::send(address, address, method, path, headers, body)
    }

    /// A request from an application container to the trusted listener.
    fn trusted(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        Self::send(&self.trusted, TRUSTED_HOST, method, path, headers, body)
    }

    /// Creates a bucket and one object through the trusted listener without credentials.
    fn bucket_with_object(&self, bucket: &str, bytes: &[u8]) {
        let resource = json!({"name": bucket}).to_string();
        self.trusted(
            "POST",
            "/storage/v1/b?project=project-a",
            JSON,
            resource.as_bytes(),
        )
        .expect(200);
        self.trusted(
            "POST",
            &format!("/upload/storage/v1/b/{bucket}/o?uploadType=media&name=object.txt"),
            "",
            bytes,
        )
        .expect(200);
    }
}

fn part_statuses(reply: &Reply) -> Vec<String> {
    String::from_utf8_lossy(&reply.body)
        .lines()
        .filter(|line| line.starts_with("HTTP/1.1 "))
        .map(str::to_owned)
        .collect()
}

#[test]
fn requests_without_credentials_act_as_the_trusted_principal_only_on_the_trusted_listener() {
    let fixture = Fixture::start();
    let ready = &fixture.server.ready;
    assert_eq!(ready["trusted_principal"], APP, "{ready}");
    let trusted: SocketAddr = fixture.trusted.parse().unwrap();
    assert_ne!(trusted.to_string(), fixture.server.address);
    // The listeners list keeps the browser-facing listener only.
    assert_eq!(ready["listeners"].as_array().unwrap().len(), 1, "{ready}");

    let bucket = br#"{"name":"private-bucket"}"#;
    fixture
        .main("POST", "/storage/v1/b?project=project-a", JSON, bucket)
        .expect(401);
    fixture
        .trusted("POST", "/storage/v1/b?project=project-a", JSON, bucket)
        .expect(200);
    let media = "/upload/storage/v1/b/private-bucket/o?uploadType=media&projection=full";
    fixture
        .main(
            "POST",
            &format!("{media}&name=object.txt"),
            "",
            b"from the app",
        )
        .expect(401);
    let anonymous = fixture
        .trusted(
            "POST",
            &format!("{media}&name=object.txt"),
            "",
            b"from the app",
        )
        .json_ok();
    // The object records the same owner as an upload that presents the principal's token.
    let presented = fixture
        .main(
            "POST",
            &format!("{media}&name=token.txt"),
            "Authorization: Bearer app-token\r\n",
            b"with a token",
        )
        .json_ok();
    assert!(anonymous["owner"].is_object(), "{anonymous}");
    assert_eq!(anonymous["owner"], presented["owner"]);
    let grants = |object: &Value| {
        object["acl"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| (entry["entity"].clone(), entry["role"].clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(grants(&anonymous), grants(&presented));
    assert!(
        grants(&anonymous).contains(&(json!(format!("user-{APP}")), json!("OWNER"))),
        "{anonymous}"
    );

    fixture
        .main("GET", &format!("{PRIVATE}?alt=media"), "", b"")
        .expect(401);
    let read = fixture.trusted("GET", &format!("{PRIVATE}?alt=media"), "", b"");
    read.expect(200);
    assert_eq!(read.body, b"from the app");
    fixture
        .main("GET", "/private-bucket/object.txt", "", b"")
        .expect(401);
    let read = fixture.trusted("GET", "/private-bucket/object.txt", "", b"");
    read.expect(200);
    assert_eq!(read.body, b"from the app");
    fixture
        .main("PUT", "/private-bucket/xml.txt", "", b"xml bytes")
        .expect(401);
    fixture
        .trusted("PUT", "/private-bucket/xml.txt", "", b"xml bytes")
        .expect(200);

    let start = "/upload/storage/v1/b/private-bucket/o?uploadType=resumable&name=resumable.txt";
    fixture.main("POST", start, "", b"").expect(401);
    let started = fixture.trusted("POST", start, "", b"");
    started.expect(200);
    let origin = format!("http://{TRUSTED_HOST}");
    let location = &started.headers["location"];
    let session = location
        .strip_prefix(&origin)
        .unwrap_or_else(|| panic!("the session must continue on the trusted host: {location}"));
    let finished = fixture
        .trusted("PUT", session, "Content-Range: bytes 0-4/5\r\n", b"bytes")
        .json_ok();
    assert_eq!(finished["name"], "resumable.txt");
    assert!(
        finished["mediaLink"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{origin}/")),
        "{finished}"
    );
}

#[test]
fn presented_credentials_and_public_grants_decide_alike_on_both_listeners() {
    let fixture = Fixture::start();
    fixture.bucket_with_object("private-bucket", b"private bytes");
    // A presented token acts as its own identity, never as the trusted principal.
    let read = fixture.trusted("GET", &format!("{PRIVATE}?alt=media"), VIEWER, b"");
    read.expect(200);
    assert_eq!(read.body, b"private bytes");
    fixture
        .trusted(
            "POST",
            "/storage/v1/b?project=project-a",
            &format!("{VIEWER}{JSON}"),
            br#"{"name":"viewer-bucket"}"#,
        )
        .expect(403);
    fixture.trusted("GET", PRIVATE, OUTSIDER, b"").expect(403);
    // Invalid credentials fail; they never fall back to the trusted principal.
    for headers in [
        "Authorization: Bearer unknown-token\r\n",
        "Authorization: Basic YXBwOg==\r\n",
    ] {
        fixture
            .trusted("GET", PRIVATE, headers, b"")
            .expect_with(401, headers);
    }
    fixture
        .trusted("GET", &format!("{PRIVATE}?access_token=app-token"), "", b"")
        .expect(401);
    let expires = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;
    let unsigned = fixture.trusted(
        "GET",
        &format!(
            "/private-bucket/object.txt?GoogleAccessId=unknown%40example.invalid&Expires={expires}&Signature=AAAA"
        ),
        "",
        b"",
    );
    assert_eq!(
        unsigned.status,
        403,
        "{}",
        String::from_utf8_lossy(&unsigned.body)
    );

    // A bucket granting allUsers stays readable anonymously through the browser-facing listener,
    // while the private bucket stays closed there.
    fixture.bucket_with_object("public-bucket", b"public bytes");
    fixture
        .trusted(
            "PUT",
            "/storage/v1/b/public-bucket/iam",
            JSON,
            json!({"bindings": [
                {"role": "roles/storage.objectViewer", "members": ["allUsers"]}
            ]})
            .to_string()
            .as_bytes(),
        )
        .expect(200);
    let public = fixture.main(
        "GET",
        "/storage/v1/b/public-bucket/o/object.txt?alt=media",
        "",
        b"",
    );
    public.expect(200);
    assert_eq!(public.body, b"public bytes");
    let public = fixture.main("GET", "/public-bucket/object.txt", "", b"");
    public.expect(200);
    assert_eq!(public.body, b"public bytes");
    fixture
        .main("GET", &format!("{PRIVATE}?alt=media"), "", b"")
        .expect(401);
}

#[test]
fn browser_initiated_requests_are_refused_on_the_trusted_listener_only() {
    let fixture = Fixture::start();
    fixture.bucket_with_object("public-bucket", b"public bytes");
    fixture
        .trusted(
            "PUT",
            "/storage/v1/b/public-bucket/iam",
            JSON,
            json!({"bindings": [
                {"role": "roles/storage.objectViewer", "members": ["allUsers"]}
            ]})
            .to_string()
            .as_bytes(),
        )
        .expect(200);
    let object = "/storage/v1/b/public-bucket/o/object.txt?alt=media";
    for header in [
        "Origin: http://localhost:3000\r\n",
        "Origin: null\r\n",
        "Sec-Fetch-Site: cross-site\r\n",
        "Sec-Fetch-Site: same-origin\r\n",
    ] {
        for path in [
            object,
            "/public-bucket/object.txt",
            "/_internal/healthcheck",
        ] {
            let refused = fixture.trusted("GET", path, header, b"");
            refused.expect_with(403, format_args!("{header}{path}"));
            let message = refused.json()["error"]["message"].clone();
            assert!(
                message.as_str().unwrap().contains("trusted listener"),
                "{message}"
            );
        }
        // The browser-facing listener answers browsers as before.
        let public = fixture.main("GET", object, header, b"");
        public.expect_with(200, header);
        assert_eq!(public.body, b"public bytes");
    }
    // A URL the developer typed sends only `Sec-Fetch-Site: none`, as a local tool sends nothing.
    fixture
        .trusted("GET", object, "Sec-Fetch-Site: none\r\n", b"")
        .expect(200);
}

#[test]
fn foreign_host_names_are_misdirected_on_the_trusted_listener_even_with_credentials() {
    let fixture = Fixture::start();
    fixture.bucket_with_object("private-bucket", b"private bytes");
    let port = fixture.trusted.parse::<SocketAddr>().unwrap().port();
    let read = format!("{PRIVATE}?alt=media");
    for host in [
        format!("127.0.0.1:{port}"),
        format!("[::1]:{port}"),
        format!("localhost:{port}"),
        format!("app.localhost:{port}"),
        "gcs".to_owned(),
        format!("{ALIAS}:{port}"),
    ] {
        let reply = fixture.trusted_as(&host, "GET", &read, "");
        reply.expect_with(200, &host);
        assert_eq!(reply.body, b"private bytes");
    }
    // A DNS rebinding page sends its own dotted name, whatever else the request carries.
    let rebound = format!("evil.example:{port}");
    for headers in ["", "Authorization: Bearer app-token\r\n", VIEWER] {
        let refused = fixture.trusted_as(&rebound, "GET", &read, headers);
        let error = refused.json_reason_value(421, "misdirected");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("trusted_allowed_hosts"),
            "{error}"
        );
    }
    let missing = Fixture::raw(
        &fixture.trusted,
        format!("GET {read} HTTP/1.1\r\nConnection: close\r\n\r\n").as_bytes(),
    );
    missing.json_reason(421, "misdirected");
    // The browser-facing listener keeps accepting any Host name.
    Fixture::send(
        &fixture.server.address,
        &rebound,
        "GET",
        "/_internal/healthcheck",
        "",
        b"",
    )
    .expect(200);
}

#[test]
fn trusted_responses_refuse_cross_origin_embedding_and_public_ones_are_unchanged() {
    let fixture = Fixture::start();
    fixture.bucket_with_object("private-bucket", b"private bytes");
    let header = |reply: &Reply, name: &str| reply.headers.get(name).cloned();
    let read = format!("{PRIVATE}?alt=media");
    for (reply, status) in [
        (fixture.trusted("GET", &read, "", b""), 200),
        (
            fixture.trusted("GET", "/_internal/healthcheck", "", b""),
            200,
        ),
        (
            fixture.trusted("GET", "/storage/v1/b/private-bucket/o/missing.txt", "", b""),
            404,
        ),
        (
            fixture.trusted(
                "GET",
                PRIVATE,
                "Authorization: Bearer unknown-token\r\n",
                b"",
            ),
            401,
        ),
        (fixture.trusted("GET", PRIVATE, OUTSIDER, b""), 403),
        (
            fixture.trusted("GET", PRIVATE, "Origin: http://localhost:3000\r\n", b""),
            403,
        ),
        (fixture.trusted_as("evil.example", "GET", PRIVATE, ""), 421),
    ] {
        reply.expect(status);
        assert_eq!(
            header(&reply, "cross-origin-resource-policy").as_deref(),
            Some("same-origin"),
            "{status}: {:?}",
            reply.headers
        );
        assert_eq!(
            header(&reply, "x-content-type-options").as_deref(),
            Some("nosniff"),
            "{status}: {:?}",
            reply.headers
        );
    }
    for (reply, status) in [
        (fixture.main("GET", &read, "", b""), 401),
        (fixture.main("GET", "/_internal/healthcheck", "", b""), 200),
        (
            fixture.main("GET", &read, "Authorization: Bearer app-token\r\n", b""),
            200,
        ),
    ] {
        reply.expect(status);
        for name in ["cross-origin-resource-policy", "x-content-type-options"] {
            assert_eq!(header(&reply, name), None, "{status} {name}");
        }
    }
}

#[test]
fn batch_parts_and_management_follow_the_listener_of_their_request() {
    let fixture = Fixture::start();
    fixture.bucket_with_object("private-bucket", b"private bytes");
    let envelope = "--batch\r\nContent-Type: application/http\r\nContent-Transfer-Encoding: binary\r\n\r\nGET /storage/v1/b/private-bucket/o/object.txt HTTP/1.1\r\n\r\n\r\n--batch--\r\n";
    let headers = "Content-Type: multipart/mixed; boundary=batch\r\n";
    let trusted = fixture.trusted("POST", "/batch/storage/v1", headers, envelope.as_bytes());
    trusted.expect(200);
    assert_eq!(part_statuses(&trusted), ["HTTP/1.1 200 OK"]);
    let main = fixture.main("POST", "/batch/storage/v1", headers, envelope.as_bytes());
    main.expect(200);
    assert_eq!(part_statuses(&main), ["HTTP/1.1 401 Unauthorized"]);
    // Management still requires admin_principal; here the trusted principal is that identity.
    fixture
        .trusted("PUT", "/_internal/config", JSON, b"{}")
        .expect(200);
    fixture
        .main("PUT", "/_internal/config", JSON, b"{}")
        .expect(401);
}

#[test]
fn startup_rejects_a_trusted_principal_missing_from_the_registry() {
    let config = TestDir::new();
    let identities = write_identities(&config.0, &identities());
    let data = config.0.join("not-created");
    let output = http_command(&data)
        .args(strict_args(&identities))
        .args([
            "--trusted-listen",
            "127.0.0.1:0",
            "--trusted-principal",
            "missing@example.invalid",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!data.exists());
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(
        text.contains(
            r#"trusted_principal \"missing@example.invalid\" is not an id in identity_file"#
        ),
        "{text}"
    );
}
