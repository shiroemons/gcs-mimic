use super::*;
#[cfg(unix)]
use crate::config::Scheme;
use crate::test_support::TestDir;

#[tokio::test]
#[cfg(unix)]
async fn fatal_listener_failure_drains_background_work_before_catalog_close() {
    let data = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let state = Arc::new(State {
        api_base_url: "http://127.0.0.1".into(),
        ..State::for_test(catalog.clone(), Auth::permissive())
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let checked = Arc::new(AtomicBool::new(false));
    let drained = checked.clone();
    let owner = catalog.clone();
    let result = ACCEPT_FAULTS
        .scope(
            Arc::new(std::sync::Mutex::new([libc::EBADF].into())),
            serve_with_tls_and_drain(
                vec![(listener, ListenerSecurity::Plain)],
                Config::default(),
                state,
                std::future::pending(),
                async move {
                    assert!(owner.is_healthy());
                    owner
                        .sync_state()
                        .catalog_id()
                        .await
                        .map_err(io::Error::other)?;
                    drained.store(true, Ordering::Release);
                    Ok(())
                },
            ),
        )
        .await;
    assert!(result.is_err());
    assert!(checked.load(Ordering::Acquire));
    assert!(!catalog.is_healthy());
    Catalog::open(&data.0).await.unwrap().close().await.unwrap();
}

async fn health(address: std::net::SocketAddr) -> String {
    let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
    stream
        .write_all(
            b"GET /_internal/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut reply))
        .await
        .expect("the server closes or answers the connection")
        .ok();
    String::from_utf8_lossy(&reply).into_owned()
}

#[cfg(unix)]
#[tokio::test]
async fn exhausted_accepts_pause_and_keep_serving() {
    let data = TestDir::new();
    let state = Arc::new(State::for_test(
        Catalog::open(&data.0).await.unwrap(),
        Auth::permissive(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    let faults = Arc::new(std::sync::Mutex::new(
        [libc::EMFILE, libc::EMFILE, libc::EMFILE, libc::ECONNABORTED].into(),
    ));
    let config = Config {
        scheme: Scheme::Http,
        ..Config::default()
    };
    let server = tokio::spawn(ACCEPT_FAULTS.scope(
        faults,
        serve(listener, config, state, async {
            shutdown.await.map_err(io::Error::other)
        }),
    ));
    let reply = health(address).await;
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply:?}");
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn exhausted_accept_pause_still_honors_shutdown() {
    const FAILURES: usize = 8;
    let data = TestDir::new();
    let state = Arc::new(State::for_test(
        Catalog::open(&data.0).await.unwrap(),
        Auth::permissive(),
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let faults = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
        vec![libc::EMFILE; FAILURES],
    )));
    let (stop, shutdown) = tokio::sync::oneshot::channel();
    let (draining, drained) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(ACCEPT_FAULTS.scope(
        faults.clone(),
        serve_with_tls_and_drain(
            vec![(listener, ListenerSecurity::Plain)],
            Config::default(),
            state,
            async { shutdown.await.map_err(io::Error::other) },
            async move {
                draining.send(tokio::time::Instant::now()).unwrap();
                Ok(())
            },
        ),
    ));
    tokio::task::yield_now().await;
    let mut backoff = transport::AcceptBackoff::default();
    let error = io::Error::from_raw_os_error(libc::EMFILE);
    for _ in 1..FAILURES {
        tokio::time::advance(backoff.exhausted(&error)).await;
        tokio::task::yield_now().await;
    }
    assert!(faults.lock().unwrap().is_empty());
    let paused_for = backoff.exhausted(&error);
    let shutdown_at = tokio::time::Instant::now();
    stop.send(()).unwrap();
    let drain_at = drained.await.unwrap();
    server.await.unwrap().unwrap();
    assert!(drain_at - shutdown_at < paused_for);
}

#[tokio::test]
async fn handler_panic_closes_only_its_connection() {
    let data = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let state = Arc::new(State {
        api_base_url: "http://127.0.0.1".into(),
        ..State::for_test(catalog, Auth::permissive())
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    // A single slot shows that the panicked connection gave its slot back.
    let config = Config {
        max_connections: 1,
        ..Config::default()
    };
    let armed = Arc::new(AtomicBool::new(true));
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(FORCE_HANDLER_PANIC.scope(
        armed.clone(),
        serve_with_tls(
            vec![(listener, ListenerSecurity::Plain)],
            config,
            state,
            async move { stopped.await.map_err(io::Error::other) },
        ),
    ));
    // The panicking request gets no response; only its connection is closed.
    assert_eq!(health(address).await, "");
    assert!(!armed.load(Ordering::Acquire));
    for _ in 0..3 {
        assert!(health(address).await.starts_with("HTTP/1.1 200"));
    }
    assert!(!server.is_finished());
    stop.send(()).unwrap();
    server.await.unwrap().unwrap();
}
