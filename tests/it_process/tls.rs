#![cfg(unix)]

use crate::common;
use common::http::Body;

use common::{TestDir, closed_by_server};
use gcs_mimic::auth::signature::{SignUrlRequest, sign_url};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    process::{Child, ChildStdout, Command, Output, Stdio},
    time::{Duration, Instant},
};

struct Server {
    child: Child,
    stdout: BufReader<ChildStdout>,
    origin: String,
    address: String,
    ready: serde_json::Value,
}

/// The scheme a test starts the server with.
#[derive(Clone, Copy)]
enum Mode {
    Http,
    Https,
    /// The default: both protocols on one port. These tests address it over TLS unless they
    /// build a plain HTTP origin from `Server::address` themselves.
    Auto,
}

impl Mode {
    fn ready_scheme(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Auto => "auto",
        }
    }

    fn origin_scheme(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https | Self::Auto => "https",
        }
    }
}

impl Server {
    /// A TLS-only listener with the fixture certificate. Supplying the files alone would also
    /// serve plain HTTP.
    fn start(dir: &Path, certs: &Path, extra: &[&str]) -> Self {
        let mut command = common::server_command();
        command
            .args(["serve", "--scheme", "https", "--listen", "127.0.0.1:0"])
            .arg("--data-dir")
            .arg(dir)
            .arg("--tls-cert")
            .arg(certs.join("server.pem"))
            .arg("--tls-key")
            .arg(certs.join("server.key"))
            .args(extra);
        Self::launch(command, Mode::Https)
    }

    fn start_http(dir: &Path) -> Self {
        let mut command = common::server_command();
        command
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(dir);
        Self::launch(command, Mode::Http)
    }

    fn launch(mut command: Command, mode: Mode) -> Self {
        let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = stdout;
            let mut line = String::new();
            stdout.read_line(&mut line).unwrap();
            let _ = send.send((stdout, line));
        });
        let (stdout, line) = receive
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|error| {
                let _ = child.kill();
                let _ = child.wait();
                panic!("TLS startup deadline: {error}");
            });
        let ready: serde_json::Value = serde_json::from_str(&line).expect("ready event");
        assert_eq!(ready["event"], "ready");
        assert_eq!(ready["scheme"], mode.ready_scheme());
        let address = ready["listen"].as_str().unwrap().to_owned();
        Self {
            child,
            stdout,
            origin: format!("{}://{address}", mode.origin_scheme()),
            address,
            ready,
        }
    }

    fn stop(mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
    }

    /// The HTTPS and plain HTTP origins of a listener that serves both, as the default does.
    fn origins(&self) -> [String; 2] {
        ["https", "http"].map(|scheme| format!("{scheme}://{}", self.address))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn fixtures(dir: &Path) {
    assert!(
        Command::new("bash")
            .arg("scripts/tls-fixtures.sh")
            .arg(dir)
            .status()
            .unwrap()
            .success()
    );
}

fn curl(server: &Server, ca: &Path, args: &[&str], path: &str) -> Output {
    curl_at(&server.origin, ca, args, path)
}

fn curl_at(origin: &str, ca: &Path, args: &[&str], path: &str) -> Output {
    Command::new("curl")
        .args([
            "-q",
            "--silent",
            "--show-error",
            "--fail",
            "--noproxy",
            "*",
            "--max-time",
            "4",
            "--cacert",
        ])
        .arg(ca)
        .args(args)
        .arg(format!("{origin}{path}"))
        .output()
        .unwrap()
}

fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// The value of a response header in curl's `--dump-header -` output.
fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

/// Whether the server closes the connection, without answering, within `wait`.
fn closed_within(stream: &mut TcpStream, wait: Duration) -> bool {
    stream.set_read_timeout(Some(wait)).unwrap();
    let mut buffer = [0; 64];
    match stream.read(&mut buffer) {
        Ok(0) => true,
        Ok(read) => panic!("the server answered {read} bytes instead of closing"),
        Err(error) => !matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
    }
}

#[test]
fn the_default_scheme_serves_http_and_https_on_one_port_and_links_follow_the_connection() {
    let dir = TestDir::new();
    let ca = dir.0.join("development-ca.pem");
    fs::write(&ca, gcs_mimic::tls::DEVELOPMENT_CA_PEM).unwrap();
    let mut command = common::server_command();
    command
        .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
        .arg(dir.0.join("data"));
    let server = Server::launch(command, Mode::Auto);
    // Without a connection to take it from, a listener entry names the scheme every client can use.
    assert_eq!(server.ready["listeners"][0]["scheme"], "http");
    assert_eq!(
        server.ready["api_base_url"],
        format!("http://{}", server.address)
    );
    let origins = server.origins();
    success(curl_at(
        &origins[0],
        &ca,
        &[
            "--header",
            "Content-Type: application/json",
            "--data",
            r#"{"name":"one-port"}"#,
        ],
        "/storage/v1/b?project=local",
    ));
    for (index, origin) in origins.iter().enumerate() {
        let name = format!("over-{index}");
        let head = success(curl_at(
            origin,
            &ca,
            &[
                "--dump-header",
                "-",
                "--output",
                "/dev/null",
                "--header",
                "Content-Type: application/json",
                "--data",
                "{}",
            ],
            &format!("/upload/storage/v1/b/one-port/o?uploadType=resumable&name={name}"),
        ));
        let head = String::from_utf8(head).unwrap();
        let session = header(&head, "location").expect("session URL");
        assert!(
            session.starts_with(&format!("{origin}/upload/storage/v1/b/one-port/o?")),
            "{session}"
        );
        let bytes = format!("sent over {origin}");
        success(
            Command::new("curl")
                .args(["-q", "--silent", "--show-error", "--fail", "--noproxy", "*"])
                .args(["--max-time", "4", "--cacert"])
                .arg(&ca)
                .args(["--request", "PUT", "--data-binary", &bytes, session])
                .output()
                .unwrap(),
        );
        // Each protocol reads what the other wrote, and resources link back to the reader's own.
        let other = &origins[1 - index];
        let path = format!("/storage/v1/b/one-port/o/{name}");
        let read = success(curl_at(other, &ca, &[], &format!("{path}?alt=media")));
        assert_eq!(read, bytes.as_bytes());
        let resource: serde_json::Value =
            serde_json::from_slice(&success(curl_at(other, &ca, &[], &path))).unwrap();
        assert_eq!(resource["selfLink"], format!("{other}{path}"));
    }
    server.stop();
}

#[test]
fn detection_waits_no_longer_than_the_header_and_handshake_limits() {
    let dir = TestDir::new();
    let mut command = common::server_command();
    command
        .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
        .arg(dir.0.join("data"))
        .args(["--header-read-timeout-ms", "4000"])
        .args(["--tls-handshake-timeout-ms", "1500"]);
    let server = Server::launch(command, Mode::Auto);
    let mut silent = TcpStream::connect(&server.address).unwrap();
    let mut late_tls = TcpStream::connect(&server.address).unwrap();
    // Before its first byte a connection has the header read limit a silent plain HTTP client
    // has, not the shorter handshake limit.
    assert!(!closed_within(&mut late_tls, Duration::from_millis(2000)));
    assert!(!closed_within(&mut silent, Duration::from_millis(500)));
    // The handshake limit counts from accept, so a TLS record arriving after it ends at once
    // instead of getting another 1.5 seconds.
    late_tls.write_all(&[0x16]).unwrap();
    assert!(closed_within(&mut late_tls, Duration::from_millis(1000)));
    assert!(closed_within(&mut silent, Duration::from_secs(10)));
    // Closing before the first byte leaves the listener serving.
    drop(TcpStream::connect(&server.address).unwrap());
    let probe = common::server_command()
        .args([
            "healthcheck",
            "--url",
            &format!("http://{}/_internal/ready", server.address),
        ])
        .output()
        .unwrap();
    assert!(probe.status.success(), "{probe:?}");
    server.stop();
}

/// The warn-level event names a server wrote to its stderr, in order.
fn warnings(stderr: impl Read) -> Vec<String> {
    BufReader::new(stderr)
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(&line.unwrap()).unwrap())
        .filter(|event| event["level"] == "warn")
        .map(|event| event["event"].as_str().unwrap().to_owned())
        .collect()
}

