use super::*;
use crate::storage::test_fixtures::PERMISSIVE;
use crate::{
    auth::{Auth, Registry},
    clock::SystemClock,
    config::Config,
    server::{ObjectActivity, State},
    storage::{
        Catalog,
        admin::{BucketDefinition, BucketName, Generation, ObjectName, Origin},
        sync_state::writeback::{Limits, Writeback},
    },
    test_support::{TestDir, eventually_async, eventually_some_async},
};
use http_body_util::BodyExt;
use std::{fs, time::Duration};

#[test]
fn nested_io_control_errors_keep_status_and_text() {
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
        let actual = control_error(&io::Error::other(io::Error::other(error)));
        assert_eq!(actual.status, expected.status, "{label}");
        assert_eq!(actual.message, expected.message, "{label}");
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
                control_error(&io::Error::other(io::Error::other(AdminError::from(
                    io::Error::from(kind),
                )))),
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
    let actual = control_error(&io::Error::other(io::Error::other(
        crate::storage::admin::AdminError::Corrupt("corrupt".into()),
    )));
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
        let actual = control_error(&io::Error::other(stop));
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
        fixture.config.catalog_wait_timeout_ms = match end {
            End::Deadline => 20,
            End::Cancel => 30_000,
        };
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
        assert!(!observer.borrow().awaiting_catalog);
        assert!(observer.borrow().deadline.is_none());
        fixture.finish().await;
    }
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
                        let (claimed, bounded) = {
                            let progress = observer.borrow();
                            (progress.awaiting_catalog, progress.deadline.is_some())
                        };
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
        observations.push((claimed, bounded, observer.borrow().awaiting_catalog));
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
        control_error(&io::Error::other(
            crate::storage::sync_state::writeback::Error::Root(RootError::Admin(
                crate::storage::admin::AdminError::Forbidden
            ))
        ))
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        control_error(&io::Error::other(
            crate::storage::admin::AdminError::Forbidden
        ))
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        control_error(&io::Error::other(
            crate::storage::admin::AdminError::Precondition
        ))
        .status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        control_error(&io::Error::other(io::Error::other(
            crate::storage::admin::AdminError::Forbidden
        )))
        .status,
        StatusCode::FORBIDDEN
    );
}

struct Fixture {
    _data: TestDir,
    tree: TestDir,
    state: State,
    config: Config,
    task: crate::sync::Task,
}

enum Tree {
    Empty,
    EmptyBucket,
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
        if matches!(shape, Tree::EmptyBucket) {
            fs::create_dir(tree.0.join("test-bucket")).unwrap();
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
        let state = State {
            api_base_url: "http://localhost".into(),
            ..State::for_test(catalog, Auth::permissive())
        };
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
    ) -> Result<Response<Full<Bytes>>, JsonError> {
        self.request_once(method, path, payload, token).await
    }

    async fn request_once(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        token: Option<&str>,
    ) -> Result<Response<Full<Bytes>>, JsonError> {
        let endpoints = self
            .config
            .runtime_endpoints
            .snapshot(&self.config, &self.state.api_base_url)
            .unwrap()
            .without_request();
        let (activity, _) = tokio::sync::watch::channel(ObjectActivity::default());
        let mut request = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            request = request.header("authorization", format!("Bearer {token}"));
        }
        let request = request
            .body(RequestBody::Buffered(Full::new(Bytes::copy_from_slice(
                body,
            ))))
            .unwrap();
        control(
            request,
            RequestCtx {
                state: &self.state,
                config: &self.config,
                endpoints: &endpoints,
                activity: &activity,
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
        let fence = self
            .state
            .catalog
            .read_changes(None, None, 1)
            .await
            .unwrap()
            .fence;
        let pending = crate::storage::sync_state::writeback::armed_observation(
            &writeback,
            &bucket,
            &name,
            Generation::new(winner.generation).unwrap(),
            fence,
        )
        .await;
        let id = pending.token.id().to_owned();
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
}

#[track_caller]
fn status_of(result: Result<Response<Full<Bytes>>, JsonError>) -> StatusCode {
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
            None,
            0,
            256,
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
    use crate::{
        object::UploadChunk,
        storage::admin::{Generation, ObjectName, Publication},
    };
    let mut fixture = Fixture::new(Tree::Empty).await;
    let bucket = BucketName::new("test-bucket").unwrap();
    let name = ObjectName::new("object").unwrap();
    let admin = fixture.state.catalog.admin(Origin::Api);
    admin
        .ensure_bucket(
            BucketDefinition::default_for(&bucket, &fixture.config.seed_project, &SystemClock)
                .unwrap(),
        )
        .await
        .unwrap();
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    sender
        .try_send(Ok(UploadChunk::Data(Bytes::from_static(b"API winner"))))
        .unwrap();
    sender.try_send(Ok(UploadChunk::End)).unwrap();
    let winner = admin
        .publish_object(
            &bucket,
            &name,
            Publication {
                content_type: "application/octet-stream".into(),
                metadata: Default::default(),
                expected: Default::default(),
                condition: None,
                max_bytes: 4096,
                mode: crate::storage::admin::PublishMode::Replace,
            },
            receiver,
        )
        .await
        .unwrap();
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
    fixture.state.auth=Auth::strict(Registry::parse(&serde_json::to_vec(&json!({"identities":[
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
    assert_eq!(conflict["conflict_id"], captured.id());
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
    let file = reader.into_file().await.unwrap();
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
async fn controls_reject_invalid_input_unsupported_routes_and_excess_admission() {
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
    let (control, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    let permits = control
        .admission
        .clone()
        .acquire_many_owned(32)
        .await
        .unwrap();
    assert_eq!(
        status_of(fixture.request("GET", "/_internal/sync", b"", None).await),
        StatusCode::SERVICE_UNAVAILABLE
    );
    drop(permits);
    drop(root);
    fixture
        .state
        .ready
        .store(false, std::sync::atomic::Ordering::Release);
    assert_eq!(fixture.status().await["state"], "Unhealthy");
    fixture.finish().await;
}

#[tokio::test]
async fn status_rechecks_the_owner_union_after_admission_authorization() {
    let mut fixture = Fixture::new(Tree::Empty).await;
    fixture.state.auth=Auth::strict(Registry::parse(&serde_json::to_vec(&json!({"identities":[
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
    fixture.state.auth=Auth::strict(Registry::parse(&serde_json::to_vec(&json!({"identities":[
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

    // Inject the exact interval between a delayed native event's invalidation and its completed scan.
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
    assert!(
        during["observation"]["revision"].as_i64().unwrap()
            > before["observation"]["revision"].as_i64().unwrap()
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
    let completed = fixture.status().await;
    assert_eq!(completed["state"], "DeletionApproval");
    assert_ne!(
        completed["pending_deletion_revision"],
        before["pending_deletion_revision"]
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
                versioning_enabled: Some(true),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    assert_eq!(
        status_of(
            fixture
                .request(
                    "GET",
                    &format!("/_internal/sync?view=deletions&revision={old}&cursor={cursor}"),
                    b"",
                    None
                )
                .await
        ),
        StatusCode::CONFLICT
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
