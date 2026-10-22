use gcs_mimic::{
    clock::Clock,
    config::Config,
    server::{self, State},
    storage::Catalog,
};
mod common;
use common::TestDir;
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
        .args(["serve", "--listen", address, "--data-dir"])
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
            .args(["serve", "--listen", address, "--data-dir"])
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
            let _ = send.send(line);
        });
        let line = match receive.recv_timeout(Duration::from_secs(3)) {
            Ok(line) => line,
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
            .args(["serve", "--listen", &address, "--data-dir"])
            .arg(&dir.0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let (mut stdout, ready) = process_ready(&mut child);
        assert_eq!(ready["event"], "ready");
        address = ready["listen"].as_str().unwrap().into();
        assert!(
            request(
                address.parse().unwrap(),
                "GET /_internal/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
            )
            .starts_with("HTTP/1.1 200")
        );
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
            .args(["serve", "--listen", first, "--data-dir"])
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
            .args(["serve", "--listen", "127.0.0.1:0", "--data-dir"])
            .arg(&dir.0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: serde_json::Value = serde_json::from_str(&line).expect("ready after recovery");
        assert_eq!(ready["event"], "ready");
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
        api_base_url: config.api_base_url.clone(),
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
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wire_probes_clock_and_unimplemented_routes() {
    let (address, state, stop, task, _dir) = start(Config::default()).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_stops_accepting_on_original_listener() {
    let dir = TestDir::new();
    let witness = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    witness.set_nonblocking(true).unwrap();
    let address = witness.local_addr().unwrap();
    let listener = tokio::net::TcpListener::from_std(witness.try_clone().unwrap()).unwrap();
    let config = Config::default();
    let state = Arc::new(State {
        ready: AtomicBool::new(true),
        clock: Arc::new(FixedClock),
        catalog: Catalog::open(&dir.0).await.unwrap(),
        api_base_url: config.api_base_url.clone(),
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
    let (_address, state, stop, task, dir) = start(Config::default()).await;
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
    assert!(!state.is_ready());
    assert!(state.catalog.get_bucket("missing").await.is_err());
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = Config::default();
    let restarted = Arc::new(State {
        ready: AtomicBool::new(true),
        clock: Arc::new(FixedClock),
        catalog,
        api_base_url: config.api_base_url.clone(),
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
    let (address, _, stop, task, _dir) = start(Config::default()).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_limit_deadline_and_shutdown_release_idle_clients() {
    let config = Config {
        max_connections: 1,
        connection_timeout_ms: 300,
        ..Config::default()
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
    let mut response = String::new();
    second.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"));
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
    let config = Config::load(&args, &env).unwrap();
    assert_eq!(config.max_connections, 6);
    assert_eq!(config.listen.port(), 1234);
    assert_eq!(
        Config::load(
            &args[..2],
            &BTreeMap::from([("GCS_MIMIC_MAX_CONNECTIONS".into(), "5".into())])
        )
        .unwrap()
        .max_connections,
        5
    );
    assert_eq!(
        Config::load(&args[..2], &BTreeMap::new())
            .unwrap()
            .max_connections,
        4
    );
    assert_eq!(
        Config::load(&[], &BTreeMap::new()).unwrap().max_connections,
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
            Config::load(&[format!("--{key}"), value.into()], &BTreeMap::new()).is_err(),
            "{key}"
        );
    }
    assert!(Config::load(&["--listen".into()], &BTreeMap::new()).is_err());
    assert!(
        Config::load(
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
        assert!(Config::load(&args[..2], &BTreeMap::new()).is_err());
    }
    fs::remove_file(file).unwrap();
}

#[test]
fn cli_help_errors_and_bind_failure() {
    let bin = env!("CARGO_BIN_EXE_gcs-mimic");
    for flag in ["--help", "--version"] {
        assert!(
            Command::new(bin)
                .env_clear()
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
        vec![
            "healthcheck",
            "--url",
            "https://127.0.0.1:4443/_internal/ready",
        ],
        vec!["healthcheck", "--url", "http://127.0.0.1:4443/anything"],
    ] {
        let output = Command::new(bin).env_clear().args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dir = TestDir::new();
    let output = Command::new(bin)
        .env_clear()
        .args([
            "serve",
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
            .env_clear()
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
    let (address, state, stop, task, _dir) = start(Config::default()).await;
    let bin = env!("CARGO_BIN_EXE_gcs-mimic");
    let probe = || {
        Command::new(bin)
            .env_clear()
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
    let mock = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
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
    let start = std::time::Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_gcs-mimic"))
        .env_clear()
        .args([
            "healthcheck",
            "--url",
            &format!("http://{address}/_internal/ready"),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
    assert!(start.elapsed() < Duration::from_secs(3));
    mock.join().unwrap();
}
