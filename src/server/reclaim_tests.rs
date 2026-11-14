//! A full server takes back one idle connection per waiting client, the oldest first.

use super::*;
use crate::test_support::TestDir;
use http_body_util::Empty;
use std::task::{Context, Waker};

fn woken(turn: &mut IdleTurn<'_>) -> bool {
    turn.turn
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
        .is_ready()
}

/// Enabling takes a stored wake, and dropping the `Notified` afterwards would lose it.
#[test]
fn a_stored_wake_taken_by_a_connection_that_leaves_reaches_the_next_in_line() {
    let slots = SlotReclaim::default();
    let _request = slots.request();
    drop(slots.join_line());
    let mut next = slots.join_line();
    assert!(woken(&mut next));
    assert!(slots.take_request());
}

#[test]
fn a_request_wakes_only_the_connection_that_turned_idle_first() {
    let slots = SlotReclaim::default();
    let mut oldest = slots.join_line();
    let mut younger = slots.join_line();
    let _request = slots.request();
    assert!(woken(&mut oldest));
    assert!(!woken(&mut younger));
    assert!(slots.take_request());
}

#[test]
fn a_full_server_without_a_waiting_client_wakes_nobody() {
    let slots = SlotReclaim::default();
    let mut idle = slots.join_line();
    assert!(!woken(&mut idle));
    assert!(!slots.is_wanted());
}

#[test]
fn a_request_with_nobody_idle_reaches_the_next_connection_to_turn_idle() {
    let slots = SlotReclaim::default();
    let _request = slots.request();
    let mut idle = slots.join_line();
    assert!(woken(&mut idle));
    assert!(slots.take_request());
}

#[test]
fn a_connection_leaving_the_line_passes_its_wake_on() {
    let slots = SlotReclaim::default();
    let oldest = slots.join_line();
    let mut younger = slots.join_line();
    let _request = slots.request();
    drop(oldest);
    assert!(woken(&mut younger));
}

#[test]
fn a_busy_connection_can_hand_its_wake_to_the_next_in_line() {
    let slots = SlotReclaim::default();
    let mut oldest = slots.join_line();
    let mut younger = slots.join_line();
    let _request = slots.request();
    assert!(woken(&mut oldest));
    drop(oldest);
    assert!(woken(&mut younger));
    assert!(slots.take_request());
}

#[test]
fn a_wake_that_outlives_its_request_closes_nothing() {
    let slots = SlotReclaim::default();
    drop(slots.request());
    let mut idle = slots.join_line();
    assert!(woken(&mut idle));
    assert!(!slots.is_wanted());
    assert!(!slots.take_request());
}

#[test]
fn only_one_connection_takes_a_request() {
    let slots = SlotReclaim::default();
    let _request = slots.request();
    assert!(slots.take_request());
    assert!(!slots.take_request());
}

/// A place in line belongs to the idle phase, so leaving it passes on a wake in any direction.
#[test]
fn leaving_the_idle_phase_gives_up_the_place_in_line() {
    let slots = SlotReclaim::default();
    let oldest = ConnectionPhase::Idle {
        turn: slots.join_line(),
    };
    let mut younger = slots.join_line();
    let _request = slots.request();
    assert!(!woken(&mut younger));
    drop(oldest);
    assert!(woken(&mut younger));
}

/// Far beyond what a healthy reclaim takes, and far below the keep-alive limit it replaces.
const HANG_GUARD: Duration = Duration::from_secs(20);

type HttpSender = hyper::client::conn::http1::SendRequest<Empty<Bytes>>;
type GrpcSender = hyper::client::conn::http2::SendRequest<Empty<Bytes>>;

/// A running server with one HTTP and one gRPC listener and a long keep-alive limit, so only the
/// reclaim can end an idle connection during a test.
struct MixedServer {
    http: std::net::SocketAddr,
    grpc: std::net::SocketAddr,
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<(), crate::diagnostics::Diagnostics>>,
    _data: TestDir,
}