/// The protocol a client speaks to the default listener.
#[derive(Clone, Copy, Debug)]
enum Protocol {
    Http,
    Https,
}

const READY_REQUEST: &[u8] =
    b"GET /_internal/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";

/// Drives `step`, which must not finish within a second while every slot is held, and must
/// finish once a silent connection gives its slot up at the header read limit.
async fn held_then_done<T>(step: impl Future<Output = T>) -> T {
    tokio::pin!(step);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), &mut step)
            .await
            .is_err(),
        "served while every slot was held"
    );
    // A hang guard only.
    tokio::time::timeout(Duration::from_secs(20), step)
        .await
        .expect("served once a slot was given up")
}

async fn read_response(mut stream: impl tokio::io::AsyncRead + Unpin) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    response
}

/// Asks for readiness over `protocol` while every slot is held by a silent connection, and
/// returns the response the client gets once one is given up.
async fn served_after_a_silent_connection(address: &str, protocol: Protocol, ca: &Path) -> Vec<u8> {
    use tokio::io::AsyncWriteExt;
    let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
    match protocol {
        Protocol::Http => {
            socket.write_all(READY_REQUEST).await.unwrap();
            held_then_done(read_response(socket)).await
        }
        Protocol::Https => {
            let connector =
                tokio_rustls::TlsConnector::from(gcs_mimic::tls::client_config(ca).unwrap());
            let name = tokio_rustls::rustls::pki_types::ServerName::try_from("localhost").unwrap();
            let mut tls = held_then_done(connector.connect(name, socket))
                .await
                .unwrap();
            tls.write_all(READY_REQUEST).await.unwrap();
            read_response(tls).await
        }
    }
}

/// A connection that has sent nothing holds its slot until the header read limit, so with every
/// slot held that way the next client of either protocol waits, and is served once one ends.
#[test]
fn silent_connections_fill_the_port_and_the_next_client_is_served_when_one_times_out() {
    const SLOTS: usize = 2;
    let dir = TestDir::new();
    let ca = dir.0.join("development-ca.pem");
    fs::write(&ca, gcs_mimic::tls::DEVELOPMENT_CA_PEM).unwrap();
    let mut command = common::server_command();
    command
        .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
        .arg(dir.0.join("data"))
        .args(["--max-connections", &SLOTS.to_string()])
        .args(["--header-read-timeout-ms", "2000"])
        .stderr(Stdio::piped());
    let mut server = Server::launch(command, Mode::Auto);
    let stderr = server.child.stderr.take().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for protocol in [Protocol::Http, Protocol::Https] {
        let _silent: Vec<_> = (0..SLOTS)
            .map(|_| TcpStream::connect(&server.address).unwrap())
            .collect();
        let response = runtime.block_on(served_after_a_silent_connection(
            &server.address,
            protocol,
            &ca,
        ));
        assert!(
            response.starts_with(b"HTTP/1.1 200"),
            "{protocol:?}: {}",
            String::from_utf8_lossy(&response)
        );
    }
    // A probe that closes before its first byte is not a client fault, so it reports nothing.
    drop(TcpStream::connect(&server.address).unwrap());
    let probe = fetch(
        &format!("http://{}", server.address),
        &ca,
        &[],
        "/_internal/ready",
    );
    assert_eq!(probe.status, 200, "{probe:?}");
    server.stop();
    assert_eq!(warnings(stderr), vec!["connection_timeout"; 2 * SLOTS]);
}

