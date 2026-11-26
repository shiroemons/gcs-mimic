#![cfg(unix)]
//! gRPC over TLS on the dedicated gRPC listener, as `grpc_scheme` selects (DESIGN-104).

use crate::common;
use bytes::Bytes;
use common::TestDir;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::{TokioExecutor, TokioIo};
use std::assert_matches;
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_rustls::rustls::{
    self,
    pki_types::{CertificateDer, ServerName, pem::PemObject},
};

/// A hang guard for steps the server finishes far sooner.
const GUARD: Duration = Duration::from_secs(10);

struct Server {
    child: Child,
    stdout: BufReader<ChildStdout>,
    ready: serde_json::Value,
    /// The primary listener's address.
    http: String,
    /// The gRPC listener's address.
    grpc: String,
}

impl Server {
    fn command(data: &Path, extra: &[&str]) -> Command {
        let mut command = common::server_command();
        command
            .args([
                "serve",
                "--listen",
                "127.0.0.1:0",
                "--grpc-listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(data)
            .args(extra);
        command
    }

    fn start(data: &Path, extra: &[&str]) -> Self {
        Self::launch(Self::command(data, extra))
    }

    fn launch(mut command: Command) -> Self {
        let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = stdout;
            let mut line = String::new();
            stdout.read_line(&mut line).unwrap();
            let _ = send.send((stdout, line));
        });
        let (stdout, line) = receive.recv_timeout(GUARD).unwrap_or_else(|error| {
            let _ = child.kill();
            let _ = child.wait();
            panic!("startup deadline: {error}");
        });
        let ready: serde_json::Value = serde_json::from_str(&line).expect("ready event");
        assert_eq!(ready["event"], "ready", "{ready}");
        let http = ready["listen"].as_str().unwrap().to_owned();
        let grpc = ready["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .find(|listener| matches!(listener["scheme"].as_str(), Some("h2c" | "h2")))
            .unwrap_or_else(|| panic!("no gRPC listener: {ready}"))["listen"]
            .as_str()
            .unwrap()
            .to_owned();
        Self {
            child,
            stdout,
            ready,
            http,
            grpc,
        }
    }

    fn grpc_listener_scheme(&self) -> &str {
        self.ready["listeners"]
            .as_array()
            .unwrap()
            .iter()
            .find(|listener| listener["listen"] == self.grpc.as_str())
            .unwrap()["scheme"]
            .as_str()
            .unwrap()
    }

    fn stop(mut self) {
        common::stop_server(&mut self.child, &mut self.stdout);
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

/// The development CA exactly as `gcs-mimic tls ca` exports it.
fn development_ca(dir: &Path) -> PathBuf {
    let output = common::server_command()
        .args(["tls", "ca"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let path = dir.join("development-ca.pem");
    fs::write(&path, output.stdout).unwrap();
    path
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

/// The ALPN protocols a test client offers.
#[derive(Clone, Copy, Debug)]
enum Offer {
    /// `h2`, as every gRPC client offers.
    H2,
    /// `http/1.1` alone, as an HTTP/1 client offers.
    Http1,
    /// No ALPN extension at all.
    Nothing,
}

fn client_config(
    ca: &Path,
    offer: Offer,
    version: &'static rustls::SupportedProtocolVersion,
) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(&fs::read(ca).unwrap()) {
        roots.add(certificate.unwrap()).unwrap();
    }
    let mut config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[version])
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    config.alpn_protocols = match offer {
        Offer::H2 => vec![b"h2".to_vec()],
        Offer::Http1 => vec![b"http/1.1".to_vec()],
        Offer::Nothing => Vec::new(),
    };
    Arc::new(config)
}

fn grpc_client(ca: &Path) -> Arc<rustls::ClientConfig> {
    client_config(ca, Offer::H2, &rustls::version::TLS13)
}

/// The TLS failure a client handshake ended with.
fn tls_error(error: &std::io::Error) -> &rustls::Error {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>())
        .unwrap_or_else(|| panic!("not a TLS failure: {error}"))
}

fn certificate_error(error: &std::io::Error) -> &rustls::CertificateError {
    match tls_error(error) {
        rustls::Error::InvalidCertificate(certificate) => certificate,
        other => panic!("not a certificate failure: {other}"),
    }
}

/// The server refused every protocol the client offered.
fn refused_protocol(error: &std::io::Error) -> bool {
    matches!(
        tls_error(error),
        rustls::Error::AlertReceived(rustls::AlertDescription::NoApplicationProtocol)
    )
}

async fn connect_tls(
    address: &str,
    name: &str,
    config: Arc<rustls::ClientConfig>,
) -> std::io::Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
    let socket = tokio::net::TcpStream::connect(address).await?;
    let name = ServerName::try_from(name.to_owned()).unwrap();
    tokio::time::timeout(
        GUARD,
        tokio_rustls::TlsConnector::from(config).connect(name, socket),
    )
    .await
    .expect("the handshake ends before the guard")
}

/// A gRPC reply: the status from the trailers, or from the headers of a trailers-only reply.
#[derive(Debug)]
struct Reply {
    status: String,
    message: Bytes,
}

/// One protobuf string or message field with a one-byte tag and a length below 128.
fn field(tag: u8, value: &[u8]) -> Vec<u8> {
    assert!(value.len() < 128);
    let mut field = vec![tag, value.len() as u8];
    field.extend_from_slice(value);
    field
}

/// A unary Storage v2 call: its method, its encoded request and the `x-goog-request-params`
/// metadata a client routes it by, without which the call is refused.
struct Call {
    method: &'static str,
    message: Vec<u8>,
    routing: String,
}

/// A Storage v2 CreateBucketRequest for `name` in `local-project`.
fn create_bucket(name: &str) -> Call {
    Call {
        method: "CreateBucket",
        message: [
            field(0x0a, b"projects/_"),
            field(0x12, &field(0x1a, b"projects/local-project")),
            field(0x1a, name.as_bytes()),
        ]
        .concat(),
        routing: "project=projects%2Flocal-project".into(),
    }
}

/// A Storage v2 GetBucketRequest for `name`.
fn get_bucket(name: &str) -> Call {
    Call {
        method: "GetBucket",
        message: field(0x0a, format!("projects/_/buckets/{name}").as_bytes()),
        routing: format!("bucket=projects%2F_%2Fbuckets%2F{name}"),
    }
}

/// Sends one unary Storage v2 call over HTTP/2 on `io`.
async fn unary(
    io: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    scheme: &str,
    call: &Call,
    token: Option<&str>,
) -> Result<Reply, hyper::Error> {
    let (mut client, connection) =
        hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(io)).await?;
    let driver = tokio::spawn(connection);
    let mut framed = vec![0];
    framed.extend_from_slice(&(call.message.len() as u32).to_be_bytes());
    framed.extend_from_slice(&call.message);
    let mut request = hyper::Request::builder()
        .method("POST")
        .uri(format!(
            "{scheme}://localhost/google.storage.v2.Storage/{}",
            call.method
        ))
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .header("x-goog-request-params", &call.routing);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = client
        .send_request(request.body(Full::new(Bytes::from(framed))).unwrap())
        .await;
    let reply = match response {
        Ok(response) => {
            assert_eq!(response.version(), hyper::Version::HTTP_2);
            let headers_status = response.headers().get("grpc-status").cloned();
            let body = response.into_body().collect().await?;
            let status = headers_status
                .or_else(|| body.trailers().and_then(|t| t.get("grpc-status").cloned()))
                .expect("grpc-status")
                .to_str()
                .unwrap()
                .to_owned();
            Ok(Reply {
                status,
                message: body.to_bytes(),
            })
        }
        Err(error) => Err(error),
    };
    drop(client);
    driver.abort();
    let _ = driver.await;
    reply
}

async fn h2c(address: &str) -> tokio::net::TcpStream {
    tokio::net::TcpStream::connect(address).await.unwrap()
}

/// A JSON API GET over plain HTTP on the primary listener; returns the status code.
fn json_status(server: &Server, path: &str, token: Option<&str>) -> u16 {
    let mut stream = TcpStream::connect(&server.http).unwrap();
    stream.set_read_timeout(Some(GUARD)).unwrap();
    let authorization = token.map_or_default(|token| format!("Authorization: Bearer {token}\r\n"));
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: localhost\r\n{authorization}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status line: {response}"))
}

/// Whether the server closes `stream` within `wait`, without sending anything.
fn closed_within(stream: &mut TcpStream, wait: Duration) -> bool {
    stream.set_read_timeout(Some(wait)).unwrap();
    let mut buffer = [0; 64];
    match stream.read(&mut buffer) {
        Ok(0) => true,
        Ok(read) => panic!("the server sent {read} bytes instead of closing"),
        Err(error) => !matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ),
    }
}