impl MixedServer {
    async fn start(max_connections: usize) -> Self {
        let data = TestDir::new();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let state = Arc::new(State {
            api_base_url: "http://127.0.0.1".into(),
            ..State::for_test(catalog, Auth::permissive())
        });
        let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let grpc = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (http_address, grpc_address) = (http.local_addr().unwrap(), grpc.local_addr().unwrap());
        let config = Config {
            max_connections,
            keep_alive_timeout: TimeLimit::from_millis(600_000),
            ..Config::default()
        };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let listeners = TcpListeners {
            public: vec![(http, ListenerSecurity::Plain)],
            trusted: None,
            grpc: Some(grpc),
        };
        let task = tokio::spawn(serve_with_grpc_and_drain(
            listeners,
            config,
            state,
            async move { stopped.await.map_err(io::Error::other) },
            async { Ok(()) },
        ));
        Self {
            http: http_address,
            grpc: grpc_address,
            stop,
            task,
            _data: data,
        }
    }

    async fn stop(self) {
        self.stop.send(()).unwrap();
        self.task.await.unwrap().unwrap();
    }

    /// A gRPC connection that finished one call, and the task that ends with the connection.
    async fn idle_grpc(&self) -> (GrpcSender, tokio::task::JoinHandle<hyper::Result<()>>) {
        let stream = tokio::net::TcpStream::connect(self.grpc).await.unwrap();
        let (mut sender, connection) = hyper::client::conn::http2::handshake(
            hyper_util::rt::TokioExecutor::new(),
            TokioIo::new(stream),
        )
        .await
        .unwrap();
        let driver = tokio::spawn(connection);
        let call = hyper::Request::post(format!("http://{}/unknown.Service/Call", self.grpc))
            .body(Empty::new())
            .unwrap();
        let response = sender.send_request(call).await.unwrap();
        response.into_body().collect().await.unwrap();
        (sender, driver)
    }

    /// An HTTP connection, and the task that ends with the connection.
    async fn http(&self) -> (HttpSender, tokio::task::JoinHandle<hyper::Result<()>>) {
        let stream = tokio::net::TcpStream::connect(self.http).await.unwrap();
        let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        (sender, tokio::spawn(connection))
    }
}

async fn health(sender: &mut HttpSender) -> StatusCode {
    let request = hyper::Request::get("/_internal/health")
        .header("host", "localhost")
        .body(Empty::new())
        .unwrap();
    let response = tokio::time::timeout(HANG_GUARD, sender.send_request(request))
        .await
        .expect("the waiting client gets a slot long before the keep-alive limit")
        .unwrap();
    let status = response.status();
    response.into_body().collect().await.unwrap();
    status
}

/// Waits for the connection to end, which only the server can cause in these tests.
async fn closed(driver: tokio::task::JoinHandle<hyper::Result<()>>) {
    tokio::time::timeout(HANG_GUARD, driver)
        .await
        .expect("the idle connection closes")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_full_server_reclaims_an_idle_grpc_connection_for_a_new_client() {
    let server = MixedServer::start(1).await;
    let (_grpc, grpc_driver) = server.idle_grpc().await;
    let (mut waiting, _driver) = server.http().await;
    assert_eq!(health(&mut waiting).await, StatusCode::OK);
    closed(grpc_driver).await;
    server.stop().await;
}

/// HTTP/1 and gRPC connections share one line, so the protocol does not pick who gives way.
#[tokio::test]
async fn the_idle_line_orders_connections_across_protocols() {
    let server = MixedServer::start(2).await;
    let (_grpc, grpc_driver) = server.idle_grpc().await;
    let (mut younger, younger_driver) = server.http().await;
    assert_eq!(health(&mut younger).await, StatusCode::OK);
    let (mut waiting, _driver) = server.http().await;
    assert_eq!(health(&mut waiting).await, StatusCode::OK);
    closed(grpc_driver).await;
    assert!(!younger_driver.is_finished());
    assert_eq!(health(&mut younger).await, StatusCode::OK);
    server.stop().await;
}