/// Clients of both protocols use the one port at the same time, and each reads what the other
/// wrote.
#[test]
fn http_and_https_clients_share_one_port_concurrently() {
    const CLIENTS: usize = 8;
    const OBJECTS: usize = 10;
    let dir = TestDir::new();
    let ca = dir.0.join("development-ca.pem");
    fs::write(&ca, gcs_mimic::tls::DEVELOPMENT_CA_PEM).unwrap();
    let mut command = common::server_command();
    command
        .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
        .arg(dir.0.join("data"));
    let server = Server::launch(command, Mode::Auto);
    let origins = server.origins();
    success(curl_at(
        &origins[0],
        &ca,
        &[
            "--header",
            "Content-Type: application/json",
            "--data",
            r#"{"name":"shared-port"}"#,
        ],
        "/storage/v1/b?project=local",
    ));
    let ca = ca.as_path();
    let body = |client: usize, object: usize| format!("client {client} object {object}");
    let origin_of = |client: usize| &origins[client % origins.len()];
    std::thread::scope(|scope| {
        for client in 0..CLIENTS {
            scope.spawn(move || {
                for object in 0..OBJECTS {
                    success(curl_at(
                        origin_of(client),
                        ca,
                        &["--data-binary", &body(client, object)],
                        &format!(
                            "/upload/storage/v1/b/shared-port/o?uploadType=media&name={client}-{object}"
                        ),
                    ));
                }
            });
        }
    });
    std::thread::scope(|scope| {
        for client in 0..CLIENTS {
            // The neighbouring client wrote over the other protocol.
            let reader = (client + 1) % CLIENTS;
            scope.spawn(move || {
                for object in 0..OBJECTS {
                    let read = success(curl_at(
                        origin_of(reader),
                        ca,
                        &[],
                        &format!("/storage/v1/b/shared-port/o/{client}-{object}?alt=media"),
                    ));
                    assert_eq!(read, body(client, object).into_bytes());
                }
            });
        }
    });
    server.stop();
}

#[test]
fn signed_private_download_uses_the_https_listener_host_and_port() {
    let data = TestDir::new();
    let config = TestDir::new();
    let ca = config.0.join("ca.pem");
    fs::write(&ca, gcs_mimic::tls::DEVELOPMENT_CA_PEM).unwrap();
    let mut command = common::server_command();
    command
        .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
        .arg(&data.0)
        .args([
            "--auth-mode",
            "strict",
            "--identity-file",
            "config/identities.json",
        ]);
    let server = Server::launch(command, Mode::Auto);
    success(curl(
        &server,
        &ca,
        &[
            "-H",
            "Authorization: Bearer local-admin-token",
            "-H",
            "Content-Type: application/json",
            "--data",
            r#"{"name":"signed-bucket"}"#,
        ],
        "/storage/v1/b?project=local-project",
    ));
    success(curl(
        &server,
        &ca,
        &[
            "-H",
            "Authorization: Bearer local-admin-token",
            "--data-binary",
            "signed bytes",
        ],
        "/upload/storage/v1/b/signed-bucket/o?uploadType=media&name=object",
    ));
    let url = sign_url(SignUrlRequest {
        url: &format!("{}/signed-bucket/object", server.origin),
        method: &hyper::Method::GET,
        signer: "local-viewer@example.invalid",
        private_key: include_bytes!("../../fixtures/signing/test-only-private.pem"),
        expires: 60,
        now: std::time::SystemTime::now(),
        headers: hyper::HeaderMap::new(),
    })
    .unwrap();
    let uri: hyper::Uri = url.parse().unwrap();
    let path = uri.path_and_query().unwrap().as_str();
    assert_eq!(success(curl(&server, &ca, &[], path)), b"signed bytes");
    let denied = curl(&server, &ca, &["-H", "Host: changed.invalid:4443"], path);
    assert_eq!(denied.status.code(), Some(22));
    assert!(String::from_utf8_lossy(&denied.stderr).contains("403"));
    server.stop();
}

/// A response whose status a test inspects, which `curl_at` would hide behind `--fail`.
#[derive(Debug, PartialEq, Eq)]
struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn ok(body: &str) -> Self {
        Self {
            status: 200,
            body: body.to_owned(),
        }
    }
}

