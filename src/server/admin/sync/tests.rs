use super::*;
use crate::{
    auth::{Auth, Registry},
    clock::SystemClock,
    config::Config,
    server::{ObjectActivity, State},
    storage::{
        Catalog,
        admin::{BucketDefinition, BucketName, Origin},
    },
    test_support::TestDir,
};
use http_body_util::BodyExt;
use std::{fs, sync::atomic::AtomicBool, time::Duration};

struct AuthorizationHook {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
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
impl Fixture {
    async fn new(long_names: bool) -> Self {
        let data = TestDir::new();
        let tree = TestDir::new();
        if long_names {
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
            ready: AtomicBool::new(true),
            clock: Arc::new(SystemClock),
            catalog,
            api_base_url: "http://localhost".into(),
            default_location: "US".into(),
            auth: Auth::permissive(),
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
        body: &[u8],
        token: Option<&str>,
    ) -> Result<Response<Full<Bytes>>, JsonError> {
        let endpoints = self
            .config
            .runtime_endpoints
            .snapshot(&self.config, &self.state.api_base_url)
            .unwrap();
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
    async fn finish(self) {
        self.task.finish().await.unwrap();
        self.state.catalog.close().await.unwrap();
    }
}

async fn body(response: Response<Full<Bytes>>) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(bytes.len() <= RESPONSE_LIMIT);
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn unmappable_source_entries_have_a_durable_root_finding_until_repaired() {
    let fixture = Fixture::new(false).await;
    fs::create_dir(fixture.tree.0.join("INVALID-BUCKET")).unwrap();
    fs::write(
        fixture.tree.0.join("INVALID-BUCKET/body"),
        b"source survives",
    )
    .unwrap();
    fixture.task.rescan().await.unwrap();
    let status = fixture.status().await;
    assert_eq!(status["state"], "SourceUnrepresentable");
    let revision = status["findings_revision"].as_str().unwrap();
    let path = format!("/_internal/sync?view=findings&revision={revision}");
    let page = body(fixture.request("GET", &path, b"", None).await.unwrap()).await;
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
        fixture
            .request("GET", &path, b"", None)
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    assert!(fixture.status().await["observation"]["scan_stop"].is_null());
    fixture.finish().await;
}

#[tokio::test]
async fn live_rescan_registers_a_protected_workspace_before_resuming_intake() {
    use crate::storage::{
        admin::{Generation, ObjectName},
        sync_state::writeback::{Kind, Limits},
    };
    let fixture = Fixture::new(false).await;
    fs::create_dir(fixture.tree.0.join("test-bucket")).unwrap();
    fs::write(fixture.tree.0.join("test-bucket/body"), b"API winner").unwrap();
    fixture.task.rescan().await.unwrap();
    let (_, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    let name = ObjectName::new("body").unwrap();
    let winner = fixture
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
    let fence = fixture
        .state
        .catalog
        .read_changes(None, None, 1)
        .await
        .unwrap()
        .fence;
    let pending = writeback
        .prepare(
            &bucket,
            &name,
            Generation::new(winner.generation).unwrap(),
            Kind::Publish,
            fence,
        )
        .await
        .unwrap();
    let pending = writeback.arm(pending.token).await.unwrap();
    let path = fixture
        .tree
        .0
        .join(crate::sync::mapping::WORKSPACE)
        .join(pending.workspace_entry.name());
    fs::write(&path, b"retained external source").unwrap();
    writeback.inspect(pending.token).await.unwrap();
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
    use crate::storage::{
        admin::{Generation, ObjectName},
        sync_state::writeback::{Kind, Limits},
    };
    let fixture = Fixture::new(false).await;
    fs::create_dir(fixture.tree.0.join("test-bucket")).unwrap();
    fs::write(fixture.tree.0.join("test-bucket/body"), b"API winner").unwrap();
    fixture.task.rescan().await.unwrap();
    let (_, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    let name = ObjectName::new("body").unwrap();
    let winner = fixture
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
    let fence = fixture
        .state
        .catalog
        .read_changes(None, None, 1)
        .await
        .unwrap()
        .fence;
    let pending = writeback
        .prepare(
            &bucket,
            &name,
            Generation::new(winner.generation).unwrap(),
            Kind::Publish,
            fence,
        )
        .await
        .unwrap();
    let pending = writeback.arm(pending.token).await.unwrap();
    let id = pending.workspace_entry.name().to_owned();
    let path = fixture
        .tree
        .0
        .join(crate::sync::mapping::WORKSPACE)
        .join(&id);
    fs::write(&path, b"protected old body").unwrap();
    writeback.inspect(pending.token).await.unwrap();
    let old = fixture.status().await;
    let old_revision = old["findings_revision"].as_str().unwrap();
    let old_path = format!("/_internal/sync?view=findings&revision={old_revision}");
    let old_page = body(fixture.request("GET", &old_path, b"", None).await.unwrap()).await;
    let original = old_page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["recovery_id"] == id)
        .unwrap();
    assert_eq!(original["retained_bytes"], 18);
    assert!(original["hashes"].is_null());
    fs::write(&path, b"changed protected body").unwrap();
    let fresh = fixture.status().await;
    assert_ne!(fresh["findings_revision"], old["findings_revision"]);
    assert_eq!(
        fixture
            .request("GET", &old_path, b"", None)
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    let page_path = format!(
        "/_internal/sync?view=findings&revision={}",
        fresh["findings_revision"].as_str().unwrap()
    );
    let page = body(fixture.request("GET", &page_path, b"", None).await.unwrap()).await;
    let selected = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["recovery_id"] == id)
        .unwrap();
    assert_eq!(selected["retained_bytes"], 22);
    assert_ne!(selected["target_identity"], original["target_identity"]);
    let mut payload = json!({"recovery_id":id,"revision":selected["recovery_revision"],"target_identity":selected["target_identity"],"action":"discard","operation_id":"http-confirmed-recovery"});
    assert_eq!(
        fixture
            .request(
                "POST",
                "/_internal/sync/resolve",
                &serde_json::to_vec(&payload).unwrap(),
                None
            )
            .await
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    payload["confirmation"] = json!("discard_without_backup");
    for mixed in ["conflict_id", "expected_generation"] {
        payload[mixed] = Value::Null;
        assert_eq!(
            fixture
                .request(
                    "POST",
                    "/_internal/sync/resolve",
                    &serde_json::to_vec(&payload).unwrap(),
                    None
                )
                .await
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
        );
        payload.as_object_mut().unwrap().remove(mixed);
    }
    payload["target_identity"] = original["target_identity"].clone();
    assert_eq!(
        fixture
            .request(
                "POST",
                "/_internal/sync/resolve",
                &serde_json::to_vec(&payload).unwrap(),
                None
            )
            .await
            .unwrap_err()
            .status,
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
    let mut fixture = Fixture::new(false).await;
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
    assert_eq!(
        fixture
            .request("GET", "/_internal/sync/conflicts", b"", None)
            .await
            .unwrap_err()
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fixture
            .request("GET", "/_internal/sync/conflicts", b"", Some("reader"))
            .await
            .unwrap_err()
            .status,
        StatusCode::FORBIDDEN
    );
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
            fixture
                .request(
                    "POST",
                    "/_internal/sync/resolve",
                    &serde_json::to_vec(&payload).unwrap(),
                    Some("admin")
                )
                .await
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
        );
        payload.as_object_mut().unwrap().remove(mixed);
    }
    payload["expected_generation"] = json!(winner.generation + 1);
    assert_eq!(
        fixture
            .request(
                "POST",
                "/_internal/sync/resolve",
                &serde_json::to_vec(&payload).unwrap(),
                Some("admin")
            )
            .await
            .unwrap_err()
            .status,
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
        fixture
            .request(
                "POST",
                "/_internal/sync/resolve",
                &serde_json::to_vec(&payload).unwrap(),
                Some("admin")
            )
            .await
            .unwrap_err()
            .status,
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
async fn findings_pages_reject_in_place_changes_and_foreign_sessions() {
    let fixture = Fixture::new(false).await;
    let (registration, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    root.observation_pause(Some("operator-review".into()))
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
    assert_eq!(page["items"][0]["detail_code"], "operator-review");
    assert_eq!(page["items"][0]["scope"], "root");
    assert!(page["items"][0]["finding_id"].is_string());
    assert!(page["items"][0]["remedy"].is_string());
    assert!(page["next_cursor"].is_null());
    root.observation_pause(Some("changed-in-place".into()))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .request("GET", &path, b"", None)
            .await
            .unwrap_err()
            .status,
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
        fixture
            .request(
                "GET",
                &format!("/_internal/sync?view=findings&revision={foreign}"),
                b"",
                None
            )
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    root.observation_pause(None).await.unwrap();
    drop((registration, root));
    fixture.finish().await;
}

#[tokio::test]
async fn protected_findings_pages_cover_every_target_with_a_final_null_cursor() {
    let fixture = Fixture::new(false).await;
    let (_, root) = fixture.state.catalog.sync_controls().unwrap().unwrap();
    let owner = fixture.config.seed_project.clone();
    let bucket = BucketName::new("test-bucket").unwrap();
    fixture
        .state
        .catalog
        .admin(Origin::Api)
        .ensure_bucket(BucketDefinition::default_for(&bucket, &owner, &SystemClock).unwrap())
        .await
        .unwrap();
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
        fixture
            .request(
                "GET",
                &format!("/_internal/sync?view=findings&revision={revision}"),
                b"",
                None
            )
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    drop(root);
    fixture.finish().await;
}

#[tokio::test]
async fn controls_reject_invalid_input_unsupported_routes_and_excess_admission() {
    let fixture = Fixture::new(false).await;
    assert_eq!(fixture.status().await["auth_mode"], "permissive");
    for query in [
        "?limit=1",
        "?view=deletions",
        "?view=unknown&revision=0123456789abcdef0123456789abcdef",
        "?view=deletions&revision=0123456789abcdef0123456789abcdef&limit=0",
        "?view=deletions&revision=0123456789abcdef0123456789abcdef&limit=1&limit=2",
    ] {
        assert_eq!(
            fixture
                .request("GET", &format!("/_internal/sync{query}"), b"", None)
                .await
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        fixture
            .request("GET", "/_internal/sync", b"x", None)
            .await
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        fixture
            .request("PUT", "/_internal/sync", b"", None)
            .await
            .unwrap_err()
            .status,
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        fixture
            .request(
                "POST",
                "/_internal/sync/rescan",
                br#"{"override":true}"#,
                None
            )
            .await
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        fixture
            .request("POST", "/_internal/sync/approve", &vec![b' '; 65_537], None)
            .await
            .unwrap_err()
            .status,
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        fixture
            .request("GET", "/_internal/sync/conflicts", b"x", None)
            .await
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        fixture
            .request("POST", "/_internal/sync/resolve", b"{}", None)
            .await
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
    for body in [
        br#"{"timeout_ms":0}"#.as_slice(),
        br#"{"timeout_ms":60001}"#,
        br#"{"unknown":1}"#,
    ] {
        assert_eq!(
            fixture
                .request("POST", "/_internal/sync/wait", body, None)
                .await
                .unwrap_err()
                .status,
            StatusCode::BAD_REQUEST
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
        fixture
            .request("GET", "/_internal/sync", b"", None)
            .await
            .unwrap_err()
            .status,
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
    let mut fixture = Fixture::new(false).await;
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
    assert_eq!(response.unwrap_err().status, StatusCode::FORBIDDEN);
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
    let mut fixture = Fixture::new(false).await;
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
    assert_eq!(
        fixture
            .request("POST", "/_internal/sync/approve", b"invalid json", None)
            .await
            .unwrap_err()
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        fixture
            .request(
                "POST",
                "/_internal/sync/approve",
                b"invalid json",
                Some("seed")
            )
            .await
            .unwrap_err()
            .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fixture
            .request(
                "POST",
                "/_internal/sync/approve",
                b"invalid json",
                Some("admin")
            )
            .await
            .unwrap_err()
            .status,
        StatusCode::BAD_REQUEST
    );
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
async fn long_name_frozen_pages_obey_byte_cap_and_stale_cursors_then_approve_all_members() {
    let fixture = Fixture::new(true).await;
    fs::remove_dir_all(fixture.tree.0.join("test-bucket")).unwrap();
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
            crate::bucket::BucketMetadataUpdate::default(),
            None,
            None,
            &crate::bucket::BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(
        fixture
            .request(
                "GET",
                &format!("/_internal/sync?view=deletions&revision={old}&cursor={cursor}"),
                b"",
                None
            )
            .await
            .unwrap_err()
            .status,
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
        fixture
            .request("POST", "/_internal/sync/approve", &payload, None)
            .await
            .unwrap_err()
            .status,
        StatusCode::CONFLICT
    );
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if fixture.status().await["pending_deletion_count"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(fixture.tree.0.join("test-bucket").is_dir());
    fixture.finish().await;
}
