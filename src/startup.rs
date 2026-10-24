use gcs_mimic::{
    auth::Auth,
    clock::{Clock, SystemClock},
    config::Config,
    diagnostics::{DiagnosticSource, Diagnostics, combine_shutdown_results as merge_close_result},
    seed::SeedOutcome,
    server::{self, State},
    storage::Catalog,
};
use serde::Serialize;
use std::{
    future::Future,
    io,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, atomic::AtomicBool},
};

pub(crate) enum PhaseResult<T> {
    Complete(T),
    Stopped,
}

// Only reviewed request waits may be dropped; their effects retain catalog-owned permits.
struct CancelSafe<F>(F);

async fn race<T>(
    work: CancelSafe<impl Future<Output = T>>,
    shutdown: Pin<&mut impl Future<Output = io::Result<()>>>,
) -> io::Result<PhaseResult<T>> {
    tokio::select! {
        biased;
        signal = shutdown => shutdown_result(signal).map(|_| PhaseResult::Stopped),
        result = work.0 => Ok(PhaseResult::Complete(result)),
    }
}

#[derive(Serialize)]
pub(crate) struct ReadyEvent {
    event: &'static str,
    listen: SocketAddr,
    protocol: &'static str,
    gcs_api: &'static str,
    max_connections: usize,
    max_object_bytes: u64,
    worker_threads: usize,
    max_blocking_threads: usize,
    connection_timeout_ms: u64,
    object_request_timeout_ms: u64,
    catalog_wait_timeout_ms: u64,
    shutdown_timeout_ms: u64,
    auth_mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    identities: Option<usize>,
    seed: SeedOutcome,
    scheme: &'static str,
    tls_handshake_timeout_ms: u64,
    public_base_url: Option<String>,
    public_virtual_host_enabled: bool,
}

impl ReadyEvent {
    pub(crate) fn new(config: &Config, auth: &Auth, listen: SocketAddr, seed: SeedOutcome) -> Self {
        Self {
            event: "ready",
            listen,
            protocol: "http1",
            gcs_api: "json-bucket-object-media-crud",
            max_connections: config.max_connections,
            max_object_bytes: config.max_object_bytes,
            worker_threads: config.worker_threads,
            max_blocking_threads: config.max_blocking_threads,
            connection_timeout_ms: config.connection_timeout_ms,
            object_request_timeout_ms: config.object_request_timeout_ms,
            catalog_wait_timeout_ms: config.catalog_wait_timeout_ms,
            shutdown_timeout_ms: config.shutdown_timeout_ms,
            auth_mode: auth.mode().as_str(),
            identities: auth.identity_count(),
            seed,
            scheme: config.scheme.as_str(),
            tls_handshake_timeout_ms: config.tls_handshake_timeout_ms,
            public_base_url: config
                .public_base_url
                .as_ref()
                .map(|origin| origin.as_str().to_owned()),
            public_virtual_host_enabled: config.public_virtual_host_enabled(),
        }
    }
}

struct BoundStartup {
    listener: tokio::net::TcpListener,
    seed: SeedOutcome,
}

async fn open_catalog(
    config: &Config,
    clock: Arc<dyn Clock>,
    shutdown: std::pin::Pin<&mut impl std::future::Future<Output = io::Result<()>>>,
) -> io::Result<PhaseResult<Catalog>> {
    let opening = async {
        Catalog::open_with_clock(&config.data_dir, clock).await.map_err(|_| phase(DiagnosticSource::Catalog, "data_dir", "catalog unavailable or already in use; use a writable compatible native data directory owned by only one server and inspect its recovery state."))
    };
    tokio::pin!(opening);
    // Dropping this borrowed wait keeps the pinned owner alive for the required recovery drain.
    match race(CancelSafe(opening.as_mut()), shutdown).await {
        Ok(PhaseResult::Complete(result)) => result.map(PhaseResult::Complete),
        interrupted => {
            let catalog = opening.await?;
            finish_startup(&catalog, interrupted.map(|_| ())).await?;
            Ok(PhaseResult::Stopped)
        }
    }
}

