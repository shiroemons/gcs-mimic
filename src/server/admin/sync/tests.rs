use super::*;
use crate::json::JsonErrorResponse;
use crate::storage::test_fixtures::PERMISSIVE;
use crate::sync::SyncError;
use crate::{
    auth::{Auth, Registry},
    clock::SystemClock,
    config::Config,
    server::{ObjectActivity, State},
    storage::{
        Catalog,
        admin::{BucketDefinition, BucketName, Generation, ObjectName, Origin},
        sync_state::{
            observation::PauseReason,
            writeback::{Limits, Stop, Writeback},
        },
    },
    test_support::{TestDir, eventually_async, eventually_some_async, joined},
};
use http_body_util::BodyExt;
use serde_json::json;
use std::{fs, io, time::Duration};

/// A control failure whose typed cause arrives inside a backend cause, as storage carries it.
fn failed_behind_backend_cause(error: impl Into<crate::storage::failure::Source>) -> ControlError {
    ControlError::Failed(SyncError::Admin(AdminError::backend(error)))
}

#[test]
fn nested_control_errors_keep_status_and_text() {
    for error in [
        RootError::Admin(crate::storage::admin::AdminError::Forbidden),
        RootError::Admin(crate::storage::admin::AdminError::Precondition),
        RootError::Admin(crate::storage::admin::AdminError::Corrupt("corrupt".into())),
        RootError::Admin(AdminError::DiskFull(std::io::ErrorKind::StorageFull.into())),
        RootError::Admin(AdminError::Io(io::ErrorKind::PermissionDenied.into())),
        RootError::InUse,
    ] {
        let label = format!("{error:?}");
        let expected = root_error(&error);
        let actual = control_error(&failed_behind_backend_cause(error));
        assert_eq!(actual.status, expected.status, "{label}");
        assert_eq!(actual.message, expected.message, "{label}");
    }
}

/// Each decision route takes its own decision, spelled out here rather than taken from the code
/// under test: a rejection taken as an approval would delete the objects it was to keep. A
/// route that only resembles one takes none.
#[test]
fn each_decision_route_takes_its_own_decision() {
    for (path, decision) in [
        ("/_internal/sync/approve", Decision::Approve),
        ("/_internal/sync/reject", Decision::Reject),
    ] {
        assert!(
            Control::parse(path) == Some(Control::Decide(decision)),
            "{path}"
        );
    }
    for path in [
        "/_internal/sync/approve/",
        "/_internal/sync/rejected",
        "/_internal/sync/decide",
        "/_internal/sync/Reject",
    ] {
        assert!(Control::parse(path).is_none(), "{path}");
    }
}

/// A stopped mirror never ran the control, so it answers as a closed queue. A paused one, and a
/// rescan that failed to resume it, answer 409 with what resumes synchronization instead of
/// asking to retry a busy queue: a failed rescan names the reason it stays paused and its cause.
#[test]
fn controls_answer_with_what_resumes_synchronization() {
    let busy = busy();
    let stopped = control_error(&ControlError::Shutdown);
    assert_eq!(
        (stopped.status, stopped.reason, &stopped.message),
        (busy.status, busy.reason, &busy.message)
    );
    let paused = control_error(&ControlError::Paused);
    assert_eq!(paused.status, StatusCode::CONFLICT);
    assert!(
        paused
            .message
            .ends_with("then resume synchronization: gcs-mimic sync rescan --endpoint URL"),
        "{}",
        paused.message
    );
    let limit = || {
        SyncError::Admin(AdminError::Invalid(
            "sync tree exceeds 3 inspected entries".to_owned(),
        ))
    };
    // A running rescan that paused, and a resume that failed again for good.
    let began = control_error(&ControlError::RescanFailed {
        reason: PauseReason::ObserverFailure,
        change: PauseChange::Began,
        error: limit(),
    });
    let recorded = control_error(&ControlError::RescanFailed {
        reason: PauseReason::ObserverFailure,
        change: PauseChange::Recorded,
        error: limit(),
    });
    // A resume that failed transiently keeps the reason and may simply be run again.
    let kept = control_error(&ControlError::RescanFailed {
        reason: PauseReason::IoFailure,
        change: PauseChange::Kept,
        error: SyncError::Admin(AdminError::Busy {
            retry_after: Duration::from_secs(1),
        }),
    });
    let retry = crate::sync::report::remedy::resume_retry_remedy();
    for (failed, parts) in [
        (
            &began,
            [
                "synchronization is now paused with paused_reason observer-failure: ",
                "sync tree exceeds 3 inspected entries",
                "such as max_sync_entries, needs a restart",
            ],
        ),
        (
            &recorded,
            [
                "synchronization stays paused with paused_reason observer-failure: ",
                "sync tree exceeds 3 inspected entries",
                "such as max_sync_entries, needs a restart",
            ],
        ),
        (
            &kept,
            [
                "synchronization stays paused with paused_reason IoFailure: ",
                "Busy",
                retry.as_str(),
            ],
        ),
    ] {
        assert_eq!(failed.status, StatusCode::CONFLICT, "{}", failed.message);
        assert_ne!(failed.message, busy.message);
        for part in parts {
            assert!(failed.message.contains(part), "{part}: {}", failed.message);
        }
    }
    for reason in PauseReason::ALL {
        assert!(
            crate::sync::report::remedy::pause_remedy(
                reason,
                crate::sync::report::command::AtHand::default()
            )
            .ends_with(": gcs-mimic sync rescan --endpoint URL"),
            "{reason:?}"
        );
    }
}

#[test]
fn storage_failures_require_capacity_or_access_repairs() {
    for (kind, reason, message) in [
        (
            io::ErrorKind::StorageFull,
            "disk_full",
            "Free disk space without deleting synchronization data, then retry",
        ),
        (
            io::ErrorKind::PermissionDenied,
            "io_failure",
            "Restore storage access, then retry",
        ),
    ] {
        for (path, actual) in [
            ("root", root_error(&RootError::from(io::Error::from(kind)))),
            (
                "control",
                control_error(&ControlError::Failed(SyncError::Admin(AdminError::from(
                    io::Error::from(kind),
                )))),
            ),
            (
                "nested control",
                control_error(&failed_behind_backend_cause(AdminError::from(
                    io::Error::from(kind),
                ))),
            ),
        ] {
            assert_eq!(
                actual.status,
                StatusCode::SERVICE_UNAVAILABLE,
                "{reason} {path}"
            );
            assert_eq!(actual.reason, reason, "{reason} {path}");
            assert_eq!(actual.message, message, "{reason} {path}");
        }
    }
}

#[test]
fn corrupt_control_causes_require_restoring_the_data_directory() {
    let actual = control_error(&failed_behind_backend_cause(
        crate::storage::admin::AdminError::Corrupt("corrupt".into()),
    ));
    assert_eq!(actual.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        actual.message,
        "Object storage is unavailable: stored data is corrupt; restore a valid data directory"
    );
}

#[test]
fn recovered_writeback_stops_keep_the_admin_busy_response() {
    let expected = busy();
    for stop in [
        Stop::ConflictCapacity,
        Stop::IoFailure,
        Stop::RecoveryRequired,
        Stop::ProtectedBody,
    ] {
        let actual = control_error(&ControlError::Failed(SyncError::Recovery(stop)));
        assert_eq!(actual.status, expected.status);
        assert_eq!(actual.message, expected.message);
    }
}

