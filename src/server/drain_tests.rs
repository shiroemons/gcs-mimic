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