/// The default `grpc_scheme=auto` serves h2c and TLS 1.2 and 1.3 on the one gRPC port, and what
/// a call writes over TLS is the state every other protocol reads.
#[tokio::test]
async fn grpc_auto_serves_h2c_and_tls_on_one_port() {
    let dir = TestDir::new();
    let ca = development_ca(&dir.0);
    let server = Server::start(&dir.0.join("data"), &["--scheme", "http"]);
    assert_eq!(server.ready["grpc_scheme"], "auto");
    // Plaintext remains accepted, so the listener entry keeps the scheme consumers search for.
    assert_eq!(server.grpc_listener_scheme(), "h2c");
    common::process::assert_ready_fields(
        &server.ready,
        &["grpc_scheme", "data_dir", "data_dir_state"],
    );

    let tls = connect_tls(&server.grpc, "localhost", grpc_client(&ca))
        .await
        .unwrap();
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let created = unary(tls, "https", &create_bucket("over-tls"), None)
        .await
        .unwrap();
    assert_eq!(created.status, "0", "{created:?}");
    assert!(
        created
            .message
            .windows(8)
            .any(|window| window == b"over-tls")
    );
    assert_eq!(json_status(&server, "/storage/v1/b/over-tls", None), 200);

    for version in [&rustls::version::TLS12, &rustls::version::TLS13] {
        let tls = connect_tls(
            &server.grpc,
            "localhost",
            client_config(&ca, Offer::H2, version),
        )
        .await
        .unwrap();
        assert_eq!(tls.get_ref().1.protocol_version(), Some(version.version));
        let read = unary(tls, "https", &get_bucket("over-tls"), None)
            .await
            .unwrap();
        assert_eq!(read.status, "0", "{version:?}: {read:?}");
    }
    let plain = unary(
        h2c(&server.grpc).await,
        "http",
        &get_bucket("over-tls"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(plain.status, "0", "{plain:?}");
    server.stop();
}

/// A supplied certificate serves gRPC over TLS; clients that cannot verify it fail on their
/// side, and the server keeps serving the next client.
#[tokio::test]
async fn grpc_tls_provided_certificate_and_client_trust_failures() {
    let dir = TestDir::new();
    let certs = dir.0.join("tls");
    fixtures(&certs);
    let ca = certs.join("ca.pem");
    let data = dir.0.join("data");
    let start = |certificate: &str, scheme: &str| {
        let certificate = certs.join(certificate);
        Server::start(
            &data,
            &[
                "--grpc-scheme",
                scheme,
                "--tls-cert",
                certificate.to_str().unwrap(),
                "--tls-key",
                certs.join("server.key").to_str().unwrap(),
            ],
        )
    };
    let server = start("server.pem", "tls");
    assert_eq!(server.ready["grpc_scheme"], "tls");
    assert_eq!(server.grpc_listener_scheme(), "h2");
    let untrusted = connect_tls(
        &server.grpc,
        "localhost",
        grpc_client(&certs.join("other.pem")),
    )
    .await
    .unwrap_err();
    assert_eq!(
        certificate_error(&untrusted),
        &rustls::CertificateError::UnknownIssuer
    );
    let wrong_name = connect_tls(&server.grpc, "wrong.invalid", grpc_client(&ca))
        .await
        .unwrap_err();
    assert_matches!(
        certificate_error(&wrong_name),
        rustls::CertificateError::NotValidForNameContext { .. },
        "{wrong_name}"
    );
    // The fixture's SANs cover the loopback IP as well as localhost, and each failed client
    // above left the listener serving.
    for (name, bucket) in [("localhost", "by-name"), ("127.0.0.1", "by-address")] {
        let tls = connect_tls(&server.grpc, name, grpc_client(&ca))
            .await
            .unwrap();
        let reply = unary(tls, "https", &create_bucket(bucket), None)
            .await
            .unwrap();
        assert_eq!(reply.status, "0", "{name}: {reply:?}");
    }
    server.stop();

    // Under the default scheme a plaintext call shows that the server still serves after a
    // client refused its expired certificate.
    let server = start("expired.pem", "auto");
    let expired = connect_tls(&server.grpc, "localhost", grpc_client(&ca))
        .await
        .unwrap_err();
    assert_matches!(
        certificate_error(&expired),
        rustls::CertificateError::ExpiredContext { .. },
        "{expired}"
    );
    let reply = unary(
        h2c(&server.grpc).await,
        "http",
        &get_bucket("by-name"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(reply.status, "0", "{reply:?}");
    server.stop();

    let server = start("wrong-host.pem", "tls");
    let refused = connect_tls(&server.grpc, "localhost", grpc_client(&ca))
        .await
        .unwrap_err();
    assert_matches!(
        certificate_error(&refused),
        rustls::CertificateError::NotValidForNameContext { .. },
        "{refused}"
    );
    // A client that names the certificate's host is then served.
    let tls = connect_tls(&server.grpc, "wrong.invalid", grpc_client(&ca))
        .await
        .unwrap();
    let reply = unary(tls, "https", &get_bucket("by-name"), None)
        .await
        .unwrap();
    assert_eq!(reply.status, "0", "{reply:?}");
    server.stop();
}

/// `grpc_scheme=tls` refuses plaintext clients.
#[tokio::test]
async fn grpc_scheme_tls_refuses_h2c_clients() {
    let dir = TestDir::new();
    let ca = development_ca(&dir.0);
    let server = Server::start(&dir.0.join("data"), &["--grpc-scheme", "tls"]);
    assert_eq!(server.grpc_listener_scheme(), "h2");
    let refused = tokio::time::timeout(
        GUARD,
        unary(h2c(&server.grpc).await, "http", &get_bucket("absent"), None),
    )
    .await
    .unwrap();
    assert!(refused.is_err(), "{refused:?}");
    let tls = connect_tls(&server.grpc, "localhost", grpc_client(&ca))
        .await
        .unwrap();
    let reply = unary(tls, "https", &get_bucket("absent"), None)
        .await
        .unwrap();
    assert_eq!(reply.status, "5", "{reply:?}");
    server.stop();
}

/// The gRPC listener alone uses the TLS identity when the primary listener is plain HTTP, so
/// the certificate files are accepted there and both listeners serve one catalog.
#[tokio::test]
async fn grpc_scheme_tls_serves_beside_a_plain_http_listener() {
    let dir = TestDir::new();
    let certs = dir.0.join("tls");
    fixtures(&certs);
    let server = Server::start(
        &dir.0.join("data"),
        &[
            "--scheme",
            "http",
            "--grpc-scheme",
            "tls",
            "--tls-cert",
            certs.join("server.pem").to_str().unwrap(),
            "--tls-key",
            certs.join("server.key").to_str().unwrap(),
        ],
    );
    assert_eq!(server.grpc_listener_scheme(), "h2");
    assert_eq!(json_status(&server, "/storage/v1/b/beside-http", None), 404);
    let tls = connect_tls(
        &server.grpc,
        "localhost",
        grpc_client(&certs.join("ca.pem")),
    )
    .await
    .unwrap();
    let reply = unary(tls, "https", &create_bucket("beside-http"), None)
        .await
        .unwrap();
    assert_eq!(reply.status, "0", "{reply:?}");
    assert_eq!(json_status(&server, "/storage/v1/b/beside-http", None), 200);
    server.stop();
}

/// `grpc_scheme=h2c` keeps the listener plaintext only, with the ready event's listener entry of
/// earlier releases; the only addition is the `grpc_scheme` member.
#[tokio::test]
async fn grpc_scheme_h2c_refuses_tls_clients_and_keeps_its_ready_event() {
    let dir = TestDir::new();
    let ca = development_ca(&dir.0);
    let server = Server::start(
        &dir.0.join("data"),
        &["--scheme", "http", "--grpc-scheme", "h2c"],
    );
    assert_eq!(
        server.ready["listeners"],
        serde_json::json!([
            {"listen": server.http, "scheme": "http"},
            {"listen": server.grpc, "scheme": "h2c"},
        ])
    );
    assert_eq!(server.ready["grpc_scheme"], "h2c");
    common::process::assert_ready_fields(
        &server.ready,
        &["grpc_scheme", "data_dir", "data_dir_state"],
    );
    assert!(
        connect_tls(&server.grpc, "localhost", grpc_client(&ca))
            .await
            .is_err()
    );
    let reply = unary(h2c(&server.grpc).await, "http", &get_bucket("absent"), None)
        .await
        .unwrap();
    assert_eq!(reply.status, "5", "{reply:?}");
    server.stop();
}

/// The gRPC acceptor offers only `h2`: a client that offers only `http/1.1` is refused with
/// rustls's no_application_protocol alert, and one that offers no ALPN completes the handshake
/// without a protocol, so the HTTP/2 preface decides; an HTTP/2 client then succeeds.
#[tokio::test]
async fn grpc_tls_alpn_refuses_http1_only_clients_and_accepts_clients_without_alpn() {
    let dir = TestDir::new();
    let ca = development_ca(&dir.0);
    let server = Server::start(&dir.0.join("data"), &[]);
    let http1 = connect_tls(
        &server.grpc,
        "localhost",
        client_config(&ca, Offer::Http1, &rustls::version::TLS13),
    )
    .await
    .unwrap_err();
    assert!(refused_protocol(&http1), "{http1}");
    let without = connect_tls(
        &server.grpc,
        "localhost",
        client_config(&ca, Offer::Nothing, &rustls::version::TLS13),
    )
    .await
    .unwrap();
    assert_eq!(without.get_ref().1.alpn_protocol(), None);
    let reply = unary(without, "https", &get_bucket("absent"), None)
        .await
        .unwrap();
    assert_eq!(reply.status, "5", "{reply:?}");
    server.stop();
}

/// The primary listener keeps offering only `http/1.1`, so a gRPC client that dials it over TLS
/// fails, and HTTPS JSON keeps working there.
#[tokio::test]
async fn a_grpc_tls_client_on_the_http_port_fails_and_https_keeps_working() {
    let dir = TestDir::new();
    let ca = development_ca(&dir.0);
    let server = Server::start(&dir.0.join("data"), &[]);
    let refused = connect_tls(&server.http, "localhost", grpc_client(&ca))
        .await
        .unwrap_err();
    assert!(refused_protocol(&refused), "{refused}");
    let mut https = connect_tls(
        &server.http,
        "localhost",
        client_config(&ca, Offer::Http1, &rustls::version::TLS13),
    )
    .await
    .unwrap();
    assert_eq!(https.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
    https
        .write_all(b"GET /storage/v1/b?project=local-project HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    https.read_to_end(&mut response).await.unwrap();
    assert!(
        response.starts_with(b"HTTP/1.1 200"),
        "{}",
        String::from_utf8_lossy(&response)
    );
    server.stop();
}

/// A bearer token that strict mode accepts over h2c is accepted over TLS, and an anonymous call
/// is UNAUTHENTICATED there too.
#[tokio::test]
async fn grpc_tls_carries_strict_bearer_tokens() {
    let dir = TestDir::new();
    let ca = development_ca(&dir.0);
    let server = Server::start(
        &dir.0.join("data"),
        &[
            "--auth-mode",
            "strict",
            "--identity-file",
            "config/identities.json",
        ],
    );
    let plain = unary(
        h2c(&server.grpc).await,
        "http",
        &create_bucket("strict-h2c"),
        Some("local-admin-token"),
    )
    .await
    .unwrap();
    assert_eq!(plain.status, "0", "{plain:?}");
    let tls = connect_tls(&server.grpc, "localhost", grpc_client(&ca))
        .await
        .unwrap();
    let created = unary(
        tls,
        "https",
        &create_bucket("strict-tls"),
        Some("local-admin-token"),
    )
    .await
    .unwrap();
    assert_eq!(created.status, "0", "{created:?}");
    assert_eq!(
        json_status(
            &server,
            "/storage/v1/b/strict-tls",
            Some("local-admin-token")
        ),
        200
    );
    let tls = connect_tls(&server.grpc, "localhost", grpc_client(&ca))
        .await
        .unwrap();
    let anonymous = unary(tls, "https", &get_bucket("strict-tls"), None)
        .await
        .unwrap();
    assert_eq!(anonymous.status, "16", "{anonymous:?}");
    server.stop();
}

/// Under `auto` a silent connection is closed when its wait for a first byte ends, one
/// `header_read_timeout_ms` after accept, without a byte from the server. Before detection the
/// HTTP/2 driver started at accept, so the server sent SETTINGS at once and closed only after
/// the GOAWAY grace that follows the preface deadline, twice that limit.
#[test]
fn grpc_detection_closes_a_silent_connection_within_one_header_read_timeout() {
    let dir = TestDir::new();
    let server = Server::start(
        &dir.0.join("data"),
        &[
            "--header-read-timeout-ms",
            "1500",
            "--keep-alive-timeout-ms",
            "60000",
        ],
    );
    let mut silent = TcpStream::connect(&server.grpc).unwrap();
    // Open after 1 s and closed by 2.7 s, well before the 3 s of two limits.
    assert!(!closed_within(&mut silent, Duration::from_millis(1000)));
    assert!(closed_within(&mut silent, Duration::from_millis(1700)));
    server.stop();
}

/// The HTTP/2 preface deadline counts from accept for plaintext, so a first byte that arrives
/// late does not restart it. A peer that never completes the preface is closed once the GOAWAY
/// grace after that deadline ends, here 2 s + 2 s from accept; counting from the first byte
/// would close it only at 5.4 s.
#[test]
fn grpc_preface_deadline_counts_from_accept_not_from_the_first_byte() {
    let dir = TestDir::new();
    let server = Server::start(
        &dir.0.join("data"),
        &[
            "--header-read-timeout-ms",
            "2000",
            "--keep-alive-timeout-ms",
            "60000",
        ],
    );
    let mut late = TcpStream::connect(&server.grpc).unwrap();
    assert!(!closed_within(&mut late, Duration::from_millis(1400)));
    // Part of the preface: the protocol is plaintext HTTP/2, but the handshake is incomplete.
    late.write_all(b"PRI * HTTP/2.0\r\n").unwrap();
    late.set_read_timeout(Some(GUARD)).unwrap();
    let mut settings = [0; 9];
    late.read_exact(&mut settings).unwrap();
    assert_eq!(settings[3], 4, "server SETTINGS first: {settings:?}");
    let length = u32::from_be_bytes([0, settings[0], settings[1], settings[2]]) as usize;
    late.read_exact(&mut vec![0; length]).unwrap();
    // Open at 3.3 s and closed by 4.8 s from accept.
    assert!(!closed_within(&mut late, Duration::from_millis(1900)));
    assert!(closed_within(&mut late, Duration::from_millis(1500)));
    server.stop();
}

/// A TLS client that stalls after its first byte is closed at `tls_handshake_timeout_ms`
/// counted from accept, so detection does not extend it.
#[test]
fn grpc_stalled_client_hello_closes_at_the_handshake_deadline_from_accept() {
    let dir = TestDir::new();
    let server = Server::start(
        &dir.0.join("data"),
        &[
            "--header-read-timeout-ms",
            "4000",
            "--tls-handshake-timeout-ms",
            "1500",
        ],
    );
    let mut stalled = TcpStream::connect(&server.grpc).unwrap();
    assert!(!closed_within(&mut stalled, Duration::from_millis(1100)));
    stalled.write_all(&[0x16]).unwrap();
    assert!(closed_within(&mut stalled, Duration::from_millis(1200)));
    server.stop();
}

/// A connection in its TLS handshake holds a `max_connections` slot, and the next client is
/// served once the handshake ends.
#[tokio::test]
async fn grpc_handshake_holds_the_only_connection_slot_until_it_ends() {
    let dir = TestDir::new();
    let ca = development_ca(&dir.0);
    let server = Server::start(
        &dir.0.join("data"),
        &[
            "--max-connections",
            "1",
            "--tls-handshake-timeout-ms",
            "600",
        ],
    );
    let mut stalled = tokio::net::TcpStream::connect(&server.grpc).await.unwrap();
    stalled.write_all(&[0x16]).await.unwrap();
    let address = server.grpc.clone();
    let next = connect_tls(&address, "localhost", grpc_client(&ca));
    tokio::pin!(next);
    assert!(
        tokio::time::timeout(Duration::from_millis(250), &mut next)
            .await
            .is_err(),
        "a pending handshake must occupy the only connection slot"
    );
    // The stalled handshake ends at its deadline and closes without a reply.
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(GUARD, stalled.read(&mut byte))
            .await
            .unwrap()
            .unwrap_or(0),
        0
    );
    let tls = next.await.unwrap();
    let reply = unary(tls, "https", &get_bucket("absent"), None)
        .await
        .unwrap();
    assert_eq!(reply.status, "5", "{reply:?}");
    server.stop();
}

/// SIGTERM ends a TLS handshake at once instead of waiting for its deadline: the server exits
/// within the 10 s that `stop` allows, far below the 60 s handshake limit.
#[test]
fn grpc_sigterm_during_a_tls_handshake_exits_promptly() {
    let dir = TestDir::new();
    let server = Server::start(
        &dir.0.join("data"),
        &[
            "--grpc-scheme",
            "tls",
            "--tls-handshake-timeout-ms",
            "60000",
        ],
    );
    let mut stalled = TcpStream::connect(&server.grpc).unwrap();
    stalled.write_all(&[0x16]).unwrap();
    // The handshake is under way and holds the connection open.
    assert!(!closed_within(&mut stalled, Duration::from_millis(200)));
    server.stop();
    assert!(closed_within(&mut stalled, GUARD));
}

fn encode_frame(kind: u8, flags: u8, stream: u32, data: &[u8]) -> Vec<u8> {
    let mut frame = (data.len() as u32).to_be_bytes()[1..].to_vec();
    frame.extend_from_slice(&[kind, flags]);
    frame.extend_from_slice(&stream.to_be_bytes());
    frame.extend_from_slice(data);
    frame
}

/// Reads one HTTP/2 frame: its kind, flags and payload, or `None` once the server closed.
async fn read_frame(tls: &mut (impl AsyncRead + Unpin)) -> Option<(u8, u8, Vec<u8>)> {
    let mut header = [0; 9];
    tls.read_exact(&mut header).await.ok()?;
    let length = u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize;
    let mut data = vec![0; length];
    tls.read_exact(&mut data).await.ok()?;
    Some((header[3], header[4], data))
}

/// Acknowledges SETTINGS and PING, so the connection stays healthy while a test waits.
async fn acknowledge(tls: &mut (impl AsyncWrite + Unpin), kind: u8, flags: u8, data: &[u8]) {
    let reply = match (kind, flags & 1) {
        (4, 0) => encode_frame(4, 1, 0, &[]),
        (6, 0) => encode_frame(6, 1, 0, data),
        _ => return,
    };
    // The server may already have closed after GOAWAY.
    let _ = tls.write_all(&reply).await;
}

/// SIGTERM sends GOAWAY on a TLS connection with an open stream and exits within the grace.
#[tokio::test]
async fn grpc_sigterm_with_an_open_tls_stream_sends_goaway_within_the_grace() {
    let dir = TestDir::new();
    let ca = development_ca(&dir.0);
    let mut server = Server::start(
        &dir.0.join("data"),
        &[
            "--grpc-scheme",
            "tls",
            "--header-read-timeout-ms",
            "1000",
            "--shutdown-timeout-ms",
            "4000",
        ],
    );
    let mut tls = connect_tls(&server.grpc, "localhost", grpc_client(&ca))
        .await
        .unwrap();
    tls.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
        .await
        .unwrap();
    tls.write_all(&encode_frame(4, 0, 0, &[])).await.unwrap();
    // :method POST, :scheme https, then :path, content-type and the routing metadata as
    // literals without indexing. The request has no END_STREAM, so the WriteObject call, which
    // would be refused at once without routing metadata, waits for its first message.
    let path = b"/google.storage.v2.Storage/WriteObject";
    let mut headers = vec![0x83, 0x87, 0x04, path.len() as u8];
    headers.extend_from_slice(path);
    headers.extend_from_slice(&[0x0f, 0x10, 16]);
    headers.extend_from_slice(b"application/grpc");
    let (name, value) = (
        b"x-goog-request-params",
        b"bucket=projects%2F_%2Fbuckets%2Fopen-stream",
    );
    headers.extend_from_slice(&[0x00, name.len() as u8]);
    headers.extend_from_slice(name);
    headers.push(value.len() as u8);
    headers.extend_from_slice(value);
    tls.write_all(&encode_frame(1, 4, 1, &headers))
        .await
        .unwrap();
    // HTTP/2 handles frames in order, so the PING's acknowledgement shows the stream is open.
    tls.write_all(&encode_frame(6, 0, 0, b"opened!!"))
        .await
        .unwrap();
    tls.flush().await.unwrap();
    tokio::time::timeout(GUARD, async {
        while let Some((kind, flags, data)) = read_frame(&mut tls).await {
            if (kind, flags, data.as_slice()) == (6, 1, b"opened!!".as_slice()) {
                return;
            }
            acknowledge(&mut tls, kind, flags, &data).await;
        }
        panic!("the server closed before the stream opened");
    })
    .await
    .unwrap();
    assert!(
        Command::new("kill")
            .args(["-TERM", &server.child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    // The grace is the lesser of header_read_timeout_ms and half of shutdown_timeout_ms, 1 s
    // here, so the connection ends well within 2.5 s of the signal.
    let mut goaway = false;
    tokio::time::timeout(Duration::from_millis(2500), async {
        while let Some((kind, flags, data)) = read_frame(&mut tls).await {
            goaway |= kind == 7;
            acknowledge(&mut tls, kind, flags, &data).await;
        }
    })
    .await
    .expect("the open stream's connection outlived its grace");
    assert!(
        goaway,
        "the open stream's connection got GOAWAY before it closed"
    );
    common::eventually("the server to exit", || {
        server.child.try_wait().unwrap().is_some()
    });
    let status = server.child.wait().unwrap();
    assert!(status.success(), "{status}");
    let mut line = String::new();
    server.stdout.read_line(&mut line).unwrap();
    assert!(line.contains("\"stopped\""), "{line}");
}

/// The gRPC listener loads the TLS identity even when the primary listener is plain HTTP, so an
/// unusable identity fails startup before the ready event and before the data directory exists.
#[test]
fn grpc_tls_invalid_identity_fails_before_readiness() {
    let dir = TestDir::new();
    let empty = dir.0.join("empty.pem");
    fs::write(&empty, b"").unwrap();
    let data = dir.0.join("data");
    let output = Server::command(
        &data,
        &[
            "--scheme",
            "http",
            "--grpc-scheme",
            "tls",
            "--tls-cert",
            empty.to_str().unwrap(),
            "--tls-key",
            empty.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("tls_cert"), "{stderr}");
    assert!(!data.exists());
    // Without the gRPC listener's TLS nothing uses the files, which startup refuses as before.
    let output = Server::command(
        &data,
        &[
            "--scheme",
            "http",
            "--grpc-scheme",
            "h2c",
            "--tls-cert",
            empty.to_str().unwrap(),
            "--tls-key",
            empty.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(!data.exists());
}