fn fetch(origin: &str, ca: &Path, args: &[&str], path: &str) -> Reply {
    let output = Command::new("curl")
        .args([
            "-q",
            "--silent",
            "--show-error",
            "--noproxy",
            "*",
            "--max-time",
            "4",
            "--write-out",
            "\n%{http_code}",
            "--cacert",
        ])
        .arg(ca)
        .args(args)
        .arg(format!("{origin}{path}"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let (body, status) = text.rsplit_once('\n').unwrap();
    Reply {
        status: status.parse().unwrap(),
        body: body.to_owned(),
    }
}

fn signed_path(url: &str, signer: &str, now: std::time::SystemTime) -> String {
    let signed = sign_url(SignUrlRequest {
        url,
        method: &hyper::Method::GET,
        signer,
        private_key: include_bytes!("../../fixtures/signing/test-only-private.pem"),
        expires: 300,
        now,
        headers: hyper::HeaderMap::new(),
    })
    .unwrap();
    let uri: hyper::Uri = signed.parse().unwrap();
    uri.path_and_query().unwrap().as_str().to_owned()
}

/// V4 signs the Host and never the scheme, so under the default scheme a URL signed for either
/// protocol verifies over both on the one port, and `signed_host_port` treats a Host without a
/// port the same way over both.
#[test]
fn a_signed_url_verifies_over_both_protocols_on_one_port() {
    let data = TestDir::new();
    let config = TestDir::new();
    let ca = config.0.join("ca.pem");
    fs::write(&ca, gcs_mimic::tls::DEVELOPMENT_CA_PEM).unwrap();
    let strict = |extra: &[&str]| {
        let mut command = common::server_command();
        command
            .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
            .arg(&data.0)
            .args([
                "--auth-mode",
                "strict",
                "--identity-file",
                "config/identities.json",
            ])
            .args(extra);
        Server::launch(command, Mode::Auto)
    };
    let server = strict(&[]);
    assert_eq!(server.ready["signed_host_port"], "exact");
    let origins = server.origins();
    let [https, http] = &origins;
    let admin = "Authorization: Bearer local-admin-token";
    success(curl_at(
        http,
        &ca,
        &[
            "-H",
            admin,
            "-H",
            "Content-Type: application/json",
            "--data",
            r#"{"name":"both-bucket"}"#,
        ],
        "/storage/v1/b?project=local-project",
    ));
    success(curl_at(
        https,
        &ca,
        &["-H", admin, "--data-binary", "signed over both"],
        "/upload/storage/v1/b/both-bucket/o?uploadType=media&name=object",
    ));
    let now = std::time::SystemTime::now();
    let viewer = "local-viewer@example.invalid";
    let stored = Reply::ok("signed over both");
    for signed_for in &origins {
        let path = signed_path(&format!("{signed_for}/both-bucket/object"), viewer, now);
        for fetched_over in &origins {
            assert_eq!(
                fetch(fetched_over, &ca, &[], &path),
                stored,
                "signed for {signed_for}, fetched over {fetched_over}"
            );
            let moved = fetch(
                fetched_over,
                &ca,
                &["-H", "Host: changed.invalid:4443"],
                &path,
            );
            assert_eq!(moved.status, 403, "{moved:?}");
            assert!(
                moved.body.contains("<Code>SignatureDoesNotMatch</Code>"),
                "{moved:?}"
            );
        }
    }
    // Go storage signs the host without its port. Exact verification refuses that over both
    // protocols and names the setting that accepts it.
    let portless = signed_path("http://127.0.0.1/both-bucket/object", viewer, now);
    for fetched_over in &origins {
        let refused = fetch(fetched_over, &ca, &[], &portless);
        assert_eq!(refused.status, 403, "{fetched_over}: {refused:?}");
        assert!(
            refused.body.contains("set signed_host_port=lenient"),
            "{refused:?}"
        );
    }
    server.stop();

    let server = strict(&["--signed-host-port", "lenient"]);
    assert_eq!(server.ready["signed_host_port"], "lenient");
    let portless = signed_path(
        "http://127.0.0.1/both-bucket/object",
        viewer,
        std::time::SystemTime::now(),
    );
    for fetched_over in &server.origins() {
        assert_eq!(
            fetch(fetched_over, &ca, &[], &portless),
            stored,
            "{fetched_over}"
        );
    }
    server.stop();
}

#[test]
fn virtual_host_https_requires_a_matching_supplied_certificate() {
    let dir = TestDir::new();
    let certs = dir.0.join("tls");
    fixtures(&certs);
    fs::write(certs.join("virtual.ext"), "basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1,DNS:dotted.bucket.storage.example.invalid\n").unwrap();
    let generated = Command::new("openssl")
        .current_dir(&certs)
        .args([
            "x509",
            "-req",
            "-in",
            "server.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-set_serial",
            "5",
            "-days",
            "2",
            "-sha256",
            "-extfile",
            "virtual.ext",
            "-out",
            "server.pem",
        ])
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let identities = dir.0.join("identities.json");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&fs::read("config/identities.json").unwrap()).unwrap();
    registry["identities"][0]["public_keys"] =
        serde_json::json!([include_str!("../../fixtures/signing/test-only-public.pem")]);
    fs::write(&identities, registry.to_string()).unwrap();
    let options = [
        "--xml-virtual-host-domains",
        "storage.example.invalid",
        "--auth-mode",
        "strict",
        "--identity-file",
        identities.to_str().unwrap(),
    ];
    let server = Server::start(&dir.0.join("custom-data"), &certs, &options);
    let ca = certs.join("ca.pem");
    success(curl(
        &server,
        &ca,
        &[
            "-H",
            "Authorization: Bearer local-admin-token",
            "-H",
            "Content-Type: application/json",
            "--data",
            r#"{"name":"dotted.bucket"}"#,
        ],
        "/storage/v1/b?project=local-project",
    ));
    let port = server.address.split_once(':').unwrap().1;
    let host = format!("dotted.bucket.storage.example.invalid:{port}");
    let connect = format!("{host}:{}", server.address);
    let url = format!("https://{host}/%C3%A9//object");
    for (method, args) in [
        (
            hyper::Method::PUT,
            vec!["--request", "PUT", "--data-binary", "virtual TLS bytes"],
        ),
        (hyper::Method::GET, vec![]),
    ] {
        let signed = sign_url(SignUrlRequest {
            url: &url,
            method: &method,
            signer: "local-admin@example.invalid",
            private_key: include_bytes!("../../fixtures/signing/test-only-private.pem"),
            expires: 60,
            now: std::time::SystemTime::now(),
            headers: hyper::HeaderMap::new(),
        })
        .unwrap();
        let output = Command::new("curl")
            .args([
                "-q",
                "--silent",
                "--show-error",
                "--fail",
                "--noproxy",
                "*",
                "--max-time",
                "4",
                "--cacert",
            ])
            .arg(&ca)
            .args(["--connect-to", &connect])
            .args(args)
            .arg(signed)
            .output()
            .unwrap();
        let body = success(output);
        if method == hyper::Method::GET {
            assert_eq!(body, b"virtual TLS bytes");
        }
    }
    server.stop();
    let mut command = common::server_command();
    command
        .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
        .arg(dir.0.join("embedded-data"))
        .args(options);
    let embedded = Server::launch(command, Mode::Auto);
    let ca = dir.0.join("embedded-ca.pem");
    fs::write(&ca, gcs_mimic::tls::DEVELOPMENT_CA_PEM).unwrap();
    let port = embedded.address.split_once(':').unwrap().1;
    let host = format!("dotted.bucket.storage.example.invalid:{port}");
    let output = Command::new("curl")
        .args([
            "-q",
            "--silent",
            "--show-error",
            "--noproxy",
            "*",
            "--max-time",
            "4",
            "--cacert",
        ])
        .arg(ca)
        .args(["--connect-to", &format!("{host}:{}", embedded.address)])
        .arg(format!("https://{host}/object"))
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(60),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    embedded.stop();
}

fn tls_stream(
    server: &Server,
    ca: &Path,
) -> tokio_rustls::rustls::StreamOwned<tokio_rustls::rustls::ClientConnection, TcpStream> {
    let socket = TcpStream::connect(&server.address).unwrap();
    // Hang guards only; server deadlines under test are far shorter.
    socket
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let connection = tokio_rustls::rustls::ClientConnection::new(
        gcs_mimic::tls::client_config(ca).unwrap(),
        tokio_rustls::rustls::pki_types::ServerName::try_from("localhost").unwrap(),
    )
    .unwrap();
    tokio_rustls::rustls::StreamOwned::new(connection, socket)
}

fn trusted_version(
    server: &Server,
    ca: &Path,
    version: &'static tokio_rustls::rustls::SupportedProtocolVersion,
) {
    use tokio_rustls::rustls::{
        self,
        pki_types::{CertificateDer, ServerName, pem::PemObject},
    };
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(&fs::read(ca).unwrap()) {
        roots.add(cert.unwrap()).unwrap();
    }
    let config = rustls::ClientConfig::builder_with_protocol_versions(&[version])
        .with_root_certificates(roots)
        .with_no_client_auth();
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let stream = tokio::net::TcpStream::connect(&server.address).await.unwrap();
        let mut stream = tokio_rustls::TlsConnector::from(std::sync::Arc::new(config))
            .connect(ServerName::try_from("localhost").unwrap(), stream).await.unwrap();
        assert_eq!(stream.get_ref().1.protocol_version(), Some(version.version));
        stream.write_all(b"GET /_internal/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").await.unwrap();
        stream.flush().await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
    });
}

