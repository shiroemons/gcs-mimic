use super::*;
use crate::{
    auth::Auth,
    config::{Config, RequestEndpoints},
    server::{ObjectActivity, RequestBody, State},
    storage::Catalog,
    test_support::TestDir,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

struct Fixture {
    state: State,
    config: Config,
    endpoints: RequestEndpoints,
    activity: watch::Sender<ObjectActivity>,
    _dir: TestDir,
}
impl Fixture {
    async fn new() -> Self {
        let dir = TestDir::new();
        let state = State::for_test(Catalog::open(&dir.0).await.unwrap(), Auth::permissive());
        let config = Config::default();
        let endpoints = RequestEndpoints::explicit("http://localhost");
        let (activity, _) = watch::channel(ObjectActivity::default());
        Self {
            state,
            config,
            endpoints,
            activity,
            _dir: dir,
        }
    }
    fn ctx(&self) -> ObjectCtx<'_> {
        ObjectCtx {
            state: &self.state,
            config: &self.config,
            endpoints: &self.endpoints,
            activity: &self.activity,
            caller: None,
            deadline: tokio::time::Instant::now() + Duration::from_secs(5),
        }
    }
}

#[tokio::test]
async fn terminal_chunk_reaches_a_consumer_started_by_the_first_poll() {
    let fixture = Fixture::new().await;
    for _ in 0..64 {
        let (sender, mut receiver) = upload_channel();
        let upload = async {
            let mut size = 0;
            while let Some(chunk) = receiver.recv().await {
                match chunk.unwrap() {
                    UploadChunk::Data(bytes) => size += bytes.len(),
                    UploadChunk::End => return Ok::<_, ObjectError>(size),
                    UploadChunk::EndWithHashes(_) => panic!("unexpected terminal hashes"),
                }
            }
            panic!("missing terminal chunk");
        };
        let producer = async {
            let body = RequestBody::Buffered(http_body_util::Full::new(bytes::Bytes::from(
                vec![0; 128 * 1024],
            )));
            crate::server::stream_upload(body, &sender, 128 * 1024, &fixture.activity, None)
                .await?;
            Ok::<_, ObjectError>(UploadEnd::Eof)
        };
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            drive_upload(
                CatalogWait::Upload(fixture.ctx()),
                upload,
                &sender,
                producer,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result, 128 * 1024);
    }
    fixture.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn abort_waits_for_cleanup_and_preserves_the_original_adapter_error() {
    let fixture = Fixture::new().await;
    for disk_full in [false, true] {
        let (sender, mut receiver) = upload_channel();
        let cleaned = AtomicBool::new(false);
        let upload = async {
            let Err(error) = receiver.recv().await.unwrap() else {
                panic!("expected body abort");
            };
            assert!(error.get_ref().unwrap().is::<AdapterRejectedBody>());
            tokio::task::yield_now().await;
            cleaned.store(true, Ordering::Release);
            Err::<(), _>(if disk_full {
                ObjectError::Io(io::ErrorKind::StorageFull.into())
            } else {
                ObjectError::body_aborted(error)
            })
        };
        let producer = async { Err::<UploadEnd, _>("original adapter error") };
        let result = drive_upload(
            CatalogWait::Upload(fixture.ctx()),
            upload,
            &sender,
            producer,
        )
        .await;
        assert!(cleaned.load(Ordering::Acquire));
        if disk_full {
            assert!(matches!(
                result,
                Err(DriveError::Upload(ObjectError::Io(_)))
            ));
        } else {
            assert!(matches!(
                result,
                Err(DriveError::Producer("original adapter error"))
            ));
        }
    }
    fixture.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn upload_failure_first_drops_the_producer() {
    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let fixture = Fixture::new().await;
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = DropFlag(dropped.clone());
    let producer = async move {
        let _flag = flag;
        std::future::pending::<Result<UploadEnd, ObjectError>>().await
    };
    let (sender, _) = upload_channel();
    let result = drive_upload(
        CatalogWait::Upload(fixture.ctx()),
        async { Err::<(), _>(ObjectError::TooLarge) },
        &sender,
        producer,
    )
    .await;
    assert!(matches!(
        result,
        Err(DriveError::Upload(ObjectError::TooLarge))
    ));
    assert!(dropped.load(Ordering::Acquire));
    fixture.state.catalog.close().await.unwrap();
}

#[tokio::test]
async fn session_wait_uses_the_session_budget() {
    let mut fixture = Fixture::new().await;
    fixture.config.catalog_wait_timeout_ms = 1;
    let (_progress, updates) = watch::channel(false);
    let result = CatalogWait::Session {
        ctx: fixture.ctx(),
        progress: updates,
    }
    .settle(async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok::<_, ObjectError>(())
    })
    .await;
    result.unwrap();
    fixture.state.catalog.close().await.unwrap();
}
