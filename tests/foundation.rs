use gcs_mimic::storage::admin::{BucketName, Origin};
use gcs_mimic::{
    clock::Clock,
    config::Config,
    server::{self, State},
    storage::Catalog,
};
mod common;
use common::{CleanEnv, TestDir};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpStream},
    process::{Child, ChildStdout, Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;

struct FixedClock;
impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(42)
    }
}

fn process_ready(child: &mut Child) -> (BufReader<ChildStdout>, serde_json::Value) {
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let _ = send.send((stdout, line));
    });
    let (stdout, line) = receive
        .recv_timeout(Duration::from_secs(75))
        .unwrap_or_else(|error| {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("server did not become ready before the restart deadline: {error}");
        });
    let ready = serde_json::from_str(&line).expect("ready on same port");
    (stdout, ready)
}

fn conflicting_process(address: &str, dir: &TestDir) -> Output {
    let mut child = common::server_command()
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            address,
            "--data-dir",
        ])
        .arg(&dir.0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!("conflicting server did not fail fast: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn assert_bind_wait_signals_exit_without_readiness(address: &str, dir: &TestDir) {
    for signal in ["-TERM", "-INT"] {
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                address,
                "--data-dir",
            ])
            .arg(&dir.0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            stderr.read_line(&mut line).unwrap();
            let _ = send.send((stderr, line));
        });
        let (_stderr, line) = match receive.recv_timeout(Duration::from_secs(3)) {
            Ok(value) => value,
            Err(error) => {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!("server did not enter bind_wait: {error}; {output:?}");
            }
        };
        assert!(
            Command::new("kill")
                .args([signal, &child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                let output = child.wait_with_output().unwrap();
                panic!("server ignored {signal} during bind_wait: {output:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        let event: serde_json::Value = serde_json::from_str(&line).expect("bind_wait event");
        assert_eq!(event["event"], "bind_wait");
        assert_eq!(output.status.code(), Some(0), "{signal}: {output:?}");
        assert_eq!(output.stdout, b"{\"event\":\"stopped\"}\n", "{signal}");
    }
}

#[test]
fn process_restarts_on_same_port_without_allowing_a_second_live_listener() {
    let dir = TestDir::new();
    let other = TestDir::new();
    let mut address = "127.0.0.1:0".to_owned();
    for restart in 0..2 {
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                &address,
                "--data-dir",
            ])
            .arg(&dir.0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let (mut stdout, ready) = process_ready(&mut child);
        assert_eq!(ready["event"], "ready");
        address = ready["listen"].as_str().unwrap().into();
        let mut health = BufReader::new(stream(address.parse().unwrap()));
        health
            .get_mut()
            .write_all(
                b"GET /_internal/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let response = common::read_http_response(&mut health, false).unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
        // TIME_WAIT stays with the side that closes first; waiting for the server's
        // close keeps it on the listener port that the bind_wait check reuses.
        assert_eq!(health.read(&mut [0; 1]).unwrap(), 0);
        let collision = conflicting_process(&address, &other);
        assert!(!collision.status.success());
        assert!(collision.stdout.is_empty());
        #[cfg(unix)]
        assert!(String::from_utf8_lossy(&collision.stderr).contains("Address already in use"));
        common::stop_server(&mut child, &mut stdout);
        // Reuse the first server's TIME_WAIT so signal coverage adds no expiry wait.
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
        if restart == 0 {
            assert_bind_wait_signals_exit_without_readiness(&address, &dir);
        }
        #[cfg(not(all(unix, not(any(target_os = "linux", target_os = "android")))))]
        let _ = restart;
    }
}

#[cfg(unix)]
#[test]
fn live_wildcard_and_specific_listeners_are_exclusive() {
    for (first, second) in [("0.0.0.0:0", "127.0.0.1"), ("127.0.0.1:0", "0.0.0.0")] {
        let dir = TestDir::new();
        let other = TestDir::new();
        let mut child = common::server_command()
            .args(["serve", "--scheme", "http", "--listen", first, "--data-dir"])
            .arg(&dir.0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let (mut stdout, ready) = process_ready(&mut child);
        let address: SocketAddr = ready["listen"].as_str().unwrap().parse().unwrap();
        let collision = conflicting_process(&format!("{second}:{}", address.port()), &other);
        common::stop_server(&mut child, &mut stdout);
        assert!(!collision.status.success());
        assert!(collision.stdout.is_empty());
        assert!(String::from_utf8_lossy(&collision.stderr).contains("Address already in use"));
    }
}

#[test]
fn process_shutdown_reports_stopped_and_releases_catalog_for_restart() {
    let dir = TestDir::new();
    for _ in 0..2 {
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(&dir.0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: serde_json::Value = serde_json::from_str(&line).expect("ready after recovery");
        assert_eq!(
            ready
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from([
                "event",
                "listen",
                "listeners",
                "protocol",
                "gcs_api",
                "max_connections",
                "max_object_bytes",
                "worker_threads",
                "max_blocking_threads",
                "connection_timeout_ms",
                "keep_alive_timeout_ms",
                "object_request_timeout_ms",
                "catalog_wait_timeout_ms",
                "shutdown_timeout_ms",
                "auth_mode",
                "seed",
                "buckets_file",
                "scheme",
                "tls_handshake_timeout_ms",
                "public_base_url",
                "public_virtual_host_enabled",
                "api_base_url",
            ])
        );
        assert_eq!(ready["event"], "ready");
        assert_eq!(ready["protocol"], "http1");
        assert_eq!(ready["gcs_api"], "json-bucket-object-media-crud");
        assert_eq!(ready["scheme"], "http");
        assert_eq!(ready["auth_mode"], "permissive");
        assert!(ready.get("identities").is_none());
        assert_eq!(ready["max_connections"], 32);
        assert_eq!(ready["max_object_bytes"], 1_073_741_824_u64);
        assert_eq!(ready["worker_threads"], 2);
        assert_eq!(ready["max_blocking_threads"], 2);
        assert_eq!(ready["connection_timeout_ms"], 5000);
        assert_eq!(ready["keep_alive_timeout_ms"], 120_000);
        assert_eq!(ready["object_request_timeout_ms"], 900_000);
        assert_eq!(ready["catalog_wait_timeout_ms"], 30_000);
        assert_eq!(ready["shutdown_timeout_ms"], 5000);
        assert_eq!(ready["tls_handshake_timeout_ms"], 5000);
        assert!(matches!(
            ready["seed"]["outcome"].as_str(),
            Some("disabled" | "skipped")
        ));
        common::stop_server(&mut child, &mut stdout);
    }
}

async fn start(
    config: Config,
) -> (
    SocketAddr,
    Arc<State>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<std::io::Result<()>>,
    TestDir,
) {
    let dir = TestDir::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = Arc::new(State {
        ready: AtomicBool::new(true),
        clock: Arc::new(FixedClock),
        catalog: Catalog::open(&dir.0).await.unwrap(),
        api_base_url: config.effective_api_base_url(address),
        default_location: config.default_location.clone(),
        auth: Default::default(),
    });
    let (stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(server::serve(listener, config, state.clone(), async {
        shutdown.await.map_err(std::io::Error::other)
    }));
    (address, state, stop, task, dir)
}

fn stream(address: SocketAddr) -> TcpStream {
    let stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
}

fn request(address: SocketAddr, request: &str) -> String {
    let mut stream = stream(address);
    stream.write_all(request.as_bytes()).unwrap();
    let response =
        common::read_http_response(&mut BufReader::new(stream), request.starts_with("HEAD "))
            .unwrap();
    String::from_utf8(response).unwrap()
}

fn dual_tls_request(
    ca: &std::path::Path,
    address: SocketAddr,
    path: &str,
    args: &[&str],
) -> String {
    let mut command = Command::new("curl");
    command.args([
        "-q",
        "--silent",
        "--show-error",
        "--fail",
        "--noproxy",
        "*",
        "--max-time",
        "4",
    ]);
    // Windows curl (Schannel) checks revocation, which a local development CA
    // cannot answer.
    if cfg!(windows) {
        command.arg("--ssl-no-revoke");
    }
    let output = command
        .arg("--cacert")
        .arg(ca)
        .args(args)
        .arg(format!("https://{address}{path}"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn dual_listener_shares_state_and_https_primary_resumable_origin() {
    for (listen, explicit) in [
        ("127.0.0.1:0", false),
        ("0.0.0.0:0", false),
        ("[::]:0", false),
        ("127.0.0.1:0", true),
    ] {
        let dir = TestDir::new();
        let ca = dir.0.join("ca.pem");
        fs::write(&ca, gcs_mimic::tls::DEVELOPMENT_CA_PEM).unwrap();
        let mut command = common::server_command();
        command
            .args([
                "serve",
                "--scheme",
                "both",
                "--listen",
                listen,
                "--listen-http",
                "127.0.0.1:0",
                "--data-dir",
            ])
            .arg(dir.0.join("data"));
        if explicit {
            command.args(["--api-base-url", "https://proxy.invalid/prefix/"]);
        }
        let mut child = command.stdout(Stdio::piped()).spawn().unwrap();
        let (mut stdout, ready) = process_ready(&mut child);
        assert_eq!(ready["scheme"], "both");
        assert_eq!(ready["listeners"].as_array().unwrap().len(), 2);
        assert_eq!(ready["listeners"][0]["listen"], ready["listen"]);
        assert_eq!(ready["listeners"][0]["scheme"], "https");
        assert_eq!(ready["listeners"][1]["scheme"], "http");
        let mut primary: SocketAddr = ready["listen"].as_str().unwrap().parse().unwrap();
        if primary.ip().is_unspecified() {
            primary.set_ip(if primary.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        let secondary: SocketAddr = ready["listeners"][1]["listen"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert_ne!(primary.port(), 0);
        assert_ne!(secondary.port(), 0);
        if primary.ip() == secondary.ip() {
            assert_ne!(primary.port(), secondary.port());
        }
        let base = if explicit {
            "https://proxy.invalid/prefix".to_owned()
        } else {
            format!("https://{primary}")
        };
        assert_eq!(ready["api_base_url"], base);
        assert!(request(secondary, "POST /storage/v1/b?project=local-project HTTP/1.1\r\nHost: attacker.invalid\r\nContent-Type: application/json\r\nContent-Length: 22\r\n\r\n{\"name\":\"dual-bucket\"}").starts_with("HTTP/1.1 200"));
        let bucket = dual_tls_request(&ca, primary, "/storage/v1/b/dual-bucket", &[]);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&bucket).unwrap()["name"],
            "dual-bucket"
        );
        let response = request(
            secondary,
            "POST /upload/storage/v1/b/dual-bucket/o?uploadType=resumable&name=file HTTP/1.1\r\nHost: attacker.invalid\r\nForwarded: host=attacker.invalid;proto=http\r\nContent-Length: 0\r\n\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let location = response
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case("location"))
                    .map(|(_, value)| value.trim())
            })
            .unwrap();
        let path = location
            .strip_prefix(&base)
            .expect("one HTTPS primary origin");
        let object = dual_tls_request(
            &ca,
            primary,
            path,
            &[
                "-X",
                "PUT",
                "-H",
                "Content-Range: bytes 0-2/3",
                "--data-binary",
                "abc",
            ],
        );
        let object: serde_json::Value = serde_json::from_str(&object).unwrap();
        for key in ["selfLink", "mediaLink"] {
            assert!(
                object[key]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("{base}/"))
            );
        }
        assert!(
            request(
                secondary,
                "GET /storage/v1/b/dual-bucket/o/file?alt=media HTTP/1.1\r\nHost: localhost\r\n\r\n"
            )
            .ends_with("abc")
        );
        common::stop_server(&mut child, &mut stdout);
        for address in [primary, secondary] {
            assert!(TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err());
        }
    }
}

#[test]
fn dual_listener_invalid_settings_fail_before_catalog_or_readiness() {
    for args in [
        vec!["--scheme", "http", "--listen-http", "127.0.0.1:0"],
        vec!["--scheme", "both", "--listen", "127.0.0.1:8000"],
        vec!["--scheme", "both", "--listen-http", "bad-address"],
    ] {
        let dir = TestDir::new();
        let data = dir.0.join("data");
        let output = common::server_command()
            .arg("serve")
            .args(args)
            .arg("--data-dir")
            .arg(&data)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!data.exists());
        assert!(String::from_utf8(output.stderr).unwrap().contains("listen"));
    }
}

#[test]
fn dual_listener_second_bind_failure_releases_first_socket_and_catalog() {
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let primary = reserved.local_addr().unwrap();
    drop(reserved);
    let dir = TestDir::new();
    let output = common::server_command()
        .args([
            "serve",
            "--scheme",
            "both",
            "--listen",
            &primary.to_string(),
            "--listen-http",
            &occupied.local_addr().unwrap().to_string(),
            "--data-dir",
        ])
        .arg(&dir.0)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let diagnostics = String::from_utf8(output.stderr).unwrap();
    assert!(diagnostics.contains("listen_http"), "{diagnostics}");
    assert!(diagnostics.contains("--listen-http"), "{diagnostics}");
    let _rebound = std::net::TcpListener::bind(primary).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog.close().await.unwrap();
    });
}

#[test]
fn dual_listener_connection_cap_counts_tls_handshakes_and_shutdown_closes_both() {
    let dir = TestDir::new();
    let mut child = common::server_command()
        .args([
            "serve",
            "--scheme",
            "both",
            "--listen",
            "127.0.0.1:0",
            "--listen-http",
            "127.0.0.1:0",
            "--max-connections",
            "1",
            "--tls-handshake-timeout-ms",
            "500",
            "--data-dir",
        ])
        .arg(&dir.0)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (mut stdout, ready) = process_ready(&mut child);
    let primary: SocketAddr = ready["listen"].as_str().unwrap().parse().unwrap();
    let secondary: SocketAddr = ready["listeners"][1]["listen"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let mut handshake = stream(primary);
    handshake.write_all(b"\x16\x03\x01\x00\x10").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let mut http = stream(secondary);
    http.write_all(b"GET /_internal/ready HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    http.set_read_timeout(Some(Duration::from_millis(60)))
        .unwrap();
    let error = http.read(&mut [0]).unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    http.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let response = common::read_http_response(&mut BufReader::new(&mut http), false).unwrap();
    let response = String::from_utf8(response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let mut handshake = stream(primary);
    handshake.write_all(b"\x16\x03\x01\x00\x10").unwrap();
    let mut queued = stream(secondary);
    queued
        .write_all(b"GET /_internal/ready HTTP/1.1\r\n")
        .unwrap();
    common::stop_server(&mut child, &mut stdout);
    for address in [primary, secondary] {
        assert!(TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err());
    }
}

#[test]
fn api_origin_ready_and_resumable_locations_use_bound_addresses() {
    for listen in ["127.0.0.1:0", "0.0.0.0:0", "[::1]:0", "[::]:0"] {
        let dir = TestDir::new();
        let mut child = common::server_command()
            .args([
                "serve",
                "--scheme",
                "http",
                "--listen",
                listen,
                "--data-dir",
            ])
            .arg(&dir.0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let (mut stdout, ready) = process_ready(&mut child);
        let mut address: SocketAddr = ready["listen"].as_str().unwrap().parse().unwrap();
        if address.ip().is_unspecified() {
            address.set_ip(if address.is_ipv4() {
                std::net::Ipv4Addr::LOCALHOST.into()
            } else {
                std::net::Ipv6Addr::LOCALHOST.into()
            });
        }
        assert_ne!(address.port(), 0);
        let base = format!("http://{address}");
        assert_eq!(ready["api_base_url"], base);
        assert!(ready["public_base_url"].is_null());
        assert_eq!(ready["public_virtual_host_enabled"], false);
        let response = request(
            address,
            "POST /storage/v1/b?project=local-project HTTP/1.1\r\nHost: attacker.invalid\r\nForwarded: host=attacker.invalid;proto=https\r\nContent-Type: application/json\r\nContent-Length: 21\r\n\r\n{\"name\":\"api-origin\"}",
        );
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let response = request(
            address,
            "POST /upload/storage/v1/b/api-origin/o?uploadType=resumable&name=file HTTP/1.1\r\nHost: attacker.invalid\r\nX-Forwarded-Host: attacker.invalid\r\nX-Forwarded-Proto: https\r\nContent-Length: 0\r\n\r\n",
        );
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let location = response
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(key, _)| key.eq_ignore_ascii_case("location"))
                    .map(|(_, value)| value.trim())
            })
            .unwrap();
        let path = location
            .strip_prefix(&base)
            .expect("Location uses ready API base");
        let response = request(
            address,
            &format!(
                "PUT {path} HTTP/1.1\r\nHost: attacker.invalid\r\nContent-Length: 3\r\nContent-Range: bytes 0-2/3\r\n\r\nabc"
            ),
        );
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let object: serde_json::Value =
            serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
        for key in ["selfLink", "mediaLink"] {
            assert!(
                object[key]
                    .as_str()
                    .unwrap()
                    .starts_with(&format!("{base}/"))
            );
        }
        common::stop_server(&mut child, &mut stdout);
    }
}

#[test]
fn api_origin_explicit_base_wins_and_conflicts_fail_before_catalog() {
    let dir = TestDir::new();
    let data = dir.0.join("catalog");
    let output = common::server_command()
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "0.0.0.0:0",
            "--xml-virtual-host-domains",
            "0.1",
            "--data-dir",
        ])
        .arg(&data)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!data.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("api_base_url host selects"));
    let mut child = common::server_command()
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "0.0.0.0:0",
            "--api-base-url",
            "https://proxy.invalid/prefix/",
            "--xml-virtual-host-domains",
            "0.1",
            "--data-dir",
        ])
        .arg(&data)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (mut stdout, ready) = process_ready(&mut child);
    assert_eq!(ready["api_base_url"], "https://proxy.invalid/prefix");
    common::stop_server(&mut child, &mut stdout);
    let mut child = common::server_command()
        .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
        .arg(&data)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (mut stdout, ready) = process_ready(&mut child);
    assert_eq!(
        ready["api_base_url"],
        format!("https://{}", ready["listen"].as_str().unwrap())
    );
    common::stop_server(&mut child, &mut stdout);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wire_probes_clock_and_unimplemented_routes() {
    let (address, state, stop, task, _dir) = start(common::http_config()).await;
    let get = |path: &str| {
        request(
            address,
            &format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n"),
        )
    };
    let health = get("/_internal/health");
    assert!(health.starts_with("HTTP/1.1 200"));
    assert!(health.contains("\"unix_time\":42"));
    assert!(get("/_internal/ready").starts_with("HTTP/1.1 200"));
    state.ready.store(false, Ordering::Release);
    assert!(get("/_internal/ready").starts_with("HTTP/1.1 503"));
    assert!(get("/_internal/health").starts_with("HTTP/1.1 200"));
    let upload = get("/upload/storage/v1/b/b/o");
    assert!(upload.starts_with("HTTP/1.1 405"));
    assert!(upload.contains("allow: POST\r\n"));
    for path in [
        "/resumable/upload/storage/v1/b/b/o",
        "/INVALID/object",
        "/google.storage.v2.Storage/ReadObject",
        "/_internal/reset",
        "/",
    ] {
        assert!(get(path).starts_with("HTTP/1.1 404"), "{path}");
    }
    let response = request(
        address,
        "POST /_internal/health HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n",
    );
    assert!(response.starts_with("HTTP/1.1 405"));
    assert!(response.contains("allow: GET, HEAD"));
    let response = request(
        address,
        "HEAD /_internal/health HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.ends_with("\r\n\r\n"));
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    assert!(!state.ready.load(Ordering::Acquire));
}

// The witness relies on a duplicated listening socket staying usable after the
// server's copy closes; Windows stops accepting on it, so connects time out.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_stops_accepting_on_original_listener() {
    let dir = TestDir::new();
    let witness = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    witness.set_nonblocking(true).unwrap();
    let address = witness.local_addr().unwrap();
    let listener = tokio::net::TcpListener::from_std(witness.try_clone().unwrap()).unwrap();
    let config = common::http_config();
    let state = Arc::new(State {
        ready: AtomicBool::new(true),
        clock: Arc::new(FixedClock),
        catalog: Catalog::open(&dir.0).await.unwrap(),
        api_base_url: config.effective_api_base_url(address),
        default_location: config.default_location.clone(),
        auth: Default::default(),
    });
    let (stop, shutdown) = oneshot::channel();
    let task = tokio::spawn(server::serve(listener, config, state.clone(), async {
        shutdown.await.map_err(std::io::Error::other)
    }));
    assert!(
        request(
            address,
            "GET /_internal/ready HTTP/1.1\r\nHost: localhost\r\n\r\n"
        )
        .starts_with("HTTP/1.1 200")
    );
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    assert!(!state.is_ready());
    // Keep this exact socket bound so another test cannot reuse its ephemeral port.
    let mut client = stream(address);
    client
        .write_all(b"GET /_internal/ready HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    let witness = tokio::net::TcpListener::from_std(witness).unwrap();
    let (pending, _) = tokio::time::timeout(Duration::from_secs(2), witness.accept())
        .await
        .expect("shutdown left the connection unaccepted")
        .unwrap();
    let mut pending = pending.into_std().unwrap();
    pending.set_nonblocking(false).unwrap();
    pending
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut received = [0; 1];
    assert_eq!(pending.read(&mut received).unwrap(), 1);
    assert_eq!(received, [b'G']);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_releases_catalog_before_return_with_state_clones_alive() {
    let (_address, state, stop, task, dir) = start(common::http_config()).await;
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    assert!(!state.is_ready());
    assert!(
        state
            .catalog
            .admin(Origin::Seed)
            .get_bucket(&BucketName::new("missing").unwrap())
            .await
            .is_err()
    );
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = common::http_config();
    let restarted = Arc::new(State {
        ready: AtomicBool::new(true),
        clock: Arc::new(FixedClock),
        catalog,
        api_base_url: config.effective_api_base_url(listener.local_addr().unwrap()),
        default_location: config.default_location.clone(),
        auth: Default::default(),
    });
    server::serve(listener, config, restarted.clone(), async { Ok(()) })
        .await
        .unwrap();
    assert!(!restarted.is_ready());
    Catalog::open(&dir.0).await.unwrap().close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejects_bodies_and_oversized_or_malformed_headers() {
    let (address, _, stop, task, _dir) = start(common::http_config()).await;
    for header in ["Content-Length: 1073741824", "Transfer-Encoding: chunked"] {
        let response = request(
            address,
            &format!("GET /_internal/ready HTTP/1.1\r\nHost: localhost\r\n{header}\r\n\r\n"),
        );
        assert!(response.starts_with("HTTP/1.1 413"));
    }
    for headers in [
        format!("X-Large: {}\r\n", "x".repeat(20_000)),
        (0..80).map(|n| format!("X-{n}: x\r\n")).collect(),
    ] {
        let mut stream = stream(address);
        stream
            .write_all(
                format!("GET /_internal/health HTTP/1.1\r\nHost: localhost\r\n{headers}\r\n")
                    .as_bytes(),
            )
            .unwrap();
        let mut status = String::new();
        // Unread oversized input can reset TCP after the rejection has been sent.
        std::io::BufReader::new(stream)
            .read_line(&mut status)
            .unwrap();
        assert!(status.starts_with("HTTP/1.1 431"));
    }
    assert!(request(address, "invalid request\r\n\r\n").starts_with("HTTP/1.1 400"));
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

/// Sends one request on a persistent connection and reads its framed response.
fn exchange(connection: &mut BufReader<TcpStream>, request: &[u8]) -> String {
    connection.get_mut().write_all(request).unwrap();
    let head = request.starts_with(b"HEAD ");
    String::from_utf8(common::read_http_response(connection, head).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn keep_alive_serves_sequential_requests_until_idle_timeout() {
    let config = Config {
        connection_timeout_ms: 500,
        keep_alive_timeout_ms: 500,
        max_object_bytes: 16,
        ..common::http_config()
    };
    let (address, _, stop, task, _dir) = start(config).await;
    let mut connection = BufReader::new(stream(address));
    let bucket = r#"{"name":"keep-alive-bucket"}"#;
    let created = exchange(&mut connection, format!("POST /storage/v1/b?project=keep-alive HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{bucket}", bucket.len()).as_bytes());
    assert!(created.starts_with("HTTP/1.1 200"), "{created}");
    let uploaded = exchange(&mut connection, b"POST /upload/storage/v1/b/keep-alive-bucket/o?uploadType=media&name=object HTTP/1.1\r\nHost: localhost\r\nContent-Length: 5\r\n\r\nhello");
    assert!(uploaded.starts_with("HTTP/1.1 200"), "{uploaded}");
    let media = exchange(
        &mut connection,
        b"GET /storage/v1/b/keep-alive-bucket/o/object?alt=media HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert!(media.starts_with("HTTP/1.1 200"), "{media}");
    assert!(media.ends_with("\r\n\r\nhello"), "{media}");
    for response in [&created, &uploaded, &media] {
        let head = response
            .split_once("\r\n\r\n")
            .unwrap()
            .0
            .to_ascii_lowercase();
        // Exact lengths keep responses framed on the reused connection.
        assert!(head.contains("\r\ncontent-length: "), "{head}");
        assert!(!head.contains("\r\nconnection: close"), "{head}");
    }
    // The keep-alive timeout closes a persistent connection left idle after a response.
    connection
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let idle = Instant::now();
    assert_eq!(connection.read(&mut [0]).unwrap(), 0);
    assert!(
        idle.elapsed() >= Duration::from_millis(400),
        "{:?}",
        idle.elapsed()
    );
    // A rejection before the body is read never parses that body as the next request.
    let mut rejected = BufReader::new(stream(address));
    let response = exchange(&mut rejected, b"POST /upload/storage/v1/b/keep-alive-bucket/o?uploadType=media&name=large HTTP/1.1\r\nHost: localhost\r\nContent-Length: 32\r\n\r\nGET /_internal/health HTTP/1.1\r\n\r\n");
    assert!(response.starts_with("HTTP/1.1 413"), "{response}");
    // The close is announced, so a pooled client never reuses the connection for its next request.
    assert!(response.contains("\r\nconnection: close\r\n"), "{response}");
    rejected
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert_eq!(rejected.read(&mut [0]).unwrap(), 0);
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_server_reclaims_idle_keep_alive_slots_for_waiting_clients() {
    let config = Config {
        max_connections: 1,
        keep_alive_timeout_ms: 60_000,
        ..common::http_config()
    };
    let (address, _, stop, task, _dir) = start(config).await;
    let health = b"GET /_internal/health HTTP/1.1\r\nHost: localhost\r\n\r\n";
    let mut idle = BufReader::new(stream(address));
    assert!(exchange(&mut idle, health).starts_with("HTTP/1.1 200"));
    // The only slot is idle, so the next client gets it long before the keep-alive timeout.
    let started = Instant::now();
    let mut waiting = BufReader::new(stream(address));
    assert!(exchange(&mut waiting, health).starts_with("HTTP/1.1 200"));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(idle.read(&mut [0]).unwrap(), 0);
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_limit_deadline_and_shutdown_release_idle_clients() {
    let config = Config {
        max_connections: 1,
        connection_timeout_ms: 300,
        ..common::http_config()
    };
    let (address, _, stop, task, _dir) = start(config).await;
    let mut first = stream(address);
    first
        .write_all(b"GET /_internal/health HTTP/1.1\r\n")
        .unwrap();
    let mut second = stream(address);
    second
        .write_all(b"GET /_internal/health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    second
        .set_read_timeout(Some(Duration::from_millis(60)))
        .unwrap();
    let mut byte = [0];
    let error = second.read(&mut byte).unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    // The partial first request expires; the queued connection can then be served.
    second
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let response = common::read_http_response(&mut BufReader::new(&mut second), false).unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200"));
    let mut idle = stream(address);
    idle.write_all(b"GET /_internal/health HTTP/1.1\r\n")
        .unwrap();
    stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[test]
fn config_precedence_validation_and_size_limit() {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let file = std::env::temp_dir().join(format!(
        "gcs-mimic-config-{}-{}.json",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&file, r#"{"max_connections":4,"listen":"127.0.0.1:1234"}"#).unwrap();
    let args = vec![
        "--config".into(),
        file.to_str().unwrap().into(),
        "--max-connections".into(),
        "6".into(),
    ];
    let env = BTreeMap::from([
        ("GCS_MIMIC_MAX_CONNECTIONS".into(), "5".into()),
        ("GCS_MIMIC_CONFIG".into(), "missing-file".into()),
    ]);
    let config = common::load_http_config(&args, &env).unwrap();
    assert_eq!(config.max_connections, 6);
    assert_eq!(config.listen.port(), 1234);
    assert_eq!(
        common::load_http_config(
            &args[..2],
            &BTreeMap::from([("GCS_MIMIC_MAX_CONNECTIONS".into(), "5".into())])
        )
        .unwrap()
        .max_connections,
        5
    );
    assert_eq!(
        common::load_http_config(&args[..2], &BTreeMap::new())
            .unwrap()
            .max_connections,
        4
    );
    assert_eq!(
        common::load_http_config(&[], &BTreeMap::new())
            .unwrap()
            .max_connections,
        32
    );
    for (key, value) in [
        ("max-connections", "0"),
        ("max-connections", "1025"),
        ("worker-threads", "33"),
        ("max-blocking-threads", "0"),
        ("connection-timeout-ms", "60001"),
        ("shutdown-timeout-ms", "-1"),
        ("listen", "localhost:4443"),
        ("auth-mode", "strict"),
        ("seed-policy", "if_empty"),
    ] {
        assert!(
            common::load_http_config(&[format!("--{key}"), value.into()], &BTreeMap::new())
                .is_err(),
            "{key}"
        );
    }
    assert!(common::load_http_config(&["--listen".into()], &BTreeMap::new()).is_err());
    assert!(
        common::load_http_config(
            &[
                "--max-connections".into(),
                "1".into(),
                "--max-connections".into(),
                "2".into()
            ],
            &BTreeMap::new()
        )
        .is_err()
    );
    for body in [
        "{bad",
        "[]",
        r#"{"unknown":1}"#,
        r#"{"worker_threads":null}"#,
        &" ".repeat(65_537),
    ] {
        fs::write(&file, body).unwrap();
        assert!(common::load_http_config(&args[..2], &BTreeMap::new()).is_err());
    }
    fs::remove_file(file).unwrap();
}

#[test]
fn startup_diagnostics_aggregate_independent_inputs_and_material_without_side_effects() {
    let root = TestDir::new();
    let data = root.0.join("never-created");
    let file = root.0.join("malformed.json");
    let cert = root.0.join("cert.pem");
    let key = root.0.join("key.pem");
    let identity = root.0.join("identities.json");
    fs::write(&file, "{ secret-config-value").unwrap();
    fs::write(&cert, "secret-certificate-value").unwrap();
    fs::write(&key, "secret-private-key-value").unwrap();
    fs::write(&identity, "{ secret-identity-value").unwrap();
    let output = common::server_command()
        .clean_env()
        .args(["serve", "--data-dir"])
        .arg(&data)
        .arg("--config")
        .arg(file)
        .arg("--tls-cert")
        .arg(cert)
        .arg("--tls-key")
        .arg(key)
        .args(["--auth-mode", "strict", "--identity-file"])
        .arg(identity)
        .args(["--worker-threads", "0", "-port=8080", "-host", "127.0.0.2"])
        .env("FAKE_GCS_PORT", "9000")
        .env("FAKE_GCS_UNRELATED", "secret-unknown-value")
        .env("GCS_MIMIC_MAX_CONNECTIONS", "0")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!data.exists());
    let text = String::from_utf8(output.stderr).unwrap();
    assert!(!text.contains("secret-"), "{text}");
    assert!(!text.contains(&root.0.display().to_string()), "{text}");
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 2, "{text}");
    assert_eq!(events[0]["level"], "warn");
    assert_eq!(events[1]["level"], "error");
    let diagnostics = events[1]["diagnostics"].as_array().unwrap();
    for (source, key) in [
        ("cli", "--config"),
        ("cli", "--worker-threads"),
        ("cli", "-host"),
        ("cli", "-port"),
        ("environment", "FAKE_GCS_PORT"),
        ("environment", "GCS_MIMIC_MAX_CONNECTIONS"),
        ("tls", "tls_cert"),
        ("identity", "identity_file"),
    ] {
        assert!(
            diagnostics
                .iter()
                .any(|d| d["source"] == source && d["key"] == key),
            "{source}/{key}: {text}"
        );
    }
    assert!(text.contains("GCS_MIMIC_LISTEN=127.0.0.2:8080"));
}

#[test]
fn invalid_tls_timeout_does_not_hide_independent_tls_and_identity_failures() {
    let root = TestDir::new();
    let data = root.0.join("never-created");
    let cert = root.0.join("cert.pem");
    let key = root.0.join("key.pem");
    let identity = root.0.join("identities.json");
    for file in [&cert, &key, &identity] {
        fs::write(file, "secret-invalid-material").unwrap();
    }
    let output = common::server_command()
        .clean_env()
        .args(["serve", "--data-dir"])
        .arg(&data)
        .args(["--tls-handshake-timeout-ms", "0", "--tls-cert"])
        .arg(cert)
        .arg("--tls-key")
        .arg(key)
        .args(["--auth-mode", "strict", "--identity-file"])
        .arg(identity)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!data.exists());
    let text = String::from_utf8(output.stderr).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let errors = events.iter().find(|e| e["level"] == "error").unwrap()["diagnostics"]
        .as_array()
        .unwrap();
    assert_eq!(errors.len(), 3, "{text}");
    for (source, key) in [
        ("cli", "--tls-handshake-timeout-ms"),
        ("tls", "tls_cert"),
        ("identity", "identity_file"),
    ] {
        assert!(
            errors
                .iter()
                .any(|d| d["source"] == source && d["key"] == key),
            "{text}"
        );
    }
    assert!(!text.contains("secret-invalid-material"));
}

#[cfg(unix)]
#[test]
fn non_utf8_environment_values_preserve_unknown_warnings_and_aggregate_errors() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let invalid = || OsString::from_vec(b"secret-invalid-\xff-value".to_vec());
    let root = TestDir::new();
    let mut child = common::server_command()
        .clean_env()
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
        ])
        .arg(root.0.join("valid-data"))
        .env("FAKE_GCS_UNRELATED", invalid())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (mut stdout, ready) = process_ready(&mut child);
    assert_eq!(ready["event"], "ready");
    common::stop_server(&mut child, &mut stdout);
    let mut text = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    let warning: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(warning["level"], "warn");
    assert_eq!(warning["diagnostics"][0]["key"], "FAKE_GCS_UNRELATED");
    assert!(!text.contains("secret-invalid"));
    for with_known in [false, true] {
        let data = root.0.join(if with_known {
            "invalid-known-data"
        } else {
            "invalid-cli-data"
        });
        let mut command = common::server_command();
        command
            .clean_env()
            .args([
                "serve",
                "--scheme",
                "http",
                "--max-connections",
                "0",
                "--data-dir",
            ])
            .arg(&data)
            .env("FAKE_GCS_UNRELATED", invalid());
        if with_known {
            command
                .env("FAKE_GCS_PORT", invalid())
                .env("GCS_MIMIC_WORKER_THREADS", invalid());
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!data.exists());
        let text = String::from_utf8(output.stderr).unwrap();
        let events: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events[0]["level"], "warn");
        let errors = events.iter().find(|e| e["level"] == "error").unwrap()["diagnostics"]
            .as_array()
            .unwrap();
        assert_eq!(errors.len(), if with_known { 3 } else { 1 }, "{text}");
        assert!(errors.iter().any(|d| d["key"] == "--max-connections"));
        if with_known {
            assert!(text.contains("<PORT_0_TO_65535>"), "{text}");
            for key in ["FAKE_GCS_PORT", "GCS_MIMIC_WORKER_THREADS"] {
                assert!(
                    errors
                        .iter()
                        .any(|d| d["key"] == key && d["fix"].as_str().unwrap().contains("UTF-8")),
                    "{text}"
                );
            }
        }
        assert!(!text.contains("secret-invalid"));
    }
}

#[test]
fn unknown_legacy_environment_warns_and_native_server_still_becomes_ready() {
    let root = TestDir::new();
    let mut child = common::server_command()
        .clean_env()
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
        ])
        .arg(root.0.join("data"))
        .env("FAKE_GCS_UNRELATED_APP", "secret-app-value")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (mut stdout, ready) = process_ready(&mut child);
    assert_eq!(ready["event"], "ready");
    common::stop_server(&mut child, &mut stdout);
    let mut text = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut text)
        .unwrap();
    let warning: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(warning["level"], "warn");
    assert_eq!(warning["diagnostics"][0]["key"], "FAKE_GCS_UNRELATED_APP");
    assert!(!text.contains("secret-app-value"));
}

#[test]
fn compose_only_variables_and_missing_subcommand_report_native_migration() {
    let root = TestDir::new();
    let output = common::server_command()
        .clean_env()
        .args(["serve", "--data-dir"])
        .arg(root.0.join("data"))
        .env("GCS_MIMIC_PORT", "secret")
        .env("GCS_MIMIC_IMAGE", "secret")
        .env("GCS_MIMIC_TLS_DIR", "secret")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!root.0.join("data").exists());
    let text = String::from_utf8(output.stderr).unwrap();
    let events: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let event = events
        .iter()
        .find(|event| event["level"] == "error")
        .unwrap();
    assert_eq!(event["diagnostics"].as_array().unwrap().len(), 3);
    assert!(!text.contains("secret"));
    for expected in [
        "Compose-only",
        "host port",
        "image selection",
        "mount selection",
        "GCS_MIMIC_TLS_CERT",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    let output = common::server_command()
        .clean_env()
        .args(["-scheme=http", "-port", "9000", "--data=fixtures"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let text = String::from_utf8(output.stderr).unwrap();
    for expected in [
        "-scheme",
        "-port",
        "--data",
        "serve --listen 0.0.0.0:4443 --data-dir /var/lib/gcs-mimic",
    ] {
        assert!(text.contains(expected), "{text}");
    }
}

#[test]
fn native_helper_values_that_look_like_legacy_flags_are_preserved() {
    for command in ["public", "sign"] {
        for object in ["-data", "-port", "-x"] {
            let mut process = common::server_command();
            process.clean_env().args([
                "url",
                command,
                "--base-url",
                "http://localhost:4443",
                "--bucket",
                "bucket",
                "--object",
                object,
            ]);
            if command == "sign" {
                process.args([
                    "--method",
                    "GET",
                    "--expires",
                    "15m",
                    "--signer",
                    "local-viewer@example.invalid",
                    "--private-key",
                    "fixtures/signing/test-only-private.pem",
                    "--timestamp",
                    "20261023T120000Z",
                ]);
            }
            let output = process.output().unwrap();
            assert!(output.status.success(), "{command}/{object}: {output:?}");
            assert!(output.stderr.is_empty());
            assert!(String::from_utf8_lossy(&output.stdout).contains(object));
        }
    }
}

#[test]
fn cli_help_errors_and_bind_failure() {
    let bin = env!("CARGO_BIN_EXE_gcs-mimic");
    for flag in ["--help", "--version"] {
        assert!(
            Command::new(bin)
                .clean_env()
                .arg(flag)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    for args in [
        vec![],
        vec!["url", "sign"],
        vec!["serve", "--worker-threads", "0"],
        vec!["tls", "ca", "unexpected"],
        vec!["healthcheck", "--url", "http://127.0.0.1:4443/anything"],
    ] {
        let output = Command::new(bin).clean_env().args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dir = TestDir::new();
    let output = Command::new(bin)
        .clean_env()
        .args([
            "serve",
            "--scheme",
            "http",
            "--listen",
            &listener.local_addr().unwrap().to_string(),
            "--data-dir",
            dir.0.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("ready"));
}

#[test]
fn healthcheck_rejects_truncated_malformed_and_oversized_responses() {
    for (response, success) in [
        ("HTTP/1.1 200 ".to_owned(), false),
        ("HTTP/1.1 2000 Nope\r\n\r\n".to_owned(), false),
        (
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx".to_owned(),
            false,
        ),
        ("HTTP/1.1 200 OK\r\nbroken\r\n\r\n".to_owned(), false),
        (
            format!("HTTP/1.1 200 OK\r\nX-Large: {}\r\n\r\n", "x".repeat(17_000)),
            false,
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_owned(),
            true,
        ),
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mock = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() <= 512);
            }
            stream.write_all(response.as_bytes()).unwrap();
        });
        let output = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
            .clean_env()
            .args([
                "healthcheck",
                "--url",
                &format!("http://{address}/_internal/ready"),
            ])
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        mock.join().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn healthcheck_exit_status_tracks_readiness() {
    let (address, state, stop, task, _dir) = start(common::http_config()).await;
    let bin = env!("CARGO_BIN_EXE_gcs-mimic");
    let probe = || {
        Command::new(bin)
            .clean_env()
            .args([
                "healthcheck",
                "--url",
                &format!("http://{address}/_internal/ready"),
            ])
            .output()
            .unwrap()
    };
    assert!(probe().status.success());
    state.ready.store(false, Ordering::Release);
    assert!(!probe().status.success());
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    assert!(!probe().status.success());
}

#[test]
fn healthcheck_deadline_includes_slow_streaming_responses() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted_send, accepted) = std::sync::mpsc::channel();
    let mock = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        accepted_send.send(std::time::Instant::now()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() <= 512);
        }
        for chunk in ["HTTP/1.1 ", "200 OK\r\n", "Content-Length: 2\r\n", "\r\nok"] {
            std::thread::sleep(Duration::from_millis(600));
            if let Err(error) = stream.write_all(chunk.as_bytes()) {
                assert!(matches!(
                    error.kind(),
                    std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset
                ));
                break;
            }
        }
    });
    let output = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
        .clean_env()
        .args([
            "healthcheck",
            "--url",
            &format!("http://{address}/_internal/ready"),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
    // The deadline starts before the connection, so timing from acceptance excludes process
    // startup, which loaded machines delay independently of the healthcheck deadline.
    assert!(accepted.recv().unwrap().elapsed() < Duration::from_secs(3));
    mock.join().unwrap();
}