#[test]
fn tls_embedded_default_startup_exports_only_ca_and_keeps_custom_trust_separate() {
    let dir = TestDir::new();
    let exported = success(
        common::server_command()
            .args(["tls", "ca"])
            .env("GCS_MIMIC_CONFIG", "missing-config.json")
            .env("GCS_MIMIC_TLS_KEY", "missing-key.pem")
            .current_dir(&dir.0)
            .output()
            .unwrap(),
    );
    assert_eq!(exported, gcs_mimic::tls::DEVELOPMENT_CA_PEM.as_bytes());
    let pem = std::str::from_utf8(&exported).unwrap();
    assert_eq!(pem.matches("-----BEGIN CERTIFICATE-----").count(), 1);
    assert!(!pem.contains("PRIVATE KEY"));
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
    let ca = dir.0.join("development-ca.pem");
    fs::write(&ca, exported).unwrap();
    let mut command = common::server_command();
    command
        .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
        .arg(dir.0.join("data"));
    let server = Server::launch(command, Mode::Auto);
    success(
        common::server_command()
            .args([
                "healthcheck",
                "--url",
                &format!("{}/_internal/ready", server.origin),
            ])
            .output()
            .unwrap(),
    );
    success(
        common::server_command()
            .args([
                "healthcheck",
                "--url",
                &format!("{}/_internal/ready", server.origin),
                "--ca-cert",
            ])
            .arg(&ca)
            .output()
            .unwrap(),
    );
    success(curl(&server, &ca, &[], "/_internal/ready"));
    let certs = dir.0.join("custom");
    fixtures(&certs);
    let replaced = common::server_command()
        .args([
            "healthcheck",
            "--url",
            &format!("{}/_internal/ready", server.origin),
            "--ca-cert",
        ])
        .arg(certs.join("ca.pem"))
        .output()
        .unwrap();
    assert!(
        !replaced.status.success(),
        "explicit custom trust must not also trust the embedded CA"
    );
    server.stop();
    let server = Server::start(&dir.0.join("data"), &certs, &[]);
    let default_trust = common::server_command()
        .args([
            "healthcheck",
            "--url",
            &format!("{}/_internal/ready", server.origin),
        ])
        .output()
        .unwrap();
    assert!(
        !default_trust.status.success(),
        "embedded trust must not silently trust a custom CA"
    );
    success(
        common::server_command()
            .args([
                "healthcheck",
                "--url",
                &format!("{}/_internal/ready", server.origin),
                "--ca-cert",
            ])
            .arg(certs.join("ca.pem"))
            .output()
            .unwrap(),
    );
    server.stop();
    for flag in ["--tls-cert", "--tls-key"] {
        let data = dir.0.join(format!("invalid-{flag}"));
        let output = common::server_command()
            .args(["serve", flag])
            .arg(certs.join(if flag == "--tls-cert" {
                "server.pem"
            } else {
                "server.key"
            }))
            .arg("--data-dir")
            .arg(&data)
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "a partial custom pair must not use embedded credentials"
        );
        assert!(output.stdout.is_empty());
        assert!(!data.exists());
    }
}

