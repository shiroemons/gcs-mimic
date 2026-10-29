use super::*;
use crate::{clock::SystemClock, test_support::TestDir};

#[tokio::test]
async fn listener_failure_drains_background_work_before_catalog_close() {
    let data = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let state = Arc::new(State {
        ready: AtomicBool::new(true),
        clock: Arc::new(SystemClock),
        catalog: catalog.clone(),
        api_base_url: "http://127.0.0.1".into(),
        default_location: "US".into(),
        auth: Auth::permissive(),
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let checked = Arc::new(AtomicBool::new(false));
    let drained = checked.clone();
    let owner = catalog.clone();
    let result = FORCE_LISTENER_ERROR
        .scope(
            true,
            serve_with_tls_and_drain(
                vec![(listener, None)],
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

#[tokio::test]
async fn handler_panic_closes_only_its_connection() {
    let data = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let state = Arc::new(State {
        ready: AtomicBool::new(true),
        clock: Arc::new(SystemClock),
        catalog,
        api_base_url: "http://127.0.0.1".into(),
        default_location: "US".into(),
        auth: Auth::permissive(),
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
        serve_with_tls(vec![(listener, None)], config, state, async move {
            stopped.await.map_err(io::Error::other)
        }),
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
