#![cfg(unix)]

mod common;

use common::TestDir;
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
}

impl Server {
    fn start(dir: &Path, certs: &Path, extra: &[&str]) -> Self {
        let mut command = common::server_command();
        command
            .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
            .arg(dir)
            .arg("--tls-cert")
            .arg(certs.join("server.pem"))
            .arg("--tls-key")
            .arg(certs.join("server.key"))
            .args(extra);
        Self::launch(command, "https")
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
        Self::launch(command, "http")
    }

    fn launch(mut command: Command, scheme: &str) -> Self {
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
        assert_eq!(ready["scheme"], scheme);
        let address = ready["listen"].as_str().unwrap().to_owned();
        Self {
            child,
            stdout,
            origin: format!("{scheme}://{address}"),
            address,
        }
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
        .arg(format!("{}{path}", server.origin))
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
    let server = Server::launch(command, "https");
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
    let url = gcs_mimic::auth::signature::sign_url(
        &format!("{}/signed-bucket/object", server.origin),
        &hyper::Method::GET,
        "local-viewer@example.invalid",
        include_bytes!("../fixtures/signing/test-only-private.pem"),
        60,
        std::time::SystemTime::now(),
        hyper::HeaderMap::new(),
    )
    .unwrap();
    let uri: hyper::Uri = url.parse().unwrap();
    let path = uri.path_and_query().unwrap().as_str();
    assert_eq!(success(curl(&server, &ca, &[], path)), b"signed bytes");
    let denied = curl(&server, &ca, &["-H", "Host: changed.invalid:4443"], path);
    assert_eq!(denied.status.code(), Some(22));
    assert!(String::from_utf8_lossy(&denied.stderr).contains("403"));
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
        serde_json::json!([include_str!("../fixtures/signing/test-only-public.pem")]);
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
        let signed = gcs_mimic::auth::signature::sign_url(
            &url,
            &method,
            "local-admin@example.invalid",
            include_bytes!("../fixtures/signing/test-only-private.pem"),
            60,
            std::time::SystemTime::now(),
            hyper::HeaderMap::new(),
        )
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
    let embedded = Server::launch(command, "https");
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
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
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
    let server = Server::launch(command, "https");
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
            "--tls-handshake-timeout-ms",
            "5000",
        ],
    );
    let mut stalled = TcpStream::connect(&server.address).unwrap();
    stalled.write_all(&[0x16]).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let started = Instant::now();
    server.stop();
    assert!(started.elapsed() < Duration::from_secs(2));
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

#[test]
fn tls_streaming_progress_and_upload_deadlines_preserve_existing_object() {
    let dir = TestDir::new();
    let certs = dir.0.join("tls");
    fixtures(&certs);
    let ca = certs.join("ca.pem");
    let server = Server::start(
        &dir.0.join("data"),
        &certs,
        &[
            "--connection-timeout-ms",
            "300",
            "--object-request-timeout-ms",
            "900",
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
    let mut response = Vec::new();
    active.read_to_end(&mut response).unwrap();
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
    assert!(
        response.is_empty(),
        "an incomplete overwrite must not succeed"
    );
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert!(started.elapsed() < Duration::from_secs(2));
    let mut trickle = tls_stream(&server, &ca);
    trickle.write_all(request).unwrap();
    trickle.write_all(b"1\r\nx\r\n").unwrap();
    trickle.flush().unwrap();
    let started = Instant::now();
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(70));
        trickle.write_all(b"1\r\nx\r\n").unwrap();
        trickle.flush().unwrap();
    }
    let mut response = Vec::new();
    trickle.read_to_end(&mut response).unwrap();
    assert!(
        response.is_empty(),
        "the absolute deadline must prevent a trickled overwrite"
    );
    assert!(started.elapsed() >= Duration::from_millis(800));
    assert!(started.elapsed() < Duration::from_secs(2));
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
        receive.recv_timeout(Duration::from_secs(5)).unwrap();
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
    assert!(started.elapsed() < Duration::from_secs(4));
    send.send(()).unwrap();
    stalled.join().unwrap();
}