async fn configure_catalog_policy(catalog: &Catalog, projects: &[String]) -> io::Result<()> {
    catalog.configure_public_access_prevention(projects).await.map_err(|_| phase(DiagnosticSource::Catalog, "data_dir", "Cannot configure catalog policy; inspect the native catalog and identity policy before restarting."))
}

async fn prepare_startup(
    catalog: &Catalog,
    config: &Config,
    projects: &[String],
    clock: &dyn Clock,
    mut shutdown: Pin<&mut impl Future<Output = io::Result<()>>>,
) -> io::Result<PhaseResult<BoundStartup>> {
    // PAP's submitted SQLite job owns its execution permit until completion.
    match race(
        CancelSafe(configure_catalog_policy(catalog, projects)),
        shutdown.as_mut(),
    )
    .await?
    {
        PhaseResult::Complete(result) => result?,
        PhaseResult::Stopped => return Ok(PhaseResult::Stopped),
    }
    // Future initializer/import/sync phases must define their own cancellation contract here.
    // Seed drops only source reads/request waits; publication and cleanup own upload permits.
    let PhaseResult::Complete(seed) = race(
        CancelSafe(async {
            gcs_mimic::seed::import(catalog, config, clock)
                .await
                .map_err(|error| phase(DiagnosticSource::Seed, "seed_dir", &seed_failure(&error)))
        }),
        shutdown.as_mut(),
    )
    .await?
    else {
        return Ok(PhaseResult::Stopped);
    };
    let seed = seed?;
    // Bind/TIME_WAIT has no catalog effects and owns its socket locally.
    let PhaseResult::Complete(listener) = race(
        CancelSafe(async {
            bind_listener(config.listen).await.map_err(|error| {
        phase(
            DiagnosticSource::Listener,
            "listen",
            if error.kind() == io::ErrorKind::AddrInUse {
                "Address already in use; choose a free --listen IP:PORT."
            } else {
                "Cannot bind listener; check --listen IP:PORT and local network permissions."
            },
        )
    })
        }),
        shutdown.as_mut(),
    )
    .await?
    else {
        return Ok(PhaseResult::Stopped);
    };
    let listener = listener?;
    Ok(PhaseResult::Complete(BoundStartup { listener, seed }))
}

async fn close_startup(catalog: &Catalog, result: io::Result<()>) -> io::Result<()> {
    merge_close_result(result, catalog.close().await.map_err(io::Error::other))
}

async fn finish_startup(catalog: &Catalog, result: io::Result<()>) -> io::Result<()> {
    close_startup(catalog, result).await?;
    report_stopped();
    Ok(())
}

fn report_stopped() {
    println!("{{\"event\":\"stopped\"}}");
}

fn shutdown_result(signal: io::Result<()>) -> io::Result<()> {
    signal.map_err(|_| {
        phase(
            DiagnosticSource::Runtime,
            "signal",
            "Cannot receive shutdown signal; check the runtime environment.",
        )
    })?;
    eprintln!("{{\"event\":\"stopping\"}}");
    Ok(())
}

pub(crate) async fn serve(
    config: Config,
    auth: Auth,
    acceptor: Option<tokio_rustls::TlsAcceptor>,
    shutdown: impl std::future::Future<Output = io::Result<()>>,
) -> io::Result<()> {
    tokio::pin!(shutdown);
    let clock = Arc::new(SystemClock);
    let catalog = match open_catalog(&config, clock.clone(), shutdown.as_mut()).await? {
        PhaseResult::Complete(catalog) => catalog,
        PhaseResult::Stopped => return Ok(()),
    };
    let BoundStartup { listener, seed } = match prepare_startup(
        &catalog,
        &config,
        auth.public_access_prevention_projects(),
        clock.as_ref(),
        shutdown.as_mut(),
    )
    .await
    {
        Ok(PhaseResult::Complete(bound)) => bound,
        result => return finish_startup(&catalog, result.map(|_| ())).await,
    };
    let ready = listener
        .local_addr()
        .map_err(|_| {
            phase(
                DiagnosticSource::Listener,
                "listen",
                "Cannot inspect listener address; check the local network environment.",
            )
        })
        .and_then(|address| {
            serde_json::to_string(&ReadyEvent::new(&config, &auth, address, seed)).map_err(|_| {
                phase(
                    DiagnosticSource::Runtime,
                    "ready",
                    "Cannot encode ready event; inspect startup configuration.",
                )
            })
        });
    let ready = match ready {
        Ok(ready) => ready,
        Err(error) => return close_startup(&catalog, Err(error)).await,
    };
    catalog.start_resumable_maintenance();
    let state = Arc::new(State {
        ready: AtomicBool::new(true),
        clock,
        catalog,
        api_base_url: config.api_base_url.clone(),
        auth,
    });
    println!("{ready}");
    server::serve_with_tls(listener, config, state, shutdown, acceptor).await?;
    report_stopped();
    Ok(())
}

