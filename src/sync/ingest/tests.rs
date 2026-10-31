#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use crate::{
    object::{ObjectMetadataUpdate, UploadChunk},
    storage::admin::Origin,
    test_support::TestDir,
};
use std::{collections::BTreeMap, fs, sync::Arc, time::Duration};

async fn fixture() -> (TestDir, TestDir, Catalog, RootLease, Config) {
    let data = TestDir::new();
    let tree = TestDir::new();
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
    (data, tree, catalog, root, config)
}

async fn scan(catalog: &Catalog, root: &RootLease, config: &Config) -> Reconciliation {
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    rescan(catalog, root, config, &stopping).await.unwrap()
}

#[tokio::test]
async fn complete_scan_clears_a_changing_source_finding_after_that_source_disappears() {
    let (_data, tree, catalog, root, config) = fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    scan(&catalog, &root, &config).await;
    let binding = root
        .sync_binding(&BucketName::new("test-bucket").unwrap())
        .await
        .unwrap();
    root.record_path_stop(
        binding,
        ObjectName::new("disappeared").unwrap(),
        PathStop::SourceChanging,
    )
    .await
    .unwrap();
    assert_eq!(root.observation().await.unwrap().path_stops, 1);
    assert_eq!(scan(&catalog, &root, &config).await.deferred, 0);
    assert_eq!(root.observation().await.unwrap().path_stops, 0);
    assert!(root.observation().await.unwrap().scan_stop.is_none());
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn recovery_rechecks_a_project_added_after_control_authorization() {
    use crate::storage::sync_state::writeback::{Kind, Limits};
    let (_data, tree, catalog, root, config) = fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/body"), b"API winner").unwrap();
    scan(&catalog, &root, &config).await;
    catalog.enable_change_journal().await.unwrap();
    let scoped = root.with_authority(control_authority(&[&config.seed_project]));
    let writeback = scoped
        .writeback(
            Limits::new(
                super::super::CONFLICT_MAX_BYTES,
                super::super::CONFLICT_MAX_ENTRIES,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let bucket = BucketName::new("test-bucket").unwrap();
    let name = ObjectName::new("body").unwrap();
    let winner = object(&catalog, "test-bucket/body").await;
    let fence = catalog.read_changes(None, None, 1).await.unwrap().fence;
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
    let path = tree
        .0
        .join(mapping::WORKSPACE)
        .join(pending.workspace_entry.name());
    fs::write(&path, b"retained original").unwrap();
    writeback.inspect(pending.token).await.unwrap();
    let authority = control_authority(&[&config.seed_project]);
    scoped.observation_for(authority).await.unwrap();
    catalog
        .admin(Origin::Api)
        .ensure_bucket(
            crate::storage::admin::BucketDefinition::default_for(
                &BucketName::new("other-bucket").unwrap(),
                "other-project",
                &crate::clock::SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let refused = writeback.preserve_protected().await;
    assert!(
        matches!(
            &refused,
            Err(crate::storage::sync_state::writeback::Error::Root(
                RootError::Admin(AdminError::Forbidden)
            ))
        ),
        "{refused:?}"
    );
    assert_eq!(fs::read(&path).unwrap(), b"retained original");
    assert_eq!(object(&catalog, "test-bucket/body").await, winner);
    drop((writeback, scoped));
    let scoped = root.with_authority(control_authority(&[&config.seed_project, "other-project"]));
    let writeback = scoped
        .writeback(
            Limits::new(
                super::super::CONFLICT_MAX_BYTES,
                super::super::CONFLICT_MAX_ENTRIES,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    writeback.preserve_protected().await.unwrap();
    assert!(!path.exists());
    assert_eq!(object(&catalog, "test-bucket/body").await, winner);
    drop((writeback, scoped, root));
    catalog.close().await.unwrap();
}

fn control_authority(projects: &[&str]) -> crate::storage::sync_state::authority::ControlAuthority {
    let roles: std::collections::BTreeMap<_, _> = projects
        .iter()
        .map(|project| (*project, vec!["roles/storage.admin"]))
        .collect();
    let auth=crate::auth::Auth::strict(crate::auth::Registry::parse(&serde_json::to_vec(&serde_json::json!({"identities":[{"id":"operator","tokens":["control-token"],"project_roles":roles}]})).unwrap()).unwrap());
    let mut headers = hyper::HeaderMap::new();
    headers.insert(
        "authorization",
        hyper::header::HeaderValue::from_static("Bearer control-token"),
    );
    let identity = auth
        .authenticate_at(&headers, None, &crate::clock::SystemClock)
        .unwrap()
        .unwrap()
        .principal()
        .unwrap()
        .clone();
    crate::storage::sync_state::authority::ControlAuthority::new(projects[0].into(), Some(identity))
}

#[tokio::test]
async fn publication_rechecks_a_project_added_after_source_staging() {
    let (_data, tree, catalog, root, config) = fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/body"), b"preserved external body").unwrap();
    let scoped = root.with_authority(control_authority(&[&config.seed_project]));
    let hook = Arc::new(Hook {
        phase: "publish",
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let (_sender, stopping) = tokio::sync::watch::channel(false);
    let (result, ()) = HOOK
        .scope(hook.clone(), async {
            tokio::join!(rescan(&catalog, &scoped, &config, &stopping), async {
                hook.entered.notified().await;
                catalog
                    .admin(Origin::Api)
                    .ensure_bucket(
                        crate::storage::admin::BucketDefinition::default_for(
                            &BucketName::new("other-bucket").unwrap(),
                            "other-project",
                            &crate::clock::SystemClock,
                        )
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                hook.release.notify_one();
            })
        })
        .await;
    assert!(result.is_err());
    assert!(matches!(
        catalog
            .admin(Origin::Api)
            .get_object(
                &BucketName::new("test-bucket").unwrap(),
                &ObjectName::new("body").unwrap(),
                None
            )
            .await,
        Err(AdminError::NotFound)
    ));
    assert_eq!(
        fs::read(tree.0.join("test-bucket/body")).unwrap(),
        b"preserved external body"
    );
    drop(scoped);
    let scoped = root.with_authority(control_authority(&[&config.seed_project, "other-project"]));
    assert_eq!(scan(&catalog, &scoped, &config).await.ingested, 1);
    assert_eq!(object(&catalog, "test-bucket/body").await.size, 23);
    drop((scoped, root));
    catalog.close().await.unwrap();
}

async fn object(catalog: &Catalog, path: &str) -> crate::object::ObjectRecord {
    let (bucket, name) = mapping::path_to_name(std::path::Path::new(path)).unwrap();
    catalog
        .admin(Origin::Api)
        .get_object(&bucket, &name, None)
        .await
        .unwrap()
}

async fn api_put(catalog: &Catalog, path: &str, bytes: &'static [u8]) {
    let (bucket, name) = mapping::path_to_name(std::path::Path::new(path)).unwrap();
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    sender
        .send(Ok(UploadChunk::Data(bytes::Bytes::from_static(bytes))))
        .await
        .unwrap();
    sender.send(Ok(UploadChunk::End)).await.unwrap();
    catalog
        .admin(Origin::Api)
        .publish_object(
            &bucket,
            &name,
            Publication {
                content_type: "application/api".into(),
                metadata: Default::default(),
                expected: Default::default(),
                condition: None,
                max_bytes: 4096,
                mode: PublishMode::Replace,
            },
            receiver,
        )
        .await
        .unwrap();
}

async fn wait_observed(catalog: &Catalog, path: &str, bytes: &[u8]) -> crate::object::ObjectRecord {
    let (bucket, name) = mapping::path_to_name(std::path::Path::new(path)).unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(record) = catalog
                .admin(Origin::Api)
                .get_object(&bucket, &name, None)
                .await
                && record.size == bytes.len() as u64
                && record.crc32c == crc32c::crc32c(bytes)
            {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn wait_absent(catalog: &Catalog, path: &str) {
    let (bucket, name) = mapping::path_to_name(std::path::Path::new(path)).unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match catalog
                .admin(Origin::Api)
                .get_object(&bucket, &name, None)
                .await
            {
                Err(AdminError::NotFound) => return,
                Ok(_) => tokio::time::sleep(Duration::from_millis(10)).await,
                Err(error) => panic!("unexpected API error: {error}"),
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn scan_entry_limit_fails_before_an_inventory_can_be_declared_complete() {
    let (_data, tree, catalog, root, _config) = fixture().await;
    fs::write(tree.0.join("a"), b"a").unwrap();
    fs::write(tree.0.join("b"), b"b").unwrap();
    let result = root
        .file_job(|root| {
            Scan {
                stack: vec![(PathBuf::new(), None)],
                visited: 9999,
            }
            .page(root)
        })
        .await;
    assert!(
        matches!(result,Err(RootError::Admin(AdminError::Invalid(reason))) if reason.contains("10000"))
    );
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn native_watcher_ingests_external_create_edit_replace_and_rename_without_manual_rescan() {
    let (_data, tree, catalog, root, config) = fixture().await;
    let task = crate::sync::start(catalog.clone(), root.clone(), &config).unwrap();
    assert!(task.rescan().await.unwrap().baseline_complete);
    fs::create_dir_all(tree.0.join("watched-bucket/nested")).unwrap();
    let path = tree.0.join("watched-bucket/nested/body");
    fs::write(&path, b"new").unwrap();
    let created = wait_observed(&catalog, "watched-bucket/nested/body", b"new").await;
    fs::write(&path, b"edited in place").unwrap();
    let edited = wait_observed(&catalog, "watched-bucket/nested/body", b"edited in place").await;
    assert!(edited.generation > created.generation);
    let replacement = tree.0.join("watched-bucket/nested/replacement");
    fs::write(&replacement, b"atomic replacement").unwrap();
    fs::rename(&replacement, &path).unwrap();
    let replaced = wait_observed(
        &catalog,
        "watched-bucket/nested/body",
        b"atomic replacement",
    )
    .await;
    assert!(replaced.generation > edited.generation);
    let renamed = tree.0.join("watched-bucket/nested/renamed");
    fs::rename(&path, &renamed).unwrap();
    wait_observed(
        &catalog,
        "watched-bucket/nested/renamed",
        b"atomic replacement",
    )
    .await;
    wait_absent(&catalog, "watched-bucket/nested/body").await;
    fs::remove_file(&renamed).unwrap();
    task.rescan().await.unwrap();
    wait_absent(&catalog, "watched-bucket/nested/renamed").await;
    tokio::time::timeout(Duration::from_secs(5), task.finish())
        .await
        .unwrap()
        .unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ingest_initial_baseline_runtime_rescan_and_restart_preserve_generations() {
    let (data, tree, catalog, root, config) = fixture().await;
    fs::create_dir(tree.0.join("first-bucket")).unwrap();
    fs::create_dir(tree.0.join("second-bucket")).unwrap();
    fs::write(tree.0.join("first-bucket/zero"), b"").unwrap();
    fs::write(tree.0.join("second-bucket/body"), [0, 255, 13, 10]).unwrap();
    let task = crate::sync::start(catalog.clone(), root.clone(), &config).unwrap();
    let unchanged = task.rescan().await.unwrap();
    assert_eq!(unchanged.ingested, 0);
    let original = object(&catalog, "second-bucket/body").await;
    let unchanged = task.rescan().await.unwrap();
    assert_eq!((unchanged.ingested, unchanged.identical), (0, 0));
    assert_eq!(original.size, 4);
    assert_eq!(object(&catalog, "first-bucket/zero").await.size, 0);
    assert_eq!(
        catalog
            .admin(Origin::Api)
            .get_bucket(&BucketName::new("first-bucket").unwrap())
            .await
            .unwrap()
            .unwrap()
            .project,
        config.seed_project
    );
    fs::rename(
        tree.0.join("second-bucket/body"),
        tree.0.join("second-bucket/renamed"),
    )
    .unwrap();
    task.rescan().await.unwrap();
    wait_absent(&catalog, "second-bucket/body").await;
    assert_eq!(
        object(&catalog, "second-bucket/renamed").await.md5,
        original.md5
    );
    task.finish().await.unwrap();
    drop(root);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = crate::sync::preflight(&catalog, &config)
        .await
        .unwrap()
        .unwrap();
    assert!(root.status().permits_external_deletions());
    assert_eq!(scan(&catalog, &root, &config).await.ingested, 0);
    wait_absent(&catalog, "second-bucket/body").await;
    assert_eq!(
        object(&catalog, "second-bucket/renamed").await.md5,
        original.md5
    );
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ingest_external_only_conflicts_equal_hash_and_unsafe_entries() {
    let (_data, tree, catalog, root, config) = fixture().await;
    catalog
        .admin(Origin::Api)
        .ensure_bucket(
            crate::storage::admin::BucketDefinition::default_for(
                &BucketName::new("test-bucket").unwrap(),
                "existing-owner",
                &crate::clock::SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    api_put(&catalog, "test-bucket/shared", b"api").await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/shared"), b"external").unwrap();
    fs::write(tree.0.join("test-bucket/only"), b"new").unwrap();
    for name in ["4913", "file.swp", "file~", ".DS_Store", "._file"] {
        fs::write(tree.0.join("test-bucket").join(name), b"ignore").unwrap();
    }
    std::os::unix::fs::symlink("only", tree.0.join("test-bucket/link")).unwrap();
    let summary = scan(&catalog, &root, &config).await;
    assert_eq!(
        (
            summary.ingested,
            summary.conflicting,
            summary.ignored,
            summary.unrepresentable
        ),
        (1, 1, 5, 1)
    );
    assert_eq!(
        fs::read(tree.0.join("test-bucket/shared")).unwrap(),
        b"external"
    );
    assert_eq!(root.findings_page(0, 256).await.unwrap().len(), 1);
    assert_eq!(
        catalog
            .admin(Origin::Api)
            .get_bucket(&BucketName::new("test-bucket").unwrap())
            .await
            .unwrap()
            .unwrap()
            .project,
        "existing-owner"
    );
    let generation = object(&catalog, "test-bucket/only").await.generation;
    fs::write(tree.0.join("test-bucket/only"), b"new").unwrap();
    assert_eq!(scan(&catalog, &root, &config).await.ingested, 0);
    assert_eq!(
        object(&catalog, "test-bucket/only").await.generation,
        generation
    );
    fs::remove_file(tree.0.join("test-bucket/only")).unwrap();
    scan(&catalog, &root, &config).await;
    assert_eq!(
        object(&catalog, "test-bucket/only").await.generation,
        generation
    );
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ingest_preserves_metadata_patch_and_api_body_racing_publication() {
    for body_race in [false, true] {
        let (_data, tree, catalog, root, config) = fixture().await;
        fs::create_dir(tree.0.join("test-bucket")).unwrap();
        fs::write(tree.0.join("test-bucket/object"), b"old").unwrap();
        scan(&catalog, &root, &config).await;
        fs::write(tree.0.join("test-bucket/object"), b"external-edit").unwrap();
        let hook = Arc::new(Hook {
            phase: "publish",
            entered: Default::default(),
            release: Default::default(),
        });
        let worker_catalog = catalog.clone();
        let worker_root = root.clone();
        let worker_config = config.clone();
        let worker = tokio::spawn(HOOK.scope(hook.clone(), async move {
            scan(&worker_catalog, &worker_root, &worker_config).await
        }));
        tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
            .await
            .unwrap();
        if body_race {
            api_put(&catalog, "test-bucket/object", b"api-winner").await;
        } else {
            catalog
                .update_object_metadata(
                    "test-bucket",
                    "object",
                    None,
                    Default::default(),
                    ObjectMetadataUpdate {
                        content_type: Some(Some("application/patched".into())),
                        metadata: Some(Some(BTreeMap::from([(
                            "key".into(),
                            Some("patched".into()),
                        )]))),
                        ..Default::default()
                    },
                    |_| Ok(()),
                )
                .await
                .unwrap();
        }
        hook.release.notify_one();
        let summary = worker.await.unwrap();
        let result = object(&catalog, "test-bucket/object").await;
        if body_race {
            assert_eq!(summary.ingested, 0);
            assert_eq!(result.size, 10);
            assert_eq!(scan(&catalog, &root, &config).await.conflicting, 1);
        } else {
            assert_eq!(summary.ingested, 1);
            assert_eq!(result.content_type, "application/patched");
            assert_eq!(result.metadata["key"], "patched");
        }
        assert_eq!(
            fs::read(tree.0.join("test-bucket/object")).unwrap(),
            b"external-edit"
        );
        drop(root);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn ingest_defers_partial_and_replacement_saves_then_resumes_activation() {
    let (data, tree, catalog, root, config) = fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/object"), b"partial").unwrap();
    let worker_catalog = catalog.clone();
    let worker_root = root.clone();
    let worker_config = config.clone();
    let hook = Arc::new(Hook {
        phase: "settle",
        entered: Default::default(),
        release: Default::default(),
    });
    let worker = tokio::spawn(HOOK.scope(hook.clone(), async move {
        scan(&worker_catalog, &worker_root, &worker_config).await
    }));
    tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
        .await
        .unwrap();
    fs::write(tree.0.join("test-bucket/object~"), b"completed").unwrap();
    fs::rename(
        tree.0.join("test-bucket/object~"),
        tree.0.join("test-bucket/object"),
    )
    .unwrap();
    hook.release.notify_one();
    assert_eq!(worker.await.unwrap().deferred, 1);
    drop(root);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = crate::sync::preflight(&catalog, &config)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan(&catalog, &root, &config).await.ingested, 1);
    assert_eq!(object(&catalog, "test-bucket/object").await.size, 9);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ingest_new_files_use_seed_mime_and_bucket_default_hold() {
    let (_data, tree, catalog, root, mut config) = fixture().await;
    config.seed_content_type = crate::mime::SeedContentType::Extension;
    let record = crate::json::parse_insert(
        br#"{"name":"test-bucket","defaultEventBasedHold":true}"#,
        "owner",
        std::time::SystemTime::UNIX_EPOCH,
    )
    .unwrap();
    catalog
        .admin(Origin::Api)
        .ensure_bucket(crate::storage::admin::BucketDefinition::new(record).unwrap())
        .await
        .unwrap();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/file.txt"), b"held").unwrap();
    assert_eq!(scan(&catalog, &root, &config).await.ingested, 1);
    let record = object(&catalog, "test-bucket/file.txt").await;
    assert_eq!(record.content_type, "text/plain; charset=utf-8");
    assert!(record.event_based_hold);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ingest_shutdown_finishes_the_active_publication_before_restart() {
    let (data, tree, catalog, root, config) = fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/first"), b"first").unwrap();
    fs::write(tree.0.join("test-bucket/second"), b"second").unwrap();
    let (stop, stopping) = tokio::sync::watch::channel(false);
    let hook = Arc::new(Hook {
        phase: "publish",
        entered: Default::default(),
        release: Default::default(),
    });
    let worker_catalog = catalog.clone();
    let worker_root = root.clone();
    let worker_config = config.clone();
    let worker = tokio::spawn(HOOK.scope(hook.clone(), async move {
        rescan(&worker_catalog, &worker_root, &worker_config, &stopping)
            .await
            .unwrap()
    }));
    tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
        .await
        .unwrap();
    stop.send_replace(true);
    hook.release.notify_one();
    assert_eq!(worker.await.unwrap().ingested, 1);
    let first = object(&catalog, "test-bucket/first").await;
    drop(root);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = crate::sync::preflight(&catalog, &config)
        .await
        .unwrap()
        .unwrap();
    assert!(!root.status().permits_external_deletions());
    let task = crate::sync::start(catalog.clone(), root.clone(), &config).unwrap();
    assert!(task.rescan().await.unwrap().baseline_complete);
    task.finish().await.unwrap();
    assert_eq!(
        object(&catalog, "test-bucket/first").await.generation,
        first.generation
    );
    assert_eq!(fs::read_dir(data.0.join("staging")).unwrap().count(), 0);
    drop(root);
    catalog.close().await.unwrap();
}

#[test]
fn ingest_strict_configuration_requires_an_identity_registry() {
    let tree = TestDir::new();
    let data = TestDir::new();
    let errors = Config::load(
        &[
            "--sync-dir".into(),
            tree.0.to_str().unwrap().into(),
            "--data-dir".into(),
            data.0.to_str().unwrap().into(),
            "--auth-mode".into(),
            "strict".into(),
        ],
        &BTreeMap::new(),
    )
    .unwrap_err();
    assert!(
        errors
            .errors
            .iter()
            .any(|error| error.fix.contains("requires identity_file"))
    );
}

#[tokio::test]
async fn ingest_deleted_bucket_stays_deleted_after_restart_until_directory_recreated() {
    let (data, tree, catalog, root, config) = fixture().await;
    let bucket = BucketName::new("deleted-bucket").unwrap();
    catalog
        .admin(Origin::Api)
        .ensure_bucket(
            crate::storage::admin::BucketDefinition::default_for(
                &bucket,
                "original-owner",
                &crate::clock::SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    crate::sync::reconcile(&catalog, &root, &config)
        .await
        .unwrap();
    catalog
        .delete_bucket(bucket.as_str(), None, None)
        .await
        .unwrap();
    fs::write(tree.0.join("deleted-bucket/retained"), b"do not import").unwrap();
    drop(root);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let root = crate::sync::preflight(&catalog, &config)
        .await
        .unwrap()
        .unwrap();
    let task = crate::sync::start(catalog.clone(), root.clone(), &config).unwrap();
    let summary = task.rescan().await.unwrap();
    assert_eq!(summary.blocked_buckets, 1);
    assert!(
        catalog
            .admin(Origin::Api)
            .get_bucket(&bucket)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        root.sync_buckets_page(String::new(), 256)
            .await
            .unwrap()
            .len(),
        1
    );
    fs::create_dir(tree.0.join("new-bucket")).unwrap();
    fs::write(tree.0.join("new-bucket/body"), b"new").unwrap();
    task.rescan().await.unwrap();
    assert_eq!(
        object(&catalog, "new-bucket/body").await.crc32c,
        crc32c::crc32c(b"new")
    );
    fs::remove_file(tree.0.join("deleted-bucket/retained")).unwrap();
    fs::remove_dir(tree.0.join("deleted-bucket")).unwrap();
    assert_eq!(task.rescan().await.unwrap().blocked_buckets, 0);
    fs::create_dir(tree.0.join("deleted-bucket")).unwrap();
    assert_eq!(task.rescan().await.unwrap().blocked_buckets, 0);
    assert_eq!(
        catalog
            .admin(Origin::Api)
            .get_bucket(&bucket)
            .await
            .unwrap()
            .unwrap()
            .project,
        config.seed_project
    );
    task.finish().await.unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ingest_unreadable_file_is_deferred_while_other_files_progress() {
    use std::os::unix::fs::PermissionsExt;
    let (_data, tree, catalog, root, config) = fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    let blocked = tree.0.join("test-bucket/blocked");
    fs::write(&blocked, b"protected").unwrap();
    fs::write(tree.0.join("test-bucket/working"), b"working").unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let summary = scan(&catalog, &root, &config).await;
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!((summary.ingested, summary.deferred), (1, 1));
    assert_eq!(object(&catalog, "test-bucket/working").await.size, 7);
    assert_eq!(scan(&catalog, &root, &config).await.ingested, 1);
    drop(root);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn ingest_unchanged_files_skip_settle_and_shutdown_interrupts_changed_settle() {
    let (_data, tree, catalog, root, config) = fixture().await;
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    fs::write(tree.0.join("test-bucket/object"), b"stable").unwrap();
    scan(&catalog, &root, &config).await;
    let hook = Arc::new(Hook {
        phase: "settle",
        entered: Default::default(),
        release: Default::default(),
    });
    let summary = tokio::time::timeout(
        Duration::from_secs(5),
        HOOK.scope(hook, scan(&catalog, &root, &config)),
    )
    .await
    .unwrap();
    assert_eq!((summary.ingested, summary.identical), (0, 0));
    fs::write(tree.0.join("test-bucket/object"), b"changed").unwrap();
    let hook = Arc::new(Hook {
        phase: "settle",
        entered: Default::default(),
        release: Default::default(),
    });
    let (stop, stopping) = tokio::sync::watch::channel(false);
    {
        let work = HOOK.scope(hook.clone(), rescan(&catalog, &root, &config, &stopping));
        tokio::pin!(work);
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = &mut work => panic!("expected a pending quiet checkpoint: {result:?}"),
                _ = hook.entered.notified() => {},
            }
        })
        .await
        .unwrap();
        hook.release.notify_one();
        // Drive the released checkpoint into the quiet wait before signalling shutdown.
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(work.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        stop.send_replace(true);
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(500), &mut work)
                .await
                .unwrap()
                .unwrap()
                .ingested,
            0
        );
    }
    assert_eq!(object(&catalog, "test-bucket/object").await.size, 6);
    drop(root);
    catalog.close().await.unwrap();
}