struct AuthorizationHook {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
#[derive(Default)]
struct AuthorizationWaitHook {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    observer: std::sync::Mutex<Option<tokio::sync::watch::Receiver<ObjectActivity>>>,
}
tokio::task_local! {static AUTHORIZATION_WAIT_HOOK:Arc<AuthorizationWaitHook>;}
fn awaiting_catalog(observer: &tokio::sync::watch::Receiver<ObjectActivity>) -> bool {
    observer.borrow().stage == crate::server::RequestStage::AwaitingCatalog
}

pub(super) async fn authorization_wait_checkpoint(
    activity: &tokio::sync::watch::Sender<ObjectActivity>,
) {
    if let Ok(hook) = AUTHORIZATION_WAIT_HOOK.try_with(Arc::clone) {
        *hook.observer.lock().unwrap() = Some(activity.subscribe());
        hook.entered.notify_one();
        hook.release.notified().await;
    }
}

#[tokio::test]
async fn wait_authorization_deadline_and_cancellation_release_activity() {
    enum End {
        Deadline,
        Cancel,
    }
    for end in [End::Deadline, End::Cancel] {
        let mut fixture = Fixture::new(Tree::Empty).await;
        fixture.config.catalog_wait_timeout =
            crate::time_limit::TimeLimit::from_millis(match end {
                End::Deadline => 20,
                End::Cancel => 30_000,
            });
        let hook = Arc::new(AuthorizationWaitHook::default());
        let timeout = match end {
            End::Deadline => Duration::from_millis(500),
            End::Cancel => Duration::from_millis(20),
        };
        let result = tokio::time::timeout(
            timeout,
            AUTHORIZATION_WAIT_HOOK.scope(
                hook.clone(),
                fixture.request_once("POST", "/_internal/sync/wait", b"{}", None),
            ),
        )
        .await;
        match end {
            End::Deadline => assert_eq!(
                result.unwrap().unwrap_err().status,
                StatusCode::SERVICE_UNAVAILABLE
            ),
            End::Cancel => assert!(result.is_err()),
        }
        let observer = hook.observer.lock().unwrap().take().unwrap();
        assert!(!awaiting_catalog(&observer));
        assert!(observer.borrow().deadline.is_none());
        fixture.finish().await;
    }
}

/// The version is checked before authentication and any catalog wait: under the wait hook, a
/// request that reached authorization would block until released, so each must return alone.
#[tokio::test]
async fn a_client_of_another_release_is_refused_before_authorization_or_any_wait() {
    let fixture = Fixture::new(Tree::Empty).await;
    let hook = Arc::new(AuthorizationWaitHook::default());
    for version in [Some("0.0.1"), None] {
        for (method, path, payload) in [
            ("GET", "/_internal/sync", &b""[..]),
            ("POST", "/_internal/sync/wait", b"{}"),
            ("POST", "/_internal/sync/approve", b"{}"),
            ("POST", "/_internal/sync/reject", b"{}"),
        ] {
            let refused = AUTHORIZATION_WAIT_HOOK.scope(
                hook.clone(),
                fixture.send(method, path, payload, None, version),
            );
            let error = tokio::time::timeout(Duration::from_secs(10), refused)
                .await
                .unwrap_or_else(|_| panic!("{path} with {version:?} reached authorization"))
                .unwrap_err();
            assert_eq!(error.status, StatusCode::BAD_REQUEST, "{path} {version:?}");
            assert_eq!(error.reason, "versionMismatch", "{path} {version:?}");
            assert!(error.message.contains(crate::VERSION), "{}", error.message);
        }
    }
    fixture.finish().await;
}

#[tokio::test]
async fn wait_authorization_claims_a_bounded_catalog_wait_and_releases_it_on_errors() {
    let fixture = Fixture::new(Tree::Empty).await;
    let mut observations = Vec::new();
    for payload in [&b"{}"[..], &b"{"[..]] {
        let hook = Arc::new(AuthorizationWaitHook::default());
        let (result, (claimed, bounded, observer)) = AUTHORIZATION_WAIT_HOOK
            .scope(hook.clone(), async {
                tokio::join!(
                    fixture.request_once("POST", "/_internal/sync/wait", payload, None),
                    async {
                        hook.entered.notified().await;
                        let observer = hook.observer.lock().unwrap().take().unwrap();
                        let claimed = awaiting_catalog(&observer);
                        let bounded = observer.borrow().deadline.is_some();
                        hook.release.notify_one();
                        (claimed, bounded, observer)
                    }
                )
            })
            .await;
        if payload == b"{}" {
            assert_eq!(result.unwrap().status(), StatusCode::OK)
        } else {
            assert_eq!(result.unwrap_err().status, StatusCode::BAD_REQUEST)
        }
        observations.push((claimed, bounded, awaiting_catalog(&observer)));
    }
    fixture.finish().await;
    for (claimed, bounded, active) in observations {
        assert!(claimed);
        assert!(bounded);
        assert!(!active);
    }
}
tokio::task_local! {static AUTHORIZATION_HOOK:Arc<AuthorizationHook>;}
pub(super) async fn authorization_checkpoint() {
    if let Ok(hook) = AUTHORIZATION_HOOK.try_with(Arc::clone) {
        hook.entered.notify_one();
        hook.release.notified().await;
    }
}

#[test]
fn queued_native_authority_and_condition_errors_keep_their_http_status() {
    assert_eq!(
        control_error(&ControlError::Failed(SyncError::Writeback(
            crate::storage::sync_state::writeback::Error::Root(RootError::Admin(
                crate::storage::admin::AdminError::Forbidden
            ))
        )))
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        control_error(&ControlError::Failed(SyncError::Admin(
            crate::storage::admin::AdminError::Forbidden
        )))
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        control_error(&ControlError::Failed(SyncError::Admin(
            crate::storage::admin::AdminError::Precondition
        )))
        .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        control_error(&failed_behind_backend_cause(
            crate::storage::admin::AdminError::Forbidden
        ))
        .status,
        StatusCode::FORBIDDEN
    );
}

struct Fixture {
    _data: TestDir,
    tree: TestDir,
    state: std::sync::Arc<State>,
    config: Config,
    task: crate::sync::Task,
}

enum Tree {
    Empty,
    EmptyBucket,
    /// The bucket directory holds `first` and `second`.
    Pair,
    LongNames,
}

struct ArmedWorkspace {
    root: Arc<RootLease>,
    writeback: Writeback,
    bucket: BucketName,
    name: ObjectName,
    winner: crate::object::ObjectRecord,
    id: String,
    path: std::path::PathBuf,
}

impl Fixture {
    async fn new(shape: Tree) -> Self {
        let data = TestDir::new();
        let tree = TestDir::new();
        if matches!(shape, Tree::EmptyBucket | Tree::Pair) {
            fs::create_dir(tree.0.join("test-bucket")).unwrap();
        }
        if matches!(shape, Tree::Pair) {
            for name in PAIR {
                fs::write(tree.0.join("test-bucket").join(name), b"body").unwrap();
            }
        }
        if matches!(shape, Tree::LongNames) {
            let component = format!("{}{}", "a".repeat(100), "\"".repeat(100));
            let prefix = tree
                .0
                .join("test-bucket")
                .join(&component)
                .join(&component)
                .join(component);
            fs::create_dir_all(&prefix).unwrap();
            for index in 0..300 {
                fs::write(
                    prefix.join(format!("{index:04}{}", "b".repeat(216))),
                    b"body",
                )
                .unwrap();
            }
        }
        let catalog = Catalog::open(&data.0).await.unwrap();
        let config = Config {
            data_dir: data.0.clone(),
            sync_dir: Some(tree.0.clone()),
            ..Default::default()
        };
        let root = crate::sync::preflight(&catalog, &config)
            .await
            .unwrap()
            .unwrap();
        let task = crate::sync::start(catalog.clone(), root, &config).unwrap();
        assert!(task.rescan().await.unwrap().baseline_complete);
        let state = std::sync::Arc::new(State {
            api_base_url: "http://localhost".into(),
            ..State::for_test(catalog, Auth::permissive())
        });
        Self {
            _data: data,
            tree,
            state,
            config,
            task,
        }
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        payload: &[u8],
        token: Option<&str>,
    ) -> Result<Response<Full<Bytes>>, ApiError> {
        self.request_once(method, path, payload, token).await
    }

    async fn request_once(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        token: Option<&str>,
    ) -> Result<Response<Full<Bytes>>, ApiError> {
        self.send(method, path, body, token, Some(crate::VERSION))
            .await
    }

    /// A control request that reports `version` as the client's release, or none at all.
    async fn send(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        token: Option<&str>,
        version: Option<&str>,
    ) -> Result<Response<Full<Bytes>>, ApiError> {
        let endpoints = self
            .config
            .runtime_endpoints
            .snapshot(&self.config, &self.state.api_base_url)
            .unwrap()
            .without_request();
        let (activity, _) = tokio::sync::watch::channel(ObjectActivity::default());
        let mut request = Request::builder().method(method).uri(path);
        if let Some(version) = version {
            request = request.header(handshake::VERSION_HEADER, version);
        }
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let request = request
            .body(RequestBody::Buffered(Full::new(Bytes::copy_from_slice(
                body,
            ))))
            .unwrap();
        // Tests change the fixture's settings between requests, so each request shares a copy.
        let config = Arc::new(self.config.clone());
        control(
            request,
            RequestCtx {
                state: &self.state,
                config: &config,
                endpoints: &endpoints,
                activity: &activity,
                listener: crate::auth::ListenerRole::Public,
            },
        )
        .await
    }

    async fn status(&self) -> Value {
        body(
            self.request("GET", "/_internal/sync", b"", None)
                .await
                .unwrap(),
        )
        .await
    }

    /// Reads status and then the findings page for its revision. Status reports the
    /// published observation while the page checks the current one, so a native
    /// notification still settling can make the just-read revision stale (409);
    /// only that answer is retried, with a fresh status.
    async fn findings(&self) -> (Value, Value) {
        eventually_some_async("a findings page for a current revision", || async {
            let status = self.status().await;
            let path = format!(
                "/_internal/sync?view=findings&revision={}",
                status["findings_revision"].as_str().unwrap()
            );
            match self.request("GET", &path, b"", None).await {
                Ok(page) => Some((status, body(page).await)),
                Err(error) if error.status == StatusCode::CONFLICT => None,
                Err(error) => panic!("findings page: {error:?}"),
            }
        })
        .await
    }