pub(crate) fn phase(source: DiagnosticSource, key: &str, fix: &str) -> io::Error {
    io::Error::other(Diagnostics::error(source, key, fix))
}

fn seed_failure(error: &io::Error) -> String {
    let message = error.to_string();
    let reason = [
        "top-level entries",
        "Invalid bucket name",
        "Invalid object name",
        "max_object_bytes",
        "must not overlap",
        "32 directory levels",
        "10000 entries",
        "only regular files",
        "not a symlink",
        "must be UTF-8",
        "belongs to another project",
    ]
    .into_iter()
    .find(|reason| message.contains(reason))
    .unwrap_or("unreadable or invalid fixture input");
    format!(
        "seed import: {reason}; correct seed_dir fixtures and their permissions, names, project, overlap and configured limits before restarting."
    )
}

async fn bind_listener(address: SocketAddr) -> io::Result<tokio::net::TcpListener> {
    let socket = if address.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    // Linux socket(7) excludes live listeners; BSD address reuse can steal wildcard traffic.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    socket.set_reuseaddr(true)?;
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
    if let Err(error) = socket.bind(address) {
        bind_after_time_wait(&socket, address, error).await?;
    }
    #[cfg(not(all(unix, not(any(target_os = "linux", target_os = "android")))))]
    socket.bind(address)?;
    socket.listen(128)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
async fn bind_after_time_wait(
    socket: &tokio::net::TcpSocket,
    address: SocketAddr,
    original: io::Error,
) -> io::Result<()> {
    use std::time::Duration;

    if original.kind() != io::ErrorKind::AddrInUse {
        return Err(original);
    }
    // Wait out TIME_WAIT without weakening exclusive binding or waiting indefinitely.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(65);
    let mut probe = address;
    if address.ip().is_unspecified() {
        probe.set_ip(if address.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    let mut logged = false;
    loop {
        let connected = tokio::time::timeout_at(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(250)),
            tokio::net::TcpStream::connect(probe),
        )
        .await;
        if matches!(connected, Ok(Ok(_))) || tokio::time::Instant::now() >= deadline {
            return Err(original);
        }
        if !logged {
            eprintln!(
                "{}",
                serde_json::json!({"event":"bind_wait","listen":address.to_string(),"timeout_ms":65000,"reason":"address_in_use"})
            );
            logged = true;
        }
        tokio::time::sleep_until(
            deadline.min(tokio::time::Instant::now() + Duration::from_millis(250)),
        )
        .await;
        if tokio::time::Instant::now() >= deadline {
            return Err(original);
        }
        match socket.bind(address) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gcs_mimic::{
        clock::{Clock, SystemClock},
        storage::Catalog,
    };
    use std::{
        fs::{self, File},
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        task::Poll,
        time::{Duration, SystemTime},
    };

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "gcs-mimic-startup-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn config(&self) -> Config {
            Config {
                data_dir: self.0.clone(),
                listen: "127.0.0.1:0".parse().unwrap(),
                ..Config::default()
            }
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_dir_all(&self.0) {
                assert!(std::thread::panicking(), "fixture cleanup failed: {error}");
            }
        }
    }

    #[tokio::test]
    async fn shutdown_wins_before_starting_the_next_phase() {
        let shutdown = async { Ok(()) };
        tokio::pin!(shutdown);
        assert!(matches!(
            race::<()>(
                CancelSafe(std::future::poll_fn(|_| panic!("phase must not start"))),
                shutdown.as_mut()
            )
            .await,
            Ok(PhaseResult::Stopped)
        ));
    }

    #[tokio::test]
    async fn phase_completion_and_signal_errors_are_preserved() {
        let shutdown = std::future::pending();
        tokio::pin!(shutdown);
        assert!(matches!(
            race(CancelSafe(async { 42 }), shutdown.as_mut()).await,
            Ok(PhaseResult::Complete(42))
        ));
        assert!(matches!(
            race(
                CancelSafe(async { Err::<(), _>(io::Error::other("work failure")) }),
                shutdown.as_mut()
            )
            .await,
            Ok(PhaseResult::Complete(Err(_)))
        ));
        let shutdown = async { Err(io::Error::other("signal failure")) };
        tokio::pin!(shutdown);
        assert!(
            race(
                CancelSafe(std::future::pending::<io::Result<()>>()),
                shutdown.as_mut()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn catalog_open_failure_preserves_diagnostics_without_repolling() {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let shutdown = std::future::pending();
        tokio::pin!(shutdown);
        let error = open_catalog(&dir.config(), Arc::new(SystemClock), shutdown.as_mut())
            .await
            .err()
            .expect("live owner must reject open");
        let diagnostics = error
            .get_ref()
            .unwrap()
            .downcast_ref::<Diagnostics>()
            .unwrap();
        assert_eq!(diagnostics.errors[0].source, DiagnosticSource::Catalog);
        assert_eq!(diagnostics.errors[0].key, "data_dir");
        assert!(catalog.is_healthy());
        catalog.close().await.unwrap();
    }

    struct RecoveryClock {
        entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl Clock for RecoveryClock {
        fn now(&self) -> SystemTime {
            if let Some(entered) = self.entered.lock().unwrap().take() {
                let _ = entered.send(());
                let _ = self
                    .release
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(10));
            }
            SystemTime::now()
        }
    }

    #[tokio::test]
    async fn shutdown_during_recovery_drains_open_before_unlocking() {
        let dir = TestDir::new();
        let config = dir.config();
        let (entered, recovering) = tokio::sync::oneshot::channel();
        let (release, released) = mpsc::channel();
        let clock = Arc::new(RecoveryClock {
            entered: Mutex::new(Some(entered)),
            release: Mutex::new(released),
        });
        let (signal, shutdown) = tokio::sync::oneshot::channel();
        let (observed, stopping) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let shutdown = async {
                shutdown.await.map_err(io::Error::other)?;
                observed.send(()).unwrap();
                Ok(())
            };
            tokio::pin!(shutdown);
            open_catalog(&config, clock, shutdown.as_mut()).await
        });
        tokio::time::timeout(Duration::from_secs(5), recovering)
            .await
            .unwrap()
            .unwrap();
        signal.send(()).unwrap();
        stopping.await.unwrap();
        assert!(!task.is_finished());
        assert!(Catalog::open(&dir.0).await.is_err());
        release.send(()).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            PhaseResult::Stopped
        ));
        let reopened = Catalog::open(&dir.0).await.unwrap();
        reopened.close().await.unwrap();
    }

    #[test]
    fn shutdown_during_policy_configuration_drains_owned_work() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let dir = TestDir::new();
            let mut config = dir.config();
            config.data_dir = dir.0.join("data");
            config.seed_dir = Some(dir.0.join("fixtures"));
            fs::create_dir_all(dir.0.join("fixtures/seed-bucket")).unwrap();
            let catalog = Catalog::open(&config.data_dir).await.unwrap();
            catalog
                .configure_public_access_prevention(&["old-project".into()])
                .await
                .unwrap();
            let (release, released) = mpsc::channel();
            let (entered, blocked) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(10)).unwrap();
            });
            blocked.await.unwrap();
            {
                let projects = ["new-project".into()];
                let (signal, shutdown) = tokio::sync::oneshot::channel();
                let shutdown = async { shutdown.await.map_err(io::Error::other) };
                tokio::pin!(shutdown);
                let work = prepare_startup(
                    &catalog,
                    &config,
                    &projects,
                    &SystemClock,
                    shutdown.as_mut(),
                );
                tokio::pin!(work);
                std::future::poll_fn(|cx| {
                    assert!(work.as_mut().poll(cx).is_pending());
                    Poll::Ready(())
                })
                .await;
                signal.send(()).unwrap();
                assert!(matches!(
                    tokio::time::timeout(Duration::from_secs(2), work)
                        .await
                        .unwrap(),
                    Ok(PhaseResult::Stopped)
                ));
            }
            let close = close_startup(&catalog, Ok(()));
            tokio::pin!(close);
            std::future::poll_fn(|cx| {
                assert!(close.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            assert!(matches!(
                File::open(config.data_dir.join("catalog.lock"))
                    .unwrap()
                    .try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(5), close)
                .await
                .unwrap()
                .unwrap();
            blocker.await.unwrap();
            let reopened = Catalog::open(&config.data_dir).await.unwrap();
            assert!(
                reopened
                    .admin(gcs_mimic::storage::admin::Origin::Seed)
                    .get_bucket(&gcs_mimic::storage::admin::BucketName::new("seed-bucket").unwrap())
                    .await
                    .unwrap()
                    .is_none()
            );
            reopened.close().await.unwrap();
            let connection =
                rusqlite::Connection::open(config.data_dir.join("catalog.sqlite3")).unwrap();
            assert_eq!(
                connection
                    .query_row("SELECT complete FROM startup_seed", [], |row| row
                        .get::<_, i64>(0))
                    .unwrap(),
                0,
                "seed must not run after interrupted PAP"
            );
        });
    }

    #[tokio::test]
    async fn startup_failures_close_catalog_even_with_live_handles() {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let retained = catalog.clone();
        let config = dir.config();
        let shutdown = std::future::pending();
        tokio::pin!(shutdown);
        let failure = prepare_startup(
            &catalog,
            &config,
            &vec!["project".into(); 65],
            &SystemClock,
            shutdown.as_mut(),
        )
        .await
        .err()
        .expect("PAP failure");
        let diagnostics = failure
            .get_ref()
            .unwrap()
            .downcast_ref::<Diagnostics>()
            .unwrap();
        assert_eq!(diagnostics.errors[0].source, DiagnosticSource::Catalog);
        assert_eq!(diagnostics.errors[0].key, "data_dir");
        let error = finish_startup(&catalog, Err(failure)).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Cannot configure catalog policy")
        );
        assert!(!retained.is_healthy());
        let reopened = Catalog::open(&dir.0).await.unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut config = dir.config();
        config.listen = listener.local_addr().unwrap();
        let failure = prepare_startup(&reopened, &config, &[], &SystemClock, shutdown.as_mut())
            .await
            .err()
            .expect("bind failure");
        assert!(failure.to_string().contains("Address already in use"));
        finish_startup(&reopened, Err(failure)).await.unwrap_err();
        assert!(!reopened.is_healthy());
        let latest = Catalog::open(&dir.0).await.unwrap();
        latest.close().await.unwrap();
    }

    #[test]
    fn close_failure_preserves_primary_diagnostics_and_unstructured_errors() {
        for primary in [
            Ok(()),
            Err(phase(DiagnosticSource::Seed, "seed_dir", "invalid seed")),
            Err(io::Error::other("original failure")),
        ] {
            let original = primary.as_ref().err().map(ToString::to_string);
            let error = merge_close_result(
                primary,
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "close failed",
                )),
            )
            .unwrap_err();
            let diagnostics = error
                .get_ref()
                .unwrap()
                .downcast_ref::<Diagnostics>()
                .unwrap();
            assert!(
                diagnostics
                    .errors
                    .iter()
                    .any(|error| error.source == DiagnosticSource::Catalog
                        && error.key == "data_dir")
            );
            assert!(error.to_string().contains("PermissionDenied"));
            assert!(error.to_string().contains("close failed"));
            if let Some(original) = original {
                assert!(error.to_string().contains(&original));
                assert_eq!(diagnostics.errors.len(), 2);
            } else {
                assert_eq!(diagnostics.errors.len(), 1);
            }
        }
    }
}