#[test]
fn tls_trust_versions_streaming_urls_and_restart() {
    let dir = TestDir::new();
    let certs = dir.0.join("tls");
    fixtures(&certs);
    let ca = certs.join("ca.pem");
    let data = dir.0.join("data");
    let server = Server::start(
        &data,
        &certs,
        &["--api-base-url", "https://localhost:9443/proxy"],
    );
    for version in [
        &tokio_rustls::rustls::version::TLS12,
        &tokio_rustls::rustls::version::TLS13,
    ] {
        trusted_version(&server, &ca, version);
    }
    success(curl(
        &server,
        &ca,
        &[
            "-H",
            "Content-Type: application/json",
            "-d",
            r#"{"name":"tls-bucket"}"#,
        ],
        "/storage/v1/b?project=tls-project",
    ));
    let bytes = vec![b'x'; 2 * 1024 * 1024 + 17];
    let input = dir.0.join("input");
    fs::write(&input, &bytes).unwrap();
    let input_arg = format!("@{}", input.display());
    let object: serde_json::Value = serde_json::from_slice(&success(curl(
        &server,
        &ca,
        &[
            "-H",
            "Host: attacker.invalid",
            "-H",
            "Forwarded: proto=http;host=attacker.invalid",
            "--data-binary",
            &input_arg,
        ],
        "/upload/storage/v1/b/tls-bucket/o?uploadType=media&name=large",
    )))
    .unwrap();
    assert!(
        object["selfLink"]
            .as_str()
            .unwrap()
            .starts_with("https://localhost:9443/proxy/")
    );
    assert_eq!(
        success(curl(
            &server,
            &ca,
            &[],
            "/storage/v1/b/tls-bucket/o/large?alt=media"
        )),
        bytes
    );
    let headers = String::from_utf8(success(curl(
        &server,
        &ca,
        &[
            "-D",
            "-",
            "-H",
            "Content-Type: application/json",
            "-d",
            r#"{"name":"resumable"}"#,
        ],
        "/upload/storage/v1/b/tls-bucket/o?uploadType=resumable",
    )))
    .unwrap();
    let location = headers
        .lines()
        .find_map(|line| line.strip_prefix("location: "))
        .unwrap();
    assert!(
        location.starts_with("https://localhost:9443/proxy/upload/"),
        "{location}"
    );
    let local_path = location
        .strip_prefix("https://localhost:9443/proxy")
        .unwrap();
    success(curl(
        &server,
        &ca,
        &[
            "-X",
            "PUT",
            "-H",
            "Content-Range: bytes 0-2/3",
            "--data-binary",
            "abc",
        ],
        local_path,
    ));
    assert_eq!(
        success(curl(
            &server,
            &ca,
            &[],
            "/storage/v1/b/tls-bucket/o/resumable?alt=media"
        )),
        b"abc"
    );
    success(
        common::server_command()
            .args([
                "healthcheck",
                "--url",
                &format!("{}/_internal/ready", server.origin),
                "--ca-cert",
            ])
            .arg(&ca)
            .output()
            .unwrap(),
    );
    server.stop();
    let server = Server::start(&data, &certs, &[]);
    assert_eq!(
        success(curl(
            &server,
            &ca,
            &[],
            "/storage/v1/b/tls-bucket/o/large?alt=media"
        )),
        bytes
    );
    server.stop();
    let server = Server::start_http(&data);
    assert_eq!(
        success(curl(
            &server,
            &ca,
            &[],
            "/storage/v1/b/tls-bucket/o/large?alt=media"
        )),
        bytes
    );
    success(
        common::server_command()
            .args([
                "healthcheck",
                "--url",
                &format!("{}/_internal/ready", server.origin),
            ])
            .output()
            .unwrap(),
    );
    let wrong_trust = common::server_command()
        .args([
            "healthcheck",
            "--url",
            &format!("{}/_internal/ready", server.origin),
            "--ca-cert",
        ])
        .arg(&ca)
        .output()
        .unwrap();
    assert!(!wrong_trust.status.success());
    server.stop();
}