    async fn armed_workspace(&self, bytes: &[u8]) -> ArmedWorkspace {
        fs::create_dir(self.tree.0.join("test-bucket")).unwrap();
        fs::write(self.tree.0.join("test-bucket/body"), b"API winner").unwrap();
        self.task.rescan().await.unwrap();
        let (_, root) = self.state.catalog.sync_controls().unwrap().unwrap();
        let bucket = BucketName::new("test-bucket").unwrap();
        let name = ObjectName::new("body").unwrap();
        let winner = self
            .state
            .catalog
            .admin(Origin::Api)
            .get_object(&bucket, &name, None)
            .await
            .unwrap();
        let writeback = root
            .writeback(
                Limits::new(
                    crate::sync::CONFLICT_MAX_BYTES,
                    crate::sync::CONFLICT_MAX_ENTRIES,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        let fence = self.state.catalog.current_fence().await.unwrap();
        let pending = crate::storage::sync_state::writeback::armed_observation(
            &writeback,
            &bucket,
            &name,
            Generation::new(winner.generation).unwrap(),
            fence,
        )
        .await;
        let id = pending.token.id().to_string();
        let path = self.tree.0.join(crate::sync::mapping::WORKSPACE).join(&id);
        fs::write(&path, bytes).unwrap();
        writeback.recover_pending().await.unwrap();
        ArmedWorkspace {
            root,
            writeback,
            bucket,
            name,
            winner,
            id,
            path,
        }
    }

    async fn finish(self) {
        self.task.finish().await.unwrap();
        self.state.catalog.close().await.unwrap();
    }

    /// Publishes `bytes` as `name` in `test-bucket` through the API, creating the bucket first.
    async fn publish(&self, name: &str, bytes: &'static [u8]) -> crate::object::ObjectRecord {
        use crate::{
            object::UploadChunk,
            storage::admin::{Publication, PublishMode},
        };
        let bucket = BucketName::new("test-bucket").unwrap();
        let admin = self.state.catalog.admin(Origin::Api);
        admin
            .ensure_bucket(
                BucketDefinition::default_for(&bucket, &self.config.seed_project, &SystemClock)
                    .unwrap(),
            )
            .await
            .unwrap();
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        sender
            .try_send(Ok(UploadChunk::Data(Bytes::from_static(bytes))))
            .unwrap();
        sender.try_send(Ok(UploadChunk::End)).unwrap();
        admin
            .publish_object(
                &bucket,
                &ObjectName::new(name).unwrap(),
                Publication {
                    content_type: "application/octet-stream".into(),
                    metadata: Default::default(),
                    expected: Default::default(),
                    condition: None,
                    max_bytes: 4096,
                    mode: PublishMode::Replace,
                },
                receiver,
            )
            .await
            .unwrap()
    }

    /// Sends a wait with `payload` and returns the status and the answer.
    async fn wait(&self, payload: &[u8]) -> (StatusCode, Value) {
        let response = self
            .request("POST", "/_internal/sync/wait", payload, None)
            .await
            .unwrap();
        (response.status(), body(response).await)
    }
}

#[track_caller]
fn status_of(result: Result<Response<Full<Bytes>>, ApiError>) -> StatusCode {
    result.unwrap_err().status
}

async fn body(response: Response<Full<Bytes>>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(bytes.len() <= RESPONSE_LIMIT);
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn response_limit_counts_the_exact_rendered_bytes() {
    let overhead = json::render_body(&json!({"items": ""})).len();
    let value = json!({"items": "x".repeat(RESPONSE_LIMIT - overhead)});
    let bytes = bounded(&value)
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(bytes.len(), RESPONSE_LIMIT);
    assert_eq!(bytes.as_ref(), json::render_body(&value).as_bytes());
    let oversized = json!({"items": "x".repeat(RESPONSE_LIMIT - overhead + 1)});
    assert!(oversized.to_string().len() <= RESPONSE_LIMIT);
    assert_eq!(
        status_of(bounded(&oversized)),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn large_conflict_pages_fit_rendered_limit_and_resume_after_the_last_returned_item() {
    use crate::storage::{admin::ObjectName, sync_state::conflicts::CaptureRequest};
    let fixture = Fixture::new(Tree::Empty).await;
    let bucket = BucketName::new("test-bucket").unwrap();
    fixture
        .state
        .catalog
        .admin(Origin::Api)
        .ensure_bucket(
            BucketDefinition::default_for(&bucket, &fixture.config.seed_project, &SystemClock)
                .unwrap(),
        )
        .await
        .unwrap();
    let (_, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    let binding = fixture
        .state
        .catalog
        .sync_state()
        .binding(&bucket)
        .await
        .unwrap();
    let source = TestDir::new();
    fs::write(source.0.join("body"), b"retained").unwrap();
    let mut expected = std::collections::BTreeSet::new();
    for index in 0..200 {
        let name = format!("{index:04}{}", "a".repeat(1020));
        expected.insert(name.clone());
        root.capture_conflict(
            CaptureRequest {
                binding: binding.clone(),
                name: ObjectName::new(name).unwrap(),
                expected_generation: None,
                source_op: None,
            },
            fs::File::open(source.0.join("body")).unwrap(),
        )
        .await
        .unwrap();
    }
    let native = root
        .conflict_page(
            crate::storage::sync_state::conflicts::ConflictPosition::Start,
            PageLimit::LARGEST,
            ControlAuthority::new(fixture.config.seed_project.clone(), None),
        )
        .await
        .unwrap();
    assert!(serde_json::to_vec(&native.items).unwrap().len() <= 240_256);
    assert!(json::render_body(&json!({"items":native.items})).len() > RESPONSE_LIMIT);
    let mut found = std::collections::BTreeSet::new();
    let mut path = "/_internal/sync/conflicts".to_owned();
    loop {
        let response = fixture.request("GET", &path, b"", None).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page = body(response).await;
        for item in page["items"].as_array().unwrap() {
            assert!(found.insert(item["object"].as_str().unwrap().to_owned()));
        }
        let Some(cursor) = page["next_cursor"].as_str() else {
            break;
        };
        path = format!("/_internal/sync/conflicts?cursor={cursor}");
    }
    assert_eq!(found, expected);
    drop(root);
    fixture.finish().await;
}

#[tokio::test]
async fn unmappable_source_entries_have_a_durable_root_finding_until_repaired() {
    let fixture = Fixture::new(Tree::Empty).await;
    fs::create_dir(fixture.tree.0.join("INVALID-BUCKET")).unwrap();
    fs::write(
        fixture.tree.0.join("INVALID-BUCKET/body"),
        b"source survives",
    )
    .unwrap();
    fixture.task.rescan().await.unwrap();
    let (status, page) = fixture.findings().await;
    assert_eq!(status["state"], "SourceUnrepresentable");
    let path = format!(
        "/_internal/sync?view=findings&revision={}",
        status["findings_revision"].as_str().unwrap()
    );
    assert_eq!(page["total_count"], 1);
    assert_eq!(page["items"][0]["scope"], "root");
    assert_eq!(page["items"][0]["reason"], "SourceUnrepresentable");
    assert!(page["items"][0]["bucket"].is_null());
    assert!(page["items"][0]["object"].is_null());
    assert!(page["next_cursor"].is_null());
    // A wait names the entries its own pass could not take in, and the status those of the
    // latest complete scan.
    let (code, waited) = fixture.wait(br#"{"timeout_ms":10000}"#).await;
    assert_eq!(code, StatusCode::CONFLICT, "{waited}");
    assert_eq!(waited["reason"], "SourceUnrepresentable");
    let entries = json!({"total":2,"entries":[
        {"path":"INVALID-BUCKET","reason":"unmappable-name"},
        {"path":"INVALID-BUCKET/body","reason":"unmappable-name"}],"omitted":0});
    assert_eq!(waited["source_entries"], entries);
    assert_eq!(status["source_entries"], entries);
    fs::rename(
        fixture.tree.0.join("INVALID-BUCKET"),
        fixture.tree.0.join("valid-bucket"),
    )
    .unwrap();
    fixture.task.rescan().await.unwrap();
    assert_eq!(
        fs::read(fixture.tree.0.join("valid-bucket/body")).unwrap(),
        b"source survives"
    );
    assert_eq!(
        status_of(fixture.request("GET", &path, b"", None).await),
        StatusCode::CONFLICT
    );
    assert!(fixture.status().await["observation"]["scan_stop"].is_null());
    fixture.finish().await;
}

/// A wait over a tree that holds entries whose names are not UTF-8, a directory and a symlink,
/// answers with them named, rather than failing its answer or the log that names the symlink.
/// macOS file systems refuse such names, so only Linux can place them.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_wait_names_source_entries_whose_names_are_not_utf8() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new(Tree::Empty).await;
    let name = |bytes: &[u8]| fixture.tree.0.join(std::ffi::OsStr::from_bytes(bytes));
    fs::create_dir(name(b"caf\xe9")).unwrap();
    std::os::unix::fs::symlink("target", name(b"link\xff")).unwrap();
    fixture.task.rescan().await.unwrap();
    let (code, waited) = fixture.wait(br#"{"timeout_ms":10000}"#).await;
    assert_eq!(code, StatusCode::CONFLICT, "{waited}");
    assert_eq!(waited["reason"], "SourceUnrepresentable");
    assert_eq!(waited["source_entries"]["total"], 2, "{waited}");
    let mut named: Vec<(String, String)> = waited["source_entries"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["path"].as_str().unwrap().to_owned(),
                entry["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    named.sort();
    assert_eq!(
        named,
        [
            ("caf\u{fffd}".to_owned(), "unmappable-name".to_owned()),
            ("link\u{fffd}".to_owned(), "unsafe-entry".to_owned()),
        ]
    );
    fixture.finish().await;
}

/// The status lists the source entries the latest complete scan could not take in by path and
/// reason, and none once a scan no longer finds them.
#[tokio::test]
async fn a_status_lists_the_source_entries_the_latest_scan_could_not_take_in() {
    let fixture = Fixture::new(Tree::Empty).await;
    let directory = fixture.tree.0.join("test-bucket");
    fs::create_dir(&directory).unwrap();
    std::os::unix::fs::symlink("target", directory.join("link")).unwrap();
    fixture.task.rescan().await.unwrap();
    assert_eq!(
        fixture.status().await["source_entries"],
        json!({"total":1,"entries":[{"path":"test-bucket/link","reason":"unsafe-entry"}],"omitted":0})
    );
    fs::remove_file(directory.join("link")).unwrap();
    fixture.task.rescan().await.unwrap();
    assert_eq!(
        fixture.status().await["source_entries"],
        json!({"total":0,"entries":[],"omitted":0})
    );
    fixture.finish().await;
}

/// A status names only as many source entries as fit its answer, at most the bound the report
/// keeps, and counts the rest as omitted rather than failing.
#[tokio::test]
async fn a_status_names_as_many_source_entries_as_fit_its_answer() {
    let mut report = EntryReport::default();
    let long = "x".repeat(2000);
    for index in 0..EntryReport::LIMIT + 10 {
        report.record(
            std::path::Path::new(&format!("bucket/{long}{index}")),
            crate::sync::entries::EntryProblem::UnsafeEntry,
        );
    }
    let response = render_listing(&report, |listing| {
        super::super::encode(&json!({ "source_entries": listing }))
    })
    .unwrap();
    let listing = body(response).await["source_entries"].take();
    let named = listing["entries"].as_array().unwrap().len();
    assert!((1..EntryReport::LIMIT).contains(&named), "{named}");
    assert_eq!(listing["total"], EntryReport::LIMIT + 10);
    assert_eq!(listing["omitted"], EntryReport::LIMIT + 10 - named);

    let mut short = EntryReport::default();
    for index in 0..EntryReport::LIMIT + 10 {
        short.record(
            std::path::Path::new(&format!("bucket/link{index}")),
            crate::sync::entries::EntryProblem::UnsafeEntry,
        );
    }
    let response = render_listing(&short, |listing| {
        super::super::encode(&json!({ "source_entries": listing }))
    })
    .unwrap();
    let listing = body(response).await["source_entries"].take();
    assert_eq!(
        listing["entries"].as_array().unwrap().len(),
        EntryReport::LIMIT
    );
    assert_eq!(listing["omitted"], 10);
}

#[tokio::test]
async fn live_rescan_registers_a_protected_workspace_before_resuming_intake() {
    let fixture = Fixture::new(Tree::Empty).await;
    let ArmedWorkspace {
        root,
        writeback,
        bucket,
        name,
        winner,
        path,
        ..
    } = fixture.armed_workspace(b"retained external source").await;
    fixture.task.rescan().await.unwrap();
    assert!(!path.exists());
    assert_eq!(writeback.status().await.unwrap().entries, 0);
    let page = body(
        fixture
            .request("GET", "/_internal/sync/conflicts", b"", None)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    let conflict = &page["items"][0];
    assert_eq!(conflict["owner_project"], fixture.config.seed_project);
    assert_eq!(conflict["generation"], winner.generation);
    assert_eq!(
        fs::read(
            fixture
                ._data
                .0
                .join("sync-conflicts")
                .join(conflict["conflict_id"].as_str().unwrap())
        )
        .unwrap(),
        b"retained external source"
    );
    assert_eq!(
        fixture
            .state
            .catalog
            .admin(Origin::Api)
            .get_object(&bucket, &name, None)
            .await
            .unwrap(),
        winner
    );
    assert_eq!(
        fs::read(fixture.tree.0.join("test-bucket/body")).unwrap(),
        b"API winner"
    );
    fixture.task.rescan().await.unwrap();
    assert_eq!(
        body(
            fixture
                .request("GET", "/_internal/sync/conflicts", b"", None)
                .await
                .unwrap()
        )
        .await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    drop((writeback, root));
    fixture.finish().await;
}

#[tokio::test]
async fn recovery_http_refreshes_changed_workspace_identity_and_requires_exact_discard_confirmation()
 {
    let fixture = Fixture::new(Tree::Empty).await;
    let ArmedWorkspace {
        root,
        writeback,
        bucket,
        name,
        winner,
        id,
        path,
    } = fixture.armed_workspace(b"protected old body").await;
    let (old, old_page) = fixture.findings().await;
    let old_revision = old["findings_revision"].as_str().unwrap();
    let old_path = format!("/_internal/sync?view=findings&revision={old_revision}");
    let original = old_page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["recovery_id"] == id)
        .unwrap();
    assert_eq!(original["retained_bytes"], 18);
    assert!(original["hashes"].is_null());
    fs::write(&path, b"changed protected body").unwrap();
    let (fresh, page) = fixture.findings().await;
    assert_ne!(fresh["findings_revision"], old["findings_revision"]);
    assert_eq!(
        status_of(fixture.request("GET", &old_path, b"", None).await),
        StatusCode::CONFLICT
    );
    let selected = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["recovery_id"] == id)
        .unwrap();
    assert_eq!(selected["retained_bytes"], 22);
    assert_ne!(selected["target_identity"], original["target_identity"]);
    let mut payload = json!({"recovery_id":id,"revision":selected["recovery_revision"],"target_identity":selected["target_identity"],"action":"discard","operation_id":"http-confirmed-recovery"});
    for explicit_null in [false, true] {
        if explicit_null {
            payload["confirmation"] = Value::Null;
        }
        let rejected = fixture
            .request(
                "POST",
                "/_internal/sync/resolve",
                &serde_json::to_vec(&payload).unwrap(),
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            rejected.status,
            StatusCode::BAD_REQUEST,
            "confirmation=null: {explicit_null}"
        );
        assert_eq!(
            rejected.reason, "recovery_confirmation_required",
            "confirmation=null: {explicit_null}"
        );
        assert_eq!(
            rejected.message,
            "Confirm backup_verified or discard_without_backup for the exact inspected recovery identity",
            "confirmation=null: {explicit_null}"
        );
    }
    payload["confirmation"] = json!("discard_without_backup");
    for mixed in ["conflict_id", "expected_generation"] {
        payload[mixed] = Value::Null;
        assert_eq!(
            status_of(
                fixture
                    .request(
                        "POST",
                        "/_internal/sync/resolve",
                        &serde_json::to_vec(&payload).unwrap(),
                        None
                    )
                    .await
            ),
            StatusCode::BAD_REQUEST,
            "{mixed}"
        );
        payload.as_object_mut().unwrap().remove(mixed);
    }
    payload["target_identity"] = original["target_identity"].clone();
    assert_eq!(
        status_of(
            fixture
                .request(
                    "POST",
                    "/_internal/sync/resolve",
                    &serde_json::to_vec(&payload).unwrap(),
                    None
                )
                .await
        ),
        StatusCode::CONFLICT
    );
    assert_eq!(fs::read(&path).unwrap(), b"changed protected body");
    payload["target_identity"] = selected["target_identity"].clone();
    let bytes = serde_json::to_vec(&payload).unwrap();
    let resolved = body(
        fixture
            .request("POST", "/_internal/sync/resolve", &bytes, None)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(resolved["completed"], true);
    assert!(!path.exists());
    assert_eq!(
        body(
            fixture
                .request("POST", "/_internal/sync/resolve", &bytes, None)
                .await
                .unwrap()
        )
        .await,
        resolved
    );
    assert_eq!(
        fixture
            .state
            .catalog
            .admin(Origin::Api)
            .get_object(&bucket, &name, None)
            .await
            .unwrap(),
        winner
    );
    assert_eq!(
        fs::read(fixture.tree.0.join("test-bucket/body")).unwrap(),
        b"API winner"
    );
    drop((writeback, root));
    fixture.finish().await;
}

#[tokio::test]
async fn strict_http_conflict_resolution_requires_exact_targets_and_preserves_api_conditions() {
    let mut fixture = Fixture::new(Tree::Empty).await;
    let bucket = BucketName::new("test-bucket").unwrap();
    let name = ObjectName::new("object").unwrap();
    let admin = fixture.state.catalog.admin(Origin::Api);
    let winner = fixture.publish("object", b"API winner").await;
    fixture.task.rescan().await.unwrap();
    let (_, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    let external = TestDir::new();
    fs::write(external.0.join("body"), b"retained\0\xff").unwrap();
    let captured = root
        .capture_conflict(
            crate::storage::sync_state::conflicts::CaptureRequest {
                binding: fixture
                    .state
                    .catalog
                    .sync_state()
                    .binding(&bucket)
                    .await
                    .unwrap(),
                name: name.clone(),
                expected_generation: Some(Generation::new(winner.generation).unwrap()),
                source_op: None,
            },
            fs::File::open(external.0.join("body")).unwrap(),
        )
        .await
        .unwrap();
    std::sync::Arc::get_mut(&mut fixture.state).unwrap().auth=Auth::strict(Registry::parse(&serde_json::to_vec(&json!({"identities":[
        {"id":"reader","tokens":["reader"],"project_roles":{fixture.config.seed_project.clone():["roles/storage.objectViewer"]}},
        {"id":"admin","tokens":["admin"],"project_roles":{fixture.config.seed_project.clone():["roles/storage.admin"]}}
    ]})).unwrap()).unwrap());
    for (token, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("reader"), StatusCode::FORBIDDEN),
    ] {
        assert_eq!(
            status_of(
                fixture
                    .request("GET", "/_internal/sync/conflicts", b"", token)
                    .await
            ),
            expected,
            "{token:?}"
        );
    }
    let page = body(
        fixture
            .request(
                "GET",
                "/_internal/sync/conflicts?limit=1",
                b"",
                Some("admin"),
            )
            .await
            .unwrap(),
    )
    .await;
    let conflict = &page["items"][0];
    assert_eq!(conflict["conflict_id"], captured.id().to_string());
    assert_eq!(conflict["generation"], winner.generation);
    assert!(page["next_cursor"].is_null());
    let mut payload = json!({"conflict_id":conflict["conflict_id"],"revision":conflict["revision"],"action":"restore","expected_generation":winner.generation,"operation_id":"http-restore"});
    for mixed in ["recovery_id", "target_identity", "confirmation"] {
        payload[mixed] = Value::Null;
        assert_eq!(
            status_of(
                fixture
                    .request(
                        "POST",
                        "/_internal/sync/resolve",
                        &serde_json::to_vec(&payload).unwrap(),
                        Some("admin")
                    )
                    .await
            ),
            StatusCode::BAD_REQUEST,
            "{mixed}"
        );
        payload.as_object_mut().unwrap().remove(mixed);
    }
    payload["expected_generation"] = json!(winner.generation + 1);
    assert_eq!(
        status_of(
            fixture
                .request(
                    "POST",
                    "/_internal/sync/resolve",
                    &serde_json::to_vec(&payload).unwrap(),
                    Some("admin")
                )
                .await
        ),
        StatusCode::CONFLICT
    );
    assert_eq!(
        admin.get_object(&bucket, &name, None).await.unwrap(),
        winner
    );
    payload["expected_generation"] = json!(winner.generation);
    let bytes = serde_json::to_vec(&payload).unwrap();
    let result = body(
        fixture
            .request("POST", "/_internal/sync/resolve", &bytes, Some("admin"))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(result["completed"], true);
    let generation = result["result_generation"].as_i64().unwrap();
    assert_ne!(generation, winner.generation);
    assert_eq!(
        body(
            fixture
                .request("POST", "/_internal/sync/resolve", &bytes, Some("admin"))
                .await
                .unwrap()
        )
        .await,
        result
    );
    let (_, reader) = admin.open_object(&bucket, &name, None).await.unwrap();
    let file = reader.into_file().unwrap();
    let mut bytes = [0; 10];
    assert_eq!(crate::fsutil::read_at(&file, &mut bytes, 0).unwrap(), 10);
    assert_eq!(&bytes, b"retained\0\xff");
    payload["action"] = json!("discard");
    assert_eq!(
        status_of(
            fixture
                .request(
                    "POST",
                    "/_internal/sync/resolve",
                    &serde_json::to_vec(&payload).unwrap(),
                    Some("admin")
                )
                .await
        ),
        StatusCode::CONFLICT
    );
    assert_eq!(
        fs::read(external.0.join("body")).unwrap(),
        b"retained\0\xff"
    );
    drop((root, admin, file));
    fixture.finish().await;
}

#[tokio::test]
async fn unknown_stored_pause_reasons_fail_status_and_findings_with_service_unavailable() {
    let fixture = Fixture::new(Tree::Empty).await;
    let status = fixture.status().await;
    let findings = format!(
        "/_internal/sync?view=findings&revision={}",
        status["findings_revision"].as_str().unwrap()
    );
    {
        let connection =
            rusqlite::Connection::open(fixture._data.0.join("catalog.sqlite3")).unwrap();
        connection
            .execute("UPDATE sync_observations SET paused_reason='bogus'", [])
            .unwrap();
    }
    let (_, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    assert!(matches!(
        root.observation().await,
        Err(RootError::Admin(
            crate::storage::admin::AdminError::Corrupt(_)
        ))
    ));
    for path in ["/_internal/sync", findings.as_str()] {
        let response = fixture
            .request("GET", path, b"", None)
            .await
            .unwrap_err()
            .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{path}");
        assert_eq!(
            body(response).await["error"]["message"],
            "Object storage is unavailable: stored data is corrupt; restore a valid data directory",
            "{path}"
        );
    }
    drop(root);
    fixture.finish().await;
}

#[tokio::test]
async fn findings_pages_reject_in_place_changes_and_foreign_sessions() {
    let fixture = Fixture::new(Tree::Empty).await;
    let (registration, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    root.observation_pause(Some(PauseReason::ObserverFailure))
        .await
        .unwrap();
    let status = fixture.status().await;
    assert_eq!(status["state"], "Paused");
    let revision = status["findings_revision"].as_str().unwrap();
    assert_eq!(revision.len(), 48);
    let path = format!("/_internal/sync?view=findings&revision={revision}&limit=1");
    let page = body(fixture.request("GET", &path, b"", None).await.unwrap()).await;
    assert_eq!(page["total_count"], 1);
    assert_eq!(page["items"][0]["category"], "root-stop");
    assert_eq!(page["items"][0]["reason"], "Paused");
    assert_eq!(page["items"][0]["detail_code"], "observer-failure");
    assert_eq!(page["items"][0]["scope"], "root");
    assert!(page["items"][0]["finding_id"].is_string());
    assert!(page["items"][0]["remedy"].is_string());
    assert!(page["next_cursor"].is_null());
    root.observation_pause(Some(PauseReason::IoFailure))
        .await
        .unwrap();
    assert_eq!(
        status_of(fixture.request("GET", &path, b"", None).await),
        StatusCode::CONFLICT
    );
    let current = fixture.status().await;
    let current_revision = current["findings_revision"].as_str().unwrap();
    let foreign = format!(
        "{}{}",
        if registration.session == "0".repeat(32) {
            "1".repeat(32)
        } else {
            "0".repeat(32)
        },
        &current_revision[32..]
    );
    assert_eq!(
        status_of(
            fixture
                .request(
                    "GET",
                    &format!("/_internal/sync?view=findings&revision={foreign}"),
                    b"",
                    None
                )
                .await
        ),
        StatusCode::CONFLICT
    );
    root.observation_pause(None).await.unwrap();
    drop((registration, root));
    fixture.finish().await;
}

#[tokio::test]
async fn protected_findings_pages_cover_every_target_with_a_final_null_cursor() {
    // A bucket exported during pagination can legitimately invalidate the findings revision.
    let fixture = Fixture::new(Tree::EmptyBucket).await;
    let waited = body(
        fixture
            .request("POST", "/_internal/sync/wait", b"{}", None)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(waited["reason"], "Ready");
    let (_, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    let binding = fixture
        .state
        .catalog
        .sync_state()
        .binding(&bucket)
        .await
        .unwrap();
    for index in 0..300 {
        root.record_path_stop(
            binding.clone(),
            crate::storage::admin::ObjectName::new(format!("{index:04}{}", "\"".repeat(900)))
                .unwrap(),
            crate::storage::sync_state::observation::PathStop::ProtectedDeletion,
        )
        .await
        .unwrap();
    }
    let status = fixture.status().await;
    assert_eq!(status["state"], "FindingsPending");
    let revision = status["findings_revision"].as_str().unwrap();
    let mut cursor = None;
    let mut names = std::collections::BTreeSet::new();
    let mut pages = 0;
    loop {
        let mut path = format!("/_internal/sync?view=findings&revision={revision}&limit=256");
        if let Some(cursor) = cursor {
            path.push_str(&format!("&cursor={cursor}"));
        }
        let page = body(fixture.request("GET", &path, b"", None).await.unwrap()).await;
        assert_eq!(page["total_count"], 300);
        let items = page["items"].as_array().unwrap();
        assert!(!items.is_empty());
        assert!(items.len() <= 256);
        for item in items {
            assert_eq!(item["reason"], "ProtectedDeletion");
            assert_eq!(item["scope"], "path");
            assert!(names.insert(item["object"].as_str().unwrap().to_owned()));
        }
        pages += 1;
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(names.len(), 300);
    assert!(pages > 1);
    root.clear_path_stop(
        binding,
        crate::storage::admin::ObjectName::new(format!("0000{}", "\"".repeat(900))).unwrap(),
        crate::storage::sync_state::observation::PathStop::ProtectedDeletion,
    )
    .await
    .unwrap();
    assert_eq!(
        status_of(
            fixture
                .request(
                    "GET",
                    &format!("/_internal/sync?view=findings&revision={revision}"),
                    b"",
                    None
                )
                .await
        ),
        StatusCode::CONFLICT
    );
    drop(root);
    fixture.finish().await;
}

#[tokio::test]
async fn controls_reject_invalid_input_and_unsupported_routes() {
    let fixture = Fixture::new(Tree::Empty).await;
    assert_eq!(fixture.status().await["auth_mode"], "permissive");
    for query in [
        "?limit=1",
        "?view=deletions",
        "?view=unknown&revision=0123456789abcdef0123456789abcdef",
        "?view=deletions&revision=0123456789abcdef0123456789abcdef&limit=0",
        "?view=deletions&revision=0123456789abcdef0123456789abcdef&limit=1&limit=2",
    ] {
        assert_eq!(
            status_of(
                fixture
                    .request("GET", &format!("/_internal/sync{query}"), b"", None)
                    .await
            ),
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
    let oversized = vec![b' '; 65_537];
    for (method, path, bytes, expected) in [
        (
            "GET",
            "/_internal/sync",
            b"x".as_slice(),
            StatusCode::BAD_REQUEST,
        ),
        (
            "PUT",
            "/_internal/sync",
            b"",
            StatusCode::METHOD_NOT_ALLOWED,
        ),
        (
            "POST",
            "/_internal/sync/rescan",
            br#"{"override":true}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/_internal/sync/approve",
            oversized.as_slice(),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
        (
            "GET",
            "/_internal/sync/conflicts",
            b"x",
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/_internal/sync/resolve",
            b"{}",
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/_internal/sync/wait",
            br#"{"timeout_ms":0}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/_internal/sync/wait",
            br#"{"timeout_ms":60001}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/_internal/sync/wait",
            br#"{"unknown":1}"#,
            StatusCode::BAD_REQUEST,
        ),
    ] {
        assert_eq!(
            status_of(fixture.request(method, path, bytes, None).await),
            expected,
            "{method} {path}, body length {}: {:?}",
            bytes.len(),
            &bytes[..bytes.len().min(64)]
        );
    }
    let waited = body(
        fixture
            .request("POST", "/_internal/sync/wait", b"{}", None)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(waited["completed"], true);
    assert_eq!(waited["timed_out"], false);
    assert_eq!(waited["reason"], "Ready");
    assert!(waited["journal_fence"]["sequence"].is_u64());
    let status = fixture.status().await;
    assert_eq!(status["state"], "Ready");
    assert_eq!(status["mirror_state"], "running");
    // The mirror's state decides the synchronization state; the server's readiness does not.
    fixture
        .state
        .ready
        .store(false, std::sync::atomic::Ordering::Release);
    assert_eq!(fixture.status().await["state"], "Ready");
    fixture.finish().await;
}

/// A name the tree cannot hold does not stop a wait: the wait succeeds, counts the object, and
/// returns the findings revision whose view lists it, and status stays Ready. Only a wait that
/// requires no findings fails, as FindingsPending with the remedy of the objects it refuses,
/// which lists them; the mirror decided both the same way.
#[tokio::test]
async fn a_wait_reports_names_the_tree_cannot_hold_and_fails_only_when_asked_to() {
    let fixture = Fixture::new(Tree::Empty).await;
    let (status, waited) = fixture.wait(b"{}").await;
    assert_eq!(status, StatusCode::OK, "{waited}");
    assert_eq!(waited["findings"]["unmapped"], 0);
    assert!(waited["remedy"].is_null());
    assert!(waited["paused_reason"].is_null());
    fixture.publish(r"dir\file", b"backslash").await;
    let (status, waited) = fixture.wait(br#"{"timeout_ms":30000}"#).await;
    assert_eq!(status, StatusCode::OK, "{waited}");
    assert_eq!(waited["reason"], "Ready");
    assert_eq!(waited["completed"], true);
    assert_eq!(waited["findings"]["unmapped"], 1);
    assert!(waited["remedy"].is_null());
    let revision = waited["findings"]["revision"].as_str().unwrap();
    let page = body(
        fixture
            .request(
                "GET",
                &format!("/_internal/sync?view=findings&revision={revision}"),
                b"",
                None,
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(page["total_count"], 1, "{page}");
    assert_eq!(page["items"][0]["reason"], "Unrepresentable");
    assert_eq!(page["items"][0]["object"], r"dir\file");
    let status = fixture.status().await;
    assert_eq!(status["state"], "Ready", "{status}");
    assert_eq!(status["observation"]["unmapped_findings"], 1);
    assert_eq!(status["observation"]["blocking_findings"], 0);
    let (code, strict) = fixture.wait(br#"{"require_no_findings":true}"#).await;
    assert_eq!(code, StatusCode::CONFLICT, "{strict}");
    assert_eq!(strict["reason"], "FindingsPending");
    assert_eq!(strict["completed"], false);
    assert_eq!(strict["findings"]["unmapped"], 1);
    // The remedy names the findings view at the revision this answer read its findings at.
    let at_hand = crate::sync::report::command::AtHand {
        findings_revision: strict["findings"]["revision"].as_str(),
        ..Default::default()
    };
    assert_eq!(
        strict["remedy"],
        crate::sync::report::verdict::Holdup::UnmappedObjects { count: 1 }
            .remedy(at_hand)
            .unwrap()
    );
    let (code, relaxed) = fixture.wait(br#"{"require_no_findings":false}"#).await;
    assert_eq!(code, StatusCode::OK, "{relaxed}");
    assert_eq!(
        status_of(
            fixture
                .request(
                    "POST",
                    "/_internal/sync/wait",
                    br#"{"require_no_findings":"yes"}"#,
                    None
                )
                .await
        ),
        StatusCode::BAD_REQUEST
    );
    fixture.finish().await;
}

/// The findings revision a wait names stays readable while nothing the findings view lists
/// changes. A late notification about the directory the wait's own export made invalidates
/// the deletion set, and the next inventory confirms it; the view a wait had just named once
/// answered 409 then, and named an approval that the findings view does not lead to. A page
/// at a revision that did change still answers 409, naming the status to fetch again.
#[tokio::test]
async fn a_findings_revision_a_wait_names_outlives_changes_the_view_does_not_list() {
    let fixture = Fixture::new(Tree::Empty).await;
    fixture.publish(r"dir\file", b"backslash").await;
    let (status, waited) = fixture.wait(br#"{"timeout_ms":30000}"#).await;
    assert_eq!(status, StatusCode::OK, "{waited}");
    let revision = waited["findings"]["revision"].as_str().unwrap().to_owned();
    let (_, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    root.invalidate_deletions().await.unwrap();
    root.complete_baseline().await.unwrap();
    let findings = |revision: String| {
        let fixture = &fixture;
        async move {
            fixture
                .request(
                    "GET",
                    &format!("/_internal/sync?view=findings&revision={revision}"),
                    b"",
                    None,
                )
                .await
        }
    };
    let page = body(findings(revision.clone()).await.unwrap()).await;
    assert_eq!(page["total_count"], 1, "{page}");
    assert_eq!(page["revision"], revision.as_str());

    root.observation_pause(Some(PauseReason::ObserverFailure))
        .await
        .unwrap();
    let stale = findings(revision).await.unwrap_err();
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert!(
        stale.message.contains("findings_revision"),
        "{}",
        stale.message
    );
    assert!(!stale.message.contains("approval"), "{}", stale.message);
    root.observation_pause(None).await.unwrap();
    drop(root);
    fixture.finish().await;
}

/// Twice the 32 control requests the server once handled before refusing the next one.
const MANY_CONTROLS: usize = 64;
const LONG_WAIT: &[u8] = br#"{"timeout_ms":60000}"#;

type ControlTask = tokio::task::JoinHandle<Result<Response<Full<Bytes>>, ApiError>>;

/// A request on its own task, so that it can be left waiting, cancelled or awaited on its own.
fn spawn_control(
    fixture: &Arc<Fixture>,
    path: &'static str,
    payload: &'static [u8],
) -> ControlTask {
    let fixture = fixture.clone();
    tokio::spawn(async move { fixture.request("POST", path, payload, None).await })
}

impl Fixture {
    /// Fills the mirror's queue, so that every command waits in line until the permit drops.
    async fn hold_the_mirror_queue(&self) -> tokio::sync::mpsc::OwnedPermit<Command> {
        let (control, _) = self.state.catalog.sync_controls().unwrap().unwrap();
        control.sender.reserve_owned().await.unwrap()
    }
}

/// Rescans and waits, the commands that need no prior state; all commands share one queue.
fn spawn_commands(fixture: &Arc<Fixture>) -> Vec<ControlTask> {
    (0..MANY_CONTROLS)
        .map(|index| match index % 8 {
            0 => spawn_control(fixture, "/_internal/sync/rescan", b"{}"),
            _ => spawn_control(fixture, "/_internal/sync/wait", LONG_WAIT),
        })
        .collect()
}

#[tokio::test]
async fn commands_past_the_mirror_queue_wait_their_turn_and_all_succeed() {
    let fixture = Arc::new(Fixture::new(Tree::Empty).await);
    let turn = fixture.hold_the_mirror_queue().await;
    let commands = spawn_commands(&fixture);
    // Authorization runs in catalog order, so this status follows every command into the queue.
    // Being answered at all also shows that the commands in line take no slot from it.
    assert_eq!(fixture.status().await["auth_mode"], "permissive");
    assert!(commands.iter().all(|command| !command.is_finished()));
    drop(turn);
    for command in commands {
        let response = joined(command).await.unwrap();
        assert!(
            [StatusCode::OK, StatusCode::ACCEPTED].contains(&response.status()),
            "{}",
            response.status()
        );
    }
    Arc::into_inner(fixture).unwrap().finish().await;
}

#[tokio::test]
async fn a_cancelled_command_gives_up_its_place_in_the_mirror_queue() {
    let fixture = Arc::new(Fixture::new(Tree::Empty).await);
    let turn = fixture.hold_the_mirror_queue().await;
    let commands = spawn_commands(&fixture);
    assert_eq!(fixture.status().await["auth_mode"], "permissive");
    for command in &commands {
        command.abort();
    }
    for command in commands {
        assert!(command.await.unwrap_err().is_cancelled());
    }
    drop(turn);
    let (control, _) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    assert_eq!(control.sender.capacity(), control.sender.max_capacity());
    let waited = fixture
        .request("POST", "/_internal/sync/wait", b"{}", None)
        .await
        .unwrap();
    assert_eq!(body(waited).await["completed"], true);
    drop(control);
    Arc::into_inner(fixture).unwrap().finish().await;
}

#[tokio::test]
async fn stopping_the_mirror_wakes_every_command_waiting_in_its_queue() {
    let fixture = Arc::new(Fixture::new(Tree::Empty).await);
    let turn = fixture.hold_the_mirror_queue().await;
    let commands = spawn_commands(&fixture);
    assert_eq!(fixture.status().await["auth_mode"], "permissive");
    fixture.task.stop();
    // Their own timeout is 60 s away, so only the stop can end the wait this soon.
    for command in commands {
        let result = tokio::time::timeout(Duration::from_secs(10), command)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status_of(result), StatusCode::SERVICE_UNAVAILABLE);
    }
    drop(turn);
    Arc::into_inner(fixture).unwrap().finish().await;
}

#[tokio::test]
async fn commands_in_line_end_at_their_own_deadlines() {
    let mut fixture = Fixture::new(Tree::Empty).await;
    fixture.config.object_request_timeout = crate::time_limit::TimeLimit::from_millis(50);
    let turn = fixture.hold_the_mirror_queue().await;
    let waited = fixture
        .request(
            "POST",
            "/_internal/sync/wait",
            br#"{"timeout_ms":50}"#,
            None,
        )
        .await
        .unwrap();
    assert_eq!(waited.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(body(waited).await["timed_out"], true);
    assert_eq!(
        status_of(
            fixture
                .request("POST", "/_internal/sync/rescan", b"{}", None)
                .await
        ),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(turn);
    fixture.finish().await;
}

/// A rescan of a running mirror that fails transiently, as when catalog admission timed out,
/// answers 503 so its client retries, and pauses nothing: the mirror keeps running, status
/// names no pause, and the retried rescan succeeds without a resume.
#[tokio::test]
async fn a_transient_failure_of_a_running_rescan_answers_503_and_pauses_nothing() {
    use crate::sync::test_support::{RescanFault, arm_rescan_fault};
    let fixture = Fixture::new(Tree::Empty).await;
    arm_rescan_fault(&fixture.tree.0, RescanFault::Busy);
    let failed = fixture
        .request("POST", "/_internal/sync/rescan", b"{}", None)
        .await
        .unwrap_err();
    assert_eq!(failed.status, StatusCode::SERVICE_UNAVAILABLE, "{failed:?}");
    assert_eq!(fixture.task.status().state(), MirrorState::Running);
    let status = fixture.status().await;
    assert_eq!(
        (&status["state"], &status["paused_reason"]),
        (&json!("Ready"), &Value::Null),
        "{status}"
    );
    let retried = fixture
        .request("POST", "/_internal/sync/rescan", b"{}", None)
        .await
        .unwrap();
    assert_eq!(retried.status(), StatusCode::ACCEPTED);
    assert_eq!(body(retried).await["completed"], true);
    fixture.finish().await;
}

#[tokio::test]
async fn status_rechecks_the_owner_union_after_admission_authorization() {
    let mut fixture = Fixture::new(Tree::Empty).await;
    std::sync::Arc::get_mut(&mut fixture.state).unwrap().auth=Auth::strict(Registry::parse(&serde_json::to_vec(&json!({"identities":[
        {"id":"seed","tokens":["seed"],"project_roles":{fixture.config.seed_project.clone():["roles/storage.admin"]}},
        {"id":"admin","tokens":["admin"],"project_roles":{fixture.config.seed_project.clone():["roles/storage.admin"],"other-project":["roles/storage.admin"]}}
    ]})).unwrap()).unwrap());
    let hook = Arc::new(AuthorizationHook {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let (response, ()) = AUTHORIZATION_HOOK
        .scope(hook.clone(), async {
            tokio::join!(
                fixture.request("GET", "/_internal/sync", b"", Some("seed")),
                async {
                    hook.entered.notified().await;
                    fixture
                        .state
                        .catalog
                        .admin(Origin::Api)
                        .ensure_bucket(
                            BucketDefinition::default_for(
                                &BucketName::new("other-bucket").unwrap(),
                                "other-project",
                                &SystemClock,
                            )
                            .unwrap(),
                        )
                        .await
                        .unwrap();
                    hook.release.notify_one();
                }
            )
        })
        .await;
    assert_eq!(status_of(response), StatusCode::FORBIDDEN);
    assert_eq!(
        fixture
            .request("GET", "/_internal/sync", b"", Some("admin"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    fixture.finish().await;
}

#[tokio::test]
async fn strict_control_authority_precedes_body_parsing_and_requires_each_explicit_project_admin() {
    let mut fixture = Fixture::new(Tree::Empty).await;
    fixture
        .state
        .catalog
        .admin(Origin::Api)
        .ensure_bucket(
            BucketDefinition::default_for(
                &BucketName::new("other-bucket").unwrap(),
                "other-project",
                &SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    std::sync::Arc::get_mut(&mut fixture.state).unwrap().auth=Auth::strict(Registry::parse(&serde_json::to_vec(&json!({"identities":[
        {"id":"seed","tokens":["seed"],"project_roles":{fixture.config.seed_project.clone():["roles/storage.admin"]}},
        {"id":"admin","tokens":["admin"],"project_roles":{fixture.config.seed_project.clone():["roles/storage.admin"],"other-project":["roles/storage.admin"]}}
    ]})).unwrap()).unwrap());
    for (token, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some("seed"), StatusCode::FORBIDDEN),
        (Some("admin"), StatusCode::BAD_REQUEST),
    ] {
        assert_eq!(
            status_of(
                fixture
                    .request("POST", "/_internal/sync/approve", b"invalid json", token)
                    .await
            ),
            expected,
            "{token:?}"
        );
    }
    assert_eq!(
        body(
            fixture
                .request("GET", "/_internal/sync", b"", Some("admin"))
                .await
                .unwrap()
        )
        .await["auth_mode"],
        "strict"
    );
    fixture.finish().await;
}

/// An unknown control route is only reported to a caller that may use the controls, so
/// authentication answers first and the route is checked after authorization.
#[tokio::test]
async fn unknown_control_routes_answer_authentication_before_not_found() {
    let mut fixture = Fixture::new(Tree::Empty).await;
    std::sync::Arc::get_mut(&mut fixture.state).unwrap().auth=Auth::strict(Registry::parse(&serde_json::to_vec(&json!({"identities":[
        {"id":"admin","tokens":["admin"],"project_roles":{fixture.config.seed_project.clone():["roles/storage.admin"]}}
    ]})).unwrap()).unwrap());
    for method in ["GET", "POST"] {
        let error = fixture
            .request(method, "/_internal/sync/unknown", b"", None)
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::UNAUTHORIZED, "{method}");
        let error = fixture
            .request(method, "/_internal/sync/unknown", b"", Some("admin"))
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::NOT_FOUND, "{method}");
        assert_eq!(error.reason, "notFound", "{method}");
        assert_eq!(
            error.message, "Unknown synchronization control route",
            "{method}"
        );
    }
    fixture.finish().await;
}

#[tokio::test]
async fn rescan_during_native_quiet_waits_for_a_completed_scan() {
    let fixture = Fixture::new(Tree::Empty).await;
    fs::create_dir(fixture.tree.0.join("test-bucket")).unwrap();
    fs::write(fixture.tree.0.join("test-bucket/body"), b"pending").unwrap();
    let (_, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    eventually_async(
        "the native event to invalidate the deletion set",
        || async { root.deletion_set().await.unwrap().is_none() },
    )
    .await;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        fixture.request_once("POST", "/_internal/sync/rescan", b"{}", None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.status(), StatusCode::ACCEPTED);
    let result = body(result).await;
    assert_eq!(result["completed"], true);
    assert!(result["reason"].is_null());
    drop(root);
    fixture.finish().await;
}

#[tokio::test]
async fn wait_during_native_quiet_reports_source_changing_only_at_its_deadline() {
    let fixture = Fixture::new(Tree::Empty).await;
    fs::create_dir(fixture.tree.0.join("test-bucket")).unwrap();
    fs::write(fixture.tree.0.join("test-bucket/body"), b"pending").unwrap();
    let (_, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    eventually_async(
        "the native event to invalidate the deletion set",
        || async { root.deletion_set().await.unwrap().is_none() },
    )
    .await;
    let started = tokio::time::Instant::now();
    let response = fixture
        .request(
            "POST",
            "/_internal/sync/wait",
            br#"{"timeout_ms":50}"#,
            None,
        )
        .await
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(50));
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(body(response).await["reason"], "SourceChanging");
    drop(root);
    fixture.finish().await;
}

#[tokio::test]
async fn delayed_observation_keeps_completed_deletion_status_without_reauthorizing_stale_approval()
{
    let fixture = Fixture::new(Tree::LongNames).await;
    fs::remove_dir_all(fixture.tree.0.join("test-bucket")).unwrap();
    fixture.task.rescan().await.unwrap();
    let before = fixture.status().await;
    assert_eq!(before["state"], "DeletionApproval");
    let revision = before["pending_deletion_revision"].as_str().unwrap();
    let (_, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();

    // Inject a withdrawn set while an observation is unsettled, as a pass that holds back
    // deletions leaves it. A native event alone no longer withdraws a set that waits for approval.
    let publication = root.begin_observation_publication().await.unwrap();
    root.invalidate_deletions().await.unwrap();
    let during = tokio::time::timeout(Duration::from_secs(1), fixture.status())
        .await
        .unwrap();
    assert_eq!(during["state"], "DeletionApproval");
    assert_eq!(
        during["pending_deletion_revision"],
        before["pending_deletion_revision"]
    );
    assert_eq!(
        during["pending_deletion_count"],
        before["pending_deletion_count"]
    );
    // The set is withdrawn, which an approval does not undo, and the status shows the latest
    // observation.
    assert!(root.deletion_set().await.unwrap().is_none());
    assert!(
        during["observation"]["revision"].as_i64().unwrap()
            >= before["observation"]["revision"].as_i64().unwrap()
    );
    assert_eq!(
        during["observation"]["tracked_objects"],
        before["observation"]["tracked_objects"]
    );
    assert_eq!(
        status_of(
            fixture
                .request(
                    "POST",
                    "/_internal/sync/approve",
                    &serde_json::to_vec(&json!({"deletion_set":revision})).unwrap(),
                    None
                )
                .await
        ),
        StatusCode::CONFLICT
    );
    drop(publication);
    fixture.task.rescan().await.unwrap();
    // The completed scan found the same members, so it confirms the set the user already reads
    // instead of replacing it; the approval refused above authorized nothing meanwhile.
    let completed = fixture.status().await;
    assert_eq!(completed["state"], "DeletionApproval");
    assert_eq!(
        completed["pending_deletion_revision"],
        before["pending_deletion_revision"]
    );
    assert_eq!(
        completed["pending_deletion_count"],
        before["pending_deletion_count"]
    );
    // A late native event of the removal no longer withdraws the confirmed set, so its revision
    // reads at once.
    let confirmed = fixture
        .request(
            "GET",
            &format!("/_internal/sync?view=deletions&revision={revision}"),
            b"",
            None,
        )
        .await
        .unwrap();
    assert_eq!(confirmed.status(), StatusCode::OK);
    assert_eq!(
        root.deletion_set().await.unwrap().unwrap().authorization,
        crate::storage::sync_state::deletions::DeletionAuthorization::Withheld
    );
    drop(root);
    fixture.finish().await;
}

#[tokio::test]
async fn long_name_frozen_pages_obey_byte_cap_and_stale_cursors_then_approve_all_members() {
    let fixture = Fixture::new(Tree::LongNames).await;
    fs::remove_dir_all(fixture.tree.0.join("test-bucket")).unwrap();
    // Pagination starts after the native observer has published the removal's completed scan.
    eventually_async("300 pending deletions", || async {
        fixture.status().await["pending_deletion_count"] == 300
    })
    .await;
    let response = fixture
        .request("POST", "/_internal/sync/rescan", b"{}", None)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(body(response).await["operation_id"].as_str().is_some());
    let status = fixture.status().await;
    assert_eq!(status["state"], "DeletionApproval");
    let old = status["pending_deletion_revision"].as_str().unwrap();
    let first = body(
        fixture
            .request(
                "GET",
                &format!("/_internal/sync?view=deletions&revision={old}"),
                b"",
                None,
            )
            .await
            .unwrap(),
    )
    .await;
    assert!(first["items"].as_array().unwrap().len() < 256);
    let cursor = first["next_cursor"].as_str().unwrap();
    fixture
        .state
        .catalog
        .patch_bucket(
            "test-bucket",
            crate::bucket::BucketMetadataUpdate {
                versioning: Some(crate::bucket::VersioningUpdate::Set(
                    crate::bucket::Versioning::Enabled,
                )),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    // The update leaves the pages of the set the status names readable, since listing deletes
    // nothing; the approval inventories the tree again, which replaces the set, and is refused.
    // The old revision's pages then name the set that waits instead.
    let old_page = format!("/_internal/sync?view=deletions&revision={old}&cursor={cursor}");
    assert_eq!(
        fixture
            .request("GET", &old_page, b"", None)
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let old_approval = serde_json::to_vec(&json!({"deletion_set":old})).unwrap();
    assert_eq!(
        status_of(
            fixture
                .request("POST", "/_internal/sync/approve", &old_approval, None)
                .await
        ),
        StatusCode::CONFLICT
    );
    let refused = fixture
        .request("GET", &old_page, b"", None)
        .await
        .unwrap_err();
    assert_eq!(refused.status, StatusCode::CONFLICT);
    let message = refused.message;
    let waiting = fixture.status().await["pending_deletion_revision"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(waiting, old);
    assert!(
        message.contains(&format!("Deletion set {waiting} waits instead")),
        "{message}"
    );
    fixture
        .request("POST", "/_internal/sync/rescan", b"{}", None)
        .await
        .unwrap();
    let status = fixture.status().await;
    let revision = status["pending_deletion_revision"].as_str().unwrap();
    let mut cursor = None;
    let mut names = std::collections::BTreeSet::new();
    loop {
        let mut path = format!("/_internal/sync?view=deletions&revision={revision}");
        if let Some(cursor) = cursor {
            path.push_str(&format!("&cursor={cursor}"));
        }
        let page = body(fixture.request("GET", &path, b"", None).await.unwrap()).await;
        assert_eq!(page["candidate_count"], 300);
        assert_eq!(page["baseline_count"], 300);
        for item in page["items"].as_array().unwrap() {
            assert!(item.get("path").is_none());
            assert!(names.insert(item["object"].as_str().unwrap().to_owned()));
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(names.len(), 300);
    let payload = serde_json::to_vec(&json!({"deletion_set":revision})).unwrap();
    assert_eq!(
        fixture
            .request("POST", "/_internal/sync/approve", &payload, None)
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        status_of(
            fixture
                .request("POST", "/_internal/sync/approve", &payload, None)
                .await
        ),
        StatusCode::CONFLICT
    );
    eventually_async("no pending deletions", || async {
        fixture.status().await["pending_deletion_count"] == 0
    })
    .await;
    assert!(fixture.tree.0.join("test-bucket").is_dir());
    fixture.finish().await;
}

/// The files of [`Tree::Pair`].
const PAIR: [&str; 2] = ["first", "second"];

/// Rejecting the set that the missing bucket directory holds for approval brings the directory
/// and its files back from the objects and deletes nothing. A revision that is stale, or that
/// was already rejected, is refused with 409 by a rejection and by an approval alike.
#[tokio::test]
async fn rejecting_a_held_set_writes_its_files_back_and_refuses_a_stale_or_used_revision() {
    let fixture = Fixture::new(Tree::Pair).await;
    fs::remove_dir_all(fixture.tree.0.join("test-bucket")).unwrap();
    fixture.task.rescan().await.unwrap();
    let status = fixture.status().await;
    assert_eq!(status["state"], "DeletionApproval");
    assert_eq!(status["pending_deletion_count"], 2);
    let revision = status["pending_deletion_revision"]
        .as_str()
        .unwrap()
        .to_owned();
    let remedy = status["pending_deletion_remedy"].as_str().unwrap();
    for command in ["approve", "reject"] {
        let advised = format!("gcs-mimic sync {command} --deletion-set {revision}");
        assert!(remedy.contains(&advised), "{remedy}");
    }
    let stale = serde_json::to_vec(&json!({"deletion_set":"0".repeat(32)})).unwrap();
    assert_eq!(
        status_of(
            fixture
                .request("POST", "/_internal/sync/reject", &stale, None)
                .await
        ),
        StatusCode::CONFLICT
    );
    let payload = serde_json::to_vec(&json!({"deletion_set":revision})).unwrap();
    let accepted = fixture
        .request("POST", "/_internal/sync/reject", &payload, None)
        .await
        .unwrap();
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let accepted = body(accepted).await;
    assert_eq!(accepted["operation_id"], revision.as_str());
    assert_eq!(accepted["decision"], "reject");
    assert_eq!(accepted["candidate_count"], 2);
    for route in ["/_internal/sync/reject", "/_internal/sync/approve"] {
        assert_eq!(
            status_of(fixture.request("POST", route, &payload, None).await),
            StatusCode::CONFLICT,
            "{route}"
        );
    }
    let waited = body(
        fixture
            .request("POST", "/_internal/sync/wait", b"{}", None)
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(waited["reason"], "Ready", "{waited}");
    for name in PAIR {
        let path = fixture.tree.0.join("test-bucket").join(name);
        assert_eq!(fs::read(&path).unwrap(), b"body", "{name}");
    }
    let status = fixture.status().await;
    assert_eq!(status["pending_deletion_count"], 0);
    assert!(status["pending_deletion_remedy"].is_null(), "{status}");
    let admin = fixture.state.catalog.admin(Origin::Api);
    let bucket = BucketName::new("test-bucket").unwrap();
    for name in PAIR {
        let name = ObjectName::new(name).unwrap();
        assert!(
            admin.get_object(&bucket, &name, None).await.is_ok(),
            "{name:?}"
        );
    }
    fixture.finish().await;
}

/// A wait answers 200 only when it ended ready, 408 when its time ran out, and 409 for every
/// other way it ended, so a caller that reads only the status tells success from failure.
#[test]
fn a_wait_answers_with_a_status_that_tells_how_it_ended() {
    use crate::sync::report::verdict::Holdup;
    let held = |holdup| WaitResult::held(holdup, None, None);
    let cut = |why| WaitResult::unfinished(why, None, None);
    for (result, reason, status) in [
        (held(Holdup::Ready), WaitReason::Ready, StatusCode::OK),
        (
            WaitResult::timeout(),
            WaitReason::Timeout,
            StatusCode::REQUEST_TIMEOUT,
        ),
        (
            cut(Unfinished::Stopped),
            WaitReason::Stopped,
            StatusCode::CONFLICT,
        ),
        (
            held(Holdup::Paused(PauseReason::IoFailure)),
            WaitReason::Paused,
            StatusCode::CONFLICT,
        ),
        (
            cut(Unfinished::JournalChanged),
            WaitReason::JournalChanged,
            StatusCode::CONFLICT,
        ),
        (
            cut(Unfinished::SourceChanging),
            WaitReason::SourceChanging,
            StatusCode::CONFLICT,
        ),
        (
            held(Holdup::SourceUnrepresentable),
            WaitReason::SourceUnrepresentable,
            StatusCode::CONFLICT,
        ),
        (
            held(Holdup::DeletionApproval),
            WaitReason::DeletionApproval,
            StatusCode::CONFLICT,
        ),
        (
            held(Holdup::FindingsPending),
            WaitReason::FindingsPending,
            StatusCode::CONFLICT,
        ),
    ] {
        assert_eq!(result.reason, reason);
        assert_eq!(result.reason.http_status(), status, "{reason}");
    }
}