#[test]
fn tls_rejects_untrusted_wrong_host_expired_and_plaintext() {
    let dir = TestDir::new();
    let certs = dir.0.join("tls");
    fixtures(&certs);
    let ca = certs.join("ca.pem");
    let server = Server::start(&dir.0.join("data"), &certs, &[]);
    assert!(
        !curl(&server, &certs.join("other.pem"), &[], "/_internal/ready")
            .status
            .success()
    );
    let probe_url = format!("{}/_internal/ready", server.origin);
    for args in [
        vec!["healthcheck", "--url", &probe_url],
        vec![
            "healthcheck",
            "--url",
            &probe_url,
            "--ca-cert",
            certs.join("other.pem").to_str().unwrap(),
        ],
    ] {
        assert!(
            !common::server_command()
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let port = server.address.split(':').next_back().unwrap();
    let wrong = Command::new("curl")
        .args([
            "-q",
            "--silent",
            "--noproxy",
            "*",
            "--max-time",
            "4",
            "--cacert",
        ])
        .arg(&ca)
        .args([
            "--resolve",
            &format!("wrong.invalid:{port}:127.0.0.1"),
            &format!("https://wrong.invalid:{port}/_internal/ready"),
        ])
        .output()
        .unwrap();
    assert_eq!(wrong.status.code(), Some(60));
    let mut plain = TcpStream::connect(&server.address).unwrap();
    plain
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    plain
        .write_all(b"GET /_internal/ready HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let mut bytes = [0; 64];
    let read = plain.read(&mut bytes).unwrap_or(0);
    assert!(!bytes[..read].starts_with(b"HTTP/1.1 200"));
    server.stop();
    fs::copy(certs.join("expired.pem"), certs.join("server.pem")).unwrap();
    let server = Server::start(&dir.0.join("data"), &certs, &[]);
    let result = curl(&server, &ca, &[], "/_internal/ready");
    assert_eq!(result.status.code(), Some(60));
    let expired_probe = common::server_command()
        .args([
            "healthcheck",
            "--url",
            &format!("{}/_internal/ready", server.origin),
            "--ca-cert",
        ])
        .arg(&ca)
        .output()
        .unwrap();
    assert!(!expired_probe.status.success());
    server.stop();
    fs::copy(certs.join("wrong-host.pem"), certs.join("server.pem")).unwrap();
    let server = Server::start(&dir.0.join("data"), &certs, &[]);
    let wrong_host_probe = common::server_command()
        .args([
            "healthcheck",
            "--url",
            &format!("{}/_internal/ready", server.origin),
            "--ca-cert",
        ])
        .arg(&ca)
        .output()
        .unwrap();
    assert!(!wrong_host_probe.status.success());
    server.stop();
}

#[test]
fn tls_handshake_admission_timeout_shutdown_and_strict_permissions() {
    let dir = TestDir::new();
    let certs = dir.0.join("tls");
    fixtures(&certs);
    let server = Server::start(
        &dir.0.join("data"),
        &certs,
        &[
            "--max-connections",
            "1",
            "--tls-handshake-timeout-ms",
            "400",
            "--auth-mode",
            "strict",
            "--identity-file",
            "config/identities.json",
        ],
    );
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut stalled = tokio::net::TcpStream::connect(&server.address)
                .await
                .unwrap();
            stalled.write_all(&[0x16]).await.unwrap();
            let started = Instant::now();
            let next = tokio::net::TcpStream::connect(&server.address)
                .await
                .unwrap();
            let connector = tokio_rustls::TlsConnector::from(
                gcs_mimic::tls::client_config(&certs.join("ca.pem")).unwrap(),
            );
            let handshake = connector.connect(
                tokio_rustls::rustls::pki_types::ServerName::try_from("localhost").unwrap(),
                next,
            );
            tokio::pin!(handshake);
            assert!(
                tokio::time::timeout(Duration::from_millis(150), &mut handshake)
                    .await
                    .is_err(),
                "a pending handshake must occupy the only connection slot"
            );
            let mut byte = [0];
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), stalled.read(&mut byte))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
            assert!(started.elapsed() >= Duration::from_millis(250));
            let mut next = tokio::time::timeout(Duration::from_secs(2), handshake)
                .await
                .unwrap()
                .unwrap();
            next.write_all(
                b"GET /_internal/ready HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
            next.flush().await.unwrap();
            let mut response = Vec::new();
            tokio::time::timeout(Duration::from_secs(2), next.read_to_end(&mut response))
                .await
                .unwrap()
                .unwrap();
            assert!(response.starts_with(b"HTTP/1.1 200"));
        });
    success(curl(
        &server,
        &certs.join("ca.pem"),
        &[],
        "/_internal/ready",
    ));
    let denied = curl(
        &server,
        &certs.join("ca.pem"),
        &["--write-out", "%{http_code}"],
        "/storage/v1/b?project=local-project",
    );
    assert_eq!(denied.status.code(), Some(22));
    assert_eq!(denied.stdout, b"401");
    success(curl(
        &server,
        &certs.join("ca.pem"),
        &[
            "-H",
            "Authorization: Bearer local-admin-token",
            "-H",
            "Content-Type: application/json",
            "-d",
            r#"{"name":"tls-strict-bucket"}"#,
        ],
        "/storage/v1/b?project=local-project",
    ));
    let forbidden = curl(
        &server,
        &certs.join("ca.pem"),
        &[
            "--write-out",
            "%{http_code}",
            "-H",
            "Authorization: Bearer local-viewer-token",
            "-H",
            "Content-Type: application/json",
            "-d",
            r#"{"name":"tls-forbidden-bucket"}"#,
        ],
        "/storage/v1/b?project=local-project",
    );
    assert_eq!(forbidden.status.code(), Some(22));
    assert_eq!(forbidden.stdout, b"403");
    success(curl(
        &server,
        &certs.join("ca.pem"),
        &["-H", "Authorization: Bearer local-admin-token"],
        "/storage/v1/b/tls-strict-bucket",
    ));
    server.stop();
    let server = Server::start(
        &dir.0.join("data"),
        &certs,
        &[
            "--max-connections",
            "1",
            // The longest accepted handshake deadline, so a stop that waited for it
            // stays far beyond the bound below even on a stalled runner.
            "--tls-handshake-timeout-ms",
            "60000",
        ],
    );
    let mut stalled = TcpStream::connect(&server.address).unwrap();
    stalled.write_all(&[0x16]).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    server.stop();
    assert!(started.elapsed() < Duration::from_secs(30));
}

#[test]
fn tls_invalid_material_never_reaches_readiness_or_creates_data() {
    let dir = TestDir::new();
    let certs = dir.0.join("tls");
    fixtures(&certs);
    fs::write(certs.join("empty.pem"), b"").unwrap();
    let key = fs::read(certs.join("server.key")).unwrap();
    fs::write(
        certs.join("duplicate.key"),
        [key.as_slice(), key.as_slice()].concat(),
    )
    .unwrap();
    fs::write(
        certs.join("malformed.pem"),
        b"-----BEGIN CERTIFICATE-----\ninvalid!\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    fs::write(
        certs.join("malformed.key"),
        b"-----BEGIN PRIVATE KEY-----\ninvalid!\n-----END PRIVATE KEY-----\n",
    )
    .unwrap();
    fs::write(certs.join("oversized.pem"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    fs::write(certs.join("oversized.key"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    for (cert, key) in [
        ("missing", "server.key"),
        ("server.pem", "missing"),
        ("empty.pem", "server.key"),
        ("server.pem", "empty.pem"),
        ("server.pem", "duplicate.key"),
        ("ca.pem", "server.key"),
        ("server.pem", "other.key"),
        ("malformed.pem", "server.key"),
        ("server.pem", "malformed.key"),
        ("oversized.pem", "server.key"),
        ("server.pem", "oversized.key"),
    ] {
        let data = dir.0.join(format!("data-{cert}-{key}"));
        let output = common::server_command()
            .args(["serve", "--scheme", "https", "--tls-cert"])
            .arg(certs.join(cert))
            .arg("--tls-key")
            .arg(certs.join(key))
            .arg("--data-dir")
            .arg(&data)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!data.exists());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("PRIVATE KEY"));
    }
}

/// Reads response bytes that have already arrived, reporting whether there were any.
/// Decrypting first keeps post-handshake session tickets from counting as a response.
fn read_arrived(
    stream: &mut tokio_rustls::rustls::StreamOwned<
        tokio_rustls::rustls::ClientConnection,
        TcpStream,
    >,
    response: &mut Vec<u8>,
) -> bool {
    stream.sock.set_nonblocking(true).unwrap();
    let mut chunk = [0; 4096];
    let read = stream.read(&mut chunk);
    stream.sock.set_nonblocking(false).unwrap();
    match read {
        Ok(read) => {
            response.extend_from_slice(&chunk[..read]);
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
        Err(error) => panic!("{error}"),
    }
}

/// An upload stopped by a deadline is answered as Cloud Storage documents, not by a silent close.
fn assert_upload_timed_out(response: &[u8]) {
    let text = String::from_utf8_lossy(response);
    assert!(text.starts_with("HTTP/1.1 408 "), "{text}");
    assert!(
        text.contains("\"reason\": \"uploadBrokenConnection\""),
        "{text}"
    );
}

/// Zero disables the handshake deadline instead of expiring it at once, and the largest value
/// must not overflow it.
#[test]
fn tls_disabled_and_largest_handshake_limits_still_serve() {
    for value in ["0", "18446744073709551615"] {
        let dir = TestDir::new();
        let certs = dir.0.join("tls");
        fixtures(&certs);
        let server = Server::start(
            &dir.0.join("data"),
            &certs,
            &["--tls-handshake-timeout-ms", value],
        );
        success(curl(
            &server,
            &certs.join("ca.pem"),
            &[],
            "/_internal/ready",
        ));
    }
}

#[test]
fn tls_streaming_progress_and_upload_deadlines_preserve_existing_object() {
    let dir = TestDir::new();
    let certs = dir.0.join("tls");
    fixtures(&certs);
    let ca = certs.join("ca.pem");
    let server = Server::start(
        &dir.0.join("data"),
        &certs,
        // The active upload needs a TLS handshake plus 480 ms of chunks inside the
        // absolute deadline even on slow CI runners; the trickle below outlasts it.
        &[
            "--body-inactivity-timeout-ms",
            "600",
            "--object-request-timeout-ms",
            "1500",
            "--max-connections",
            "1",
        ],
    );
    success(curl(
        &server,
        &ca,
        &[
            "-H",
            "Content-Type: application/json",
            "-d",
            r#"{"name":"tls-stream-bucket"}"#,
        ],
        "/storage/v1/b?project=tls-project",
    ));
    let request = b"POST /upload/storage/v1/b/tls-stream-bucket/o?uploadType=media&name=preserved HTTP/1.1\r\nHost: localhost\r\nTransfer-Encoding: chunked\r\n\r\n";
    let mut active = tls_stream(&server, &ca);
    active.write_all(request).unwrap();
    for _ in 0..6 {
        active.write_all(b"1\r\na\r\n").unwrap();
        active.flush().unwrap();
        std::thread::sleep(Duration::from_millis(80));
    }
    active.write_all(b"0\r\n\r\n").unwrap();
    active.flush().unwrap();
    let response =
        common::read_http_response(&mut BufReader::new(&mut active), Body::Expected).unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200"));
    let original = success(curl(
        &server,
        &ca,
        &[],
        "/storage/v1/b/tls-stream-bucket/o/preserved",
    ));
    let mut idle = tls_stream(&server, &ca);
    idle.write_all(b"POST /upload/storage/v1/b/tls-stream-bucket/o?uploadType=media&name=preserved HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\n\r\nx").unwrap();
    idle.flush().unwrap();
    let started = Instant::now();
    let mut response = Vec::new();
    idle.read_to_end(&mut response).unwrap();
    assert_upload_timed_out(&response);
    assert!(started.elapsed() >= Duration::from_millis(500));
    // Either server deadline (600 ms inactivity, 1500 ms absolute) may end it; the
    // bound only rules out a hang.
    assert!(started.elapsed() < Duration::from_secs(20));
    let mut trickle = tls_stream(&server, &ca);
    trickle.write_all(request).unwrap();
    trickle.write_all(b"1\r\nx\r\n").unwrap();
    trickle.flush().unwrap();
    let started = Instant::now();
    let mut response = Vec::new();
    for _ in 0..25 {
        std::thread::sleep(Duration::from_millis(70));
        // Lingering close keeps accepting writes after the timeout response, so stop
        // when it arrives, as a client reading while it sends would.
        if read_arrived(&mut trickle, &mut response) {
            break;
        }
        if let Err(error) = trickle
            .write_all(b"1\r\nx\r\n")
            .and_then(|()| trickle.flush())
        {
            assert!(closed_by_server(&error), "{error}");
            break;
        }
    }
    let answered = started.elapsed();
    trickle.read_to_end(&mut response).unwrap();
    assert_upload_timed_out(&response);
    assert!(answered >= Duration::from_millis(1400), "{answered:?}");
    assert!(answered < Duration::from_secs(3), "{answered:?}");
    assert_eq!(
        success(curl(
            &server,
            &ca,
            &[],
            "/storage/v1/b/tls-stream-bucket/o/preserved"
        )),
        original
    );
    assert_eq!(
        success(curl(
            &server,
            &ca,
            &[],
            "/storage/v1/b/tls-stream-bucket/o/preserved?alt=media"
        )),
        b"aaaaaa"
    );
    server.stop();
}

#[test]
fn tls_healthcheck_deadline_includes_stalled_handshake() {
    let dir = TestDir::new();
    let certs = dir.0.join("tls");
    fixtures(&certs);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("https://{}/_internal/ready", listener.local_addr().unwrap());
    let (send, receive) = std::sync::mpsc::channel::<()>();
    let stalled = std::thread::spawn(move || {
        let (_socket, _) = listener.accept().unwrap();
        // The peer stays silent far longer than the bound below, so only the
        // healthcheck's own deadline can end the probe in time.
        receive.recv_timeout(Duration::from_secs(60)).unwrap();
    });
    let started = Instant::now();
    let output = common::server_command()
        .args(["healthcheck", "--url", &url, "--ca-cert"])
        .arg(certs.join("ca.pem"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("probe deadline exceeded"));
    assert!(started.elapsed() >= Duration::from_millis(1700));
    assert!(started.elapsed() < Duration::from_secs(30));
    send.send(()).unwrap();
    stalled.join().unwrap();
}
