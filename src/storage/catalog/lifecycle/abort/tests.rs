use super::*;
use crate::{
    bucket::{BucketAccess, BucketMetadataUpdate},
    clock::{Clock, ManualClock},
    object::ObjectTarget,
    storage::{
        admin::LocalAdmin,
        catalog::{
            fault::{self, Crash, Fault, Step},
            lifecycle::{ACTION_LIMIT, COALESCE, LifecycleOutcome, Pass},
            test_fixtures::{PERMISSIVE, body, bucket, permissive_upload},
            xml_multipart::{self, CompletedPart, Options, PartUpload, Target},
        },
    },
    test_support::TestDir,
};
use bytes::Bytes;
use serde_json::{Value, json};
use std::{
    io,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

const DAY: Duration = Duration::from_secs(86_400);
const MICROSECOND: Duration = Duration::from_micros(1);

async fn memory() -> (Catalog, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH + DAY));
    let catalog = Catalog::open_memory_for_server(
        64 * 1024 * 1024,
        16 * 1024 * 1024,
        clock.clone(),
        LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    catalog.configure_lifecycle(&Default::default());
    (catalog, clock)
}

async fn disk(data: &TestDir) -> (Catalog, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(UNIX_EPOCH + DAY));
    let catalog = Catalog::open_with_clock(&data.0, clock.clone())
        .await
        .unwrap();
    catalog.configure_lifecycle(&Default::default());
    (catalog, clock)
}

fn abort(condition: Value) -> Value {
    json!({"action":{"type":"AbortIncompleteMultipartUpload"},"condition":condition})
}

async fn create(catalog: &Catalog, rules: Vec<Value>) {
    let mut record = bucket("bucket", "project");
    record.lifecycle = rules;
    catalog.insert_bucket(record).await.unwrap();
}

async fn rules(catalog: &Catalog, rules: Vec<Value>) {
    catalog
        .patch_bucket(
            "bucket",
            BucketMetadataUpdate {
                lifecycle: Some(rules),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
}

async fn begin(catalog: &Catalog, name: &str) -> Target {
    catalog
        .begin_xml_multipart(
            "bucket",
            name,
            Options {
                content_type: "text/plain".into(),
                metadata: Default::default(),
            },
            BucketAccess::Permissive,
        )
        .await
        .unwrap()
        .target
}

async fn put(catalog: &Catalog, target: &Target) -> xml_multipart::Part {
    let lease = catalog
        .lease_xml_part(target.clone(), 1, BucketAccess::Permissive)
        .await
        .unwrap();
    catalog
        .put_xml_part(
            lease,
            PartUpload {
                expected: Default::default(),
                max_bytes: 100,
            },
            body(vec![Bytes::from_static(b"part")]),
        )
        .await
        .unwrap()
}

async fn execute(catalog: &Catalog) -> LifecycleCounts {
    let LifecycleOutcome::Executed(outcome) = catalog.run_lifecycle_now().await.unwrap() else {
        panic!("disabled lifecycle")
    };
    outcome
}

async fn count(catalog: &Catalog, query: &'static str) -> i64 {
    catalog
        .run::<_, CatalogError>(move |connection| Ok(sql::scalar(connection, query, [])?))
        .await
        .unwrap()
}

fn assert_counts(
    outcome: LifecycleCounts,
    scanned: u64,
    aborted: u64,
    skipped: u64,
    next: Option<SystemTime>,
) {
    assert_eq!(
        (
            outcome.scanned,
            outcome.aborted,
            outcome.deleted,
            outcome.archived,
            outcome.transitioned,
            outcome.blocked,
            outcome.skipped,
            outcome.next
        ),
        (scanned, aborted, 0, 0, 0, 0, skipped, next)
    );
}

#[tokio::test]
async fn multipart_abort_age_uses_initiation_and_exact_shared_deadlines() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![abort(json!({"age":1}))]).await;
    clock.advance(Duration::from_millis(123));
    let start = clock.now();
    let upload = begin(&catalog, "object").await;
    clock.advance(DAY - MICROSECOND);
    assert_counts(execute(&catalog).await, 1, 0, 0, Some(start + DAY));
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        1
    );
    catalog
        .list_xml_parts(upload.clone(), BucketAccess::Permissive, 0, 1000)
        .await
        .unwrap();
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(start + DAY)
    );
    clock.advance(MICROSECOND);
    assert_counts(
        execute(&catalog).await,
        1,
        1,
        0,
        Some(clock.now() + COALESCE),
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        0
    );
    clock.advance(COALESCE);
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_abort_zero_age_waits_for_next_midnight_including_midnight_initiation() {
    for offset in [Duration::ZERO, Duration::from_secs(36_000)] {
        let (catalog, clock) = memory().await;
        create(&catalog, vec![abort(json!({"age":0}))]).await;
        clock.advance(offset);
        begin(&catalog, "object").await;
        let due = UNIX_EPOCH + DAY * 2;
        assert_counts(execute(&catalog).await, 1, 0, 0, Some(due));
        clock.set(due - MICROSECOND);
        assert_counts(execute(&catalog).await, 1, 0, 0, Some(due));
        assert_eq!(
            count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
            1
        );
        clock.set(due);
        assert_counts(execute(&catalog).await, 1, 1, 0, Some(due + COALESCE));
        assert_eq!(
            count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
            0
        );
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn multipart_abort_name_conditions_and_multiple_rules_preserve_other_resources() {
    let (catalog, clock) = memory().await;
    create(
        &catalog,
        vec![
            abort(
                json!({"age":2,"matchesPrefix":["dir/","other/"],"matchesSuffix":[".jpg",".png"]}),
            ),
            abort(json!({"age":1,"matchesPrefix":["second/"]})),
            abort(json!({"age":1,"matchesPrefix":["dir/"],"matchesSuffix":[".jpg"]})),
        ],
    )
    .await;
    for name in [
        "dir/a.jpg",
        "other/a.png",
        "second/a",
        "Dir/a.jpg",
        "dir/a.JPG",
        "outside/a.jpg",
    ] {
        begin(&catalog, name).await;
    }
    catalog
        .create_resumable_session(
            "bucket",
            "dir/resumable.jpg",
            crate::storage::catalog::resumable::ResumableOptions::new(BucketAccess::Permissive),
            None,
        )
        .await
        .unwrap();
    catalog
        .put_object(
            permissive_upload("bucket", "dir/a.jpg", 100),
            body(vec![Bytes::from_static(b"object")]),
        )
        .await
        .unwrap();
    clock.advance(DAY);
    assert_counts(
        execute(&catalog).await,
        7,
        2,
        0,
        Some(clock.now() + COALESCE),
    );
    clock.advance(COALESCE);
    assert_counts(execute(&catalog).await, 5, 0, 0, Some(UNIX_EPOCH + DAY * 3));
    clock.set(UNIX_EPOCH + DAY * 3);
    assert_counts(
        execute(&catalog).await,
        5,
        1,
        0,
        Some(clock.now() + COALESCE),
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        3
    );
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 1);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM resumable_sessions").await,
        1
    );
    rules(
        &catalog,
        vec![json!({"action":{"type":"Delete"},"condition":{"age":1}})],
    )
    .await;
    assert_eq!(execute(&catalog).await.aborted, 0);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        3
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_abort_busy_leases_rearm_on_release_without_polling() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![abort(json!({"age":1}))]).await;
    let part = begin(&catalog, "part").await;
    let completing = begin(&catalog, "complete").await;
    let part_lease = catalog
        .lease_xml_part(part.clone(), 1, BucketAccess::Permissive)
        .await
        .unwrap();
    let complete_lease = catalog
        .lease_xml_multipart(completing.clone(), BucketAccess::Permissive)
        .await
        .unwrap();
    clock.advance(DAY);
    assert_counts(execute(&catalog).await, 2, 0, 2, None);
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(UNIX_EPOCH + DAY * 8)
    );
    let scans = catalog.0.lifecycle.state().scans;
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(UNIX_EPOCH + DAY * 8)
    );
    assert_eq!(catalog.0.lifecycle.state().scans, scans);
    drop(part_lease);
    drop(complete_lease);
    let LifecycleOutcome::Executed(gated) = catalog.run_lifecycle(Pass::Scheduled).await.unwrap()
    else {
        panic!("disabled")
    };
    assert_counts(gated, 0, 0, 0, Some(clock.now() + COALESCE));
    clock.advance(COALESCE);
    let LifecycleOutcome::Executed(outcome) = catalog.run_lifecycle(Pass::Scheduled).await.unwrap()
    else {
        panic!("disabled")
    };
    assert_counts(outcome, 2, 2, 0, Some(clock.now() + COALESCE));
    clock.advance(COALESCE);
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        0
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_abort_name_only_rules_match_without_an_age_condition() {
    let (catalog, clock) = memory().await;
    create(
        &catalog,
        vec![
            abort(json!({"matchesPrefix":["prefix/"]})),
            abort(json!({"matchesSuffix":[".tmp"]})),
        ],
    )
    .await;
    for name in ["prefix/value", "suffix.tmp", "keep"] {
        begin(&catalog, name).await;
    }
    assert_counts(
        execute(&catalog).await,
        3,
        2,
        0,
        Some(clock.now() + COALESCE),
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        1
    );
    clock.advance(COALESCE);
    assert_counts(execute(&catalog).await, 1, 0, 0, None);
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(UNIX_EPOCH + DAY * 8)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_abort_overflow_never_matches_or_schedules() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![abort(json!({"age":i32::MAX}))]).await;
    begin(&catalog, "object").await;
    clock.advance(DAY);
    assert_counts(execute(&catalog).await, 1, 0, 0, None);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        1
    );
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(UNIX_EPOCH + DAY * 8)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_lease_release_without_a_skipped_abort_does_not_dirty_the_schedule() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![abort(json!({"age":1}))]).await;
    let target = begin(&catalog, "object").await;
    let lease = catalog
        .lease_xml_part(target, 1, BucketAccess::Permissive)
        .await
        .unwrap();
    assert_counts(execute(&catalog).await, 1, 0, 0, Some(clock.now() + DAY));
    drop(lease);
    assert!(!catalog.0.lifecycle.dirty.load(Ordering::Acquire));
    let LifecycleOutcome::Executed(outcome) = catalog.run_lifecycle(Pass::Scheduled).await.unwrap()
    else {
        panic!("disabled")
    };
    assert_counts(outcome, 0, 0, 0, Some(clock.now() + DAY));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_abort_whole_lease_release_alone_rearms_the_next_pass() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![abort(json!({"age":1}))]).await;
    let target = begin(&catalog, "object").await;
    let lease = catalog
        .lease_xml_multipart(target, BucketAccess::Permissive)
        .await
        .unwrap();
    clock.advance(DAY);
    assert_counts(execute(&catalog).await, 1, 0, 1, None);
    assert!(!catalog.0.lifecycle.dirty.load(Ordering::Acquire));
    drop(lease);
    assert!(catalog.0.lifecycle.dirty.load(Ordering::Acquire));
    clock.advance(COALESCE);
    let LifecycleOutcome::Executed(outcome) = catalog.run_lifecycle(Pass::Scheduled).await.unwrap()
    else {
        panic!("disabled")
    };
    assert_counts(outcome, 1, 1, 0, Some(clock.now() + COALESCE));
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        0
    );
    clock.advance(COALESCE);
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_abort_success_does_not_rearm_on_an_unrelated_lease_release() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![abort(json!({"matchesPrefix":["abort/"]}))]).await;
    begin(&catalog, "abort/object").await;
    let keep = begin(&catalog, "keep").await;
    let lease = catalog
        .lease_xml_multipart(keep, BucketAccess::Permissive)
        .await
        .unwrap();
    assert_counts(
        execute(&catalog).await,
        2,
        1,
        0,
        Some(clock.now() + COALESCE),
    );
    clock.advance(COALESCE);
    assert_counts(execute(&catalog).await, 1, 0, 0, None);
    assert!(!catalog.0.lifecycle.dirty.load(Ordering::Acquire));
    drop(lease);
    assert!(!catalog.0.lifecycle.dirty.load(Ordering::Acquire));
    let LifecycleOutcome::Executed(outcome) = catalog.run_lifecycle(Pass::Scheduled).await.unwrap()
    else {
        panic!("disabled")
    };
    assert_counts(outcome, 0, 0, 0, None);
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        1
    );
    catalog.close().await.unwrap();
}

#[test]
fn multipart_abort_evaluation_rejects_object_only_conditions_without_validation() {
    for condition in [
        json!({"isLive":true}),
        json!({"matchesStorageClass":["STANDARD"]}),
        json!({"createdBefore":"9999-12-31"}),
    ] {
        let rule = serde_json::from_value(abort(condition)).unwrap();
        assert_eq!(
            multipart_abort_due(&[rule], "object", UNIX_EPOCH, UNIX_EPOCH + DAY),
            None
        );
    }
}

#[tokio::test]
async fn multipart_abort_reclaims_parts_without_notifications_on_disk_and_memory() {
    for disk_backend in [false, true] {
        let data = TestDir::new();
        let (catalog, clock) = if disk_backend {
            disk(&data).await
        } else {
            memory().await
        };
        create(&catalog, vec![abort(json!({"age":1}))]).await;
        catalog
            .insert_notification(
                "bucket",
                crate::notification::NotificationConfig {
                    topic: "//pubsub.googleapis.com/projects/project/topics/topic"
                        .parse()
                        .unwrap(),
                    payload_format: crate::notification::PayloadFormat::JsonApiV1,
                    event_types: crate::notification::EventTypes::default(),
                    custom_attributes: Default::default(),
                    object_name_prefix: String::new(),
                },
                PERMISSIVE,
            )
            .await
            .unwrap();
        let target = begin(&catalog, "object").await;
        put(&catalog, &target).await;
        let part_id: crate::storage::blob_id::BlobId = catalog
            .run::<_, CatalogError>(|connection| {
                Ok(sql::scalar(
                    connection,
                    "SELECT blob_id FROM xml_multipart_parts",
                    [],
                )?)
            })
            .await
            .unwrap();
        if !disk_backend {
            assert!(catalog.0.backend.open(&part_id).is_ok());
        }
        assert_eq!(
            count(&catalog, "SELECT count(*) FROM xml_multipart_parts").await,
            1
        );
        clock.advance(DAY);
        assert_counts(
            execute(&catalog).await,
            1,
            1,
            0,
            Some(clock.now() + COALESCE),
        );
        assert_eq!(
            count(&catalog, "SELECT count(*) FROM xml_multipart_parts").await,
            0
        );
        assert_eq!(
            count(&catalog, "SELECT count(*) FROM xml_multipart_deletions").await,
            1
        );
        if disk_backend {
            assert_eq!(
                std::fs::read_dir(data.0.join("staging")).unwrap().count(),
                1
            );
        }
        catalog.maintain_resumable_sessions().await.unwrap();
        if !disk_backend {
            assert!(
                matches!(catalog.0.backend.open(&part_id), Err(error) if error.kind() == io::ErrorKind::NotFound)
            );
        }
        assert_eq!(
            count(&catalog, "SELECT count(*) FROM xml_multipart_deletions").await,
            0
        );
        if disk_backend {
            assert_eq!(
                std::fs::read_dir(data.0.join("staging")).unwrap().count(),
                0
            );
        }
        assert!(catalog.next_notification().await.unwrap().is_none());
        assert_eq!(
            count(
                &catalog,
                "SELECT count(*) FROM change_journal WHERE object IS NOT NULL"
            )
            .await,
            0
        );
        assert_missing_xml_operations(&catalog, clock, &target).await;
        catalog.close().await.unwrap();
    }
}

async fn assert_missing_xml_operations(
    catalog: &Catalog,
    clock: Arc<ManualClock>,
    target: &Target,
) {
    use crate::{
        auth::Auth,
        config::{Config, RequestEndpoints},
        server::{ObjectActivity, ObjectCtx, RequestBody, State},
        xml::{self, ErrorBody, ReadRequest},
    };
    use http_body_util::{BodyExt, Full};
    let state = State {
        clock,
        ..State::for_test(catalog.clone(), Auth::permissive())
    };
    let config = Config::default();
    let endpoints = RequestEndpoints::explicit("http://localhost");
    let (activity, _) = tokio::sync::watch::channel(ObjectActivity::default());
    let ctx = ObjectCtx {
        state: &state,
        config: &config,
        endpoints: &endpoints,
        activity: &activity,
        caller: None,
        deadline: config.object_request_timeout.deadline(),
    };
    for (method, query, bytes) in [
        ("PUT", "&partNumber=1", "part"),
        ("GET", "", ""),
        (
            "POST",
            "",
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>\"test\"</ETag></Part></CompleteMultipartUpload>",
        ),
        ("DELETE", "", ""),
    ] {
        let request = hyper::Request::builder()
            .method(method)
            .uri(format!(
                "/bucket/{}?uploadId={}{}",
                target.name, target.id, query
            ))
            .header("content-length", bytes.len())
            .body(RequestBody::Buffered(Full::new(Bytes::copy_from_slice(
                bytes.as_bytes(),
            ))))
            .unwrap();
        let path = ReadRequest {
            target: ObjectTarget::new("bucket", &target.name, None),
            response_overrides: Default::default(),
        };
        let error = xml::multipart::respond(request, path, ctx)
            .await
            .unwrap_err();
        let response = error.response(ErrorBody::Include);
        assert_eq!(response.status(), hyper::StatusCode::NOT_FOUND, "{method}");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(
            std::str::from_utf8(&bytes)
                .unwrap()
                .contains("<Code>NoSuchUpload</Code>"),
            "{method}"
        );
    }
}

#[tokio::test]
async fn multipart_abort_completion_lease_wins_or_missing_id_prevents_completion() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![abort(json!({"age":1}))]).await;
    let completing = begin(&catalog, "complete").await;
    let part = put(&catalog, &completing).await;
    let lease = catalog
        .lease_xml_multipart(completing.clone(), BucketAccess::Permissive)
        .await
        .unwrap();
    clock.advance(DAY);
    assert_counts(execute(&catalog).await, 1, 0, 1, None);
    catalog
        .complete_xml_multipart(
            lease,
            vec![CompletedPart {
                number: 1,
                etag: part.etag,
            }],
            100,
        )
        .await
        .unwrap();
    assert_counts(execute(&catalog).await, 1, 0, 0, None);
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 1);
    let aborted = begin(&catalog, "aborted").await;
    put(&catalog, &aborted).await;
    clock.advance(DAY);
    assert_eq!(execute(&catalog).await.aborted, 1);
    assert!(matches!(
        catalog
            .lease_xml_multipart(aborted, BucketAccess::Permissive)
            .await,
        Err(xml_multipart::Error::NoSuchUpload)
    ));
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_abort_disabled_and_sync_defaults_preserve_uploads_until_enabled() {
    for explicit_disabled in [false, true] {
        let tree = TestDir::new();
        let (catalog, clock) = memory().await;
        create(&catalog, vec![abort(json!({"age":1}))]).await;
        begin(&catalog, "object").await;
        let mut config = crate::config::Config {
            sync_dir: Some(tree.0.clone()),
            ..Default::default()
        };
        if explicit_disabled {
            config.lifecycle_execution = Some(crate::config::LifecycleExecution::Disabled);
        }
        catalog.configure_lifecycle(&config);
        clock.advance(DAY);
        let outcome = catalog.run_lifecycle_now().await.unwrap();
        assert!(matches!(outcome, LifecycleOutcome::Disabled));
        assert_eq!(serde_json::to_value(outcome).unwrap()["aborted"], 0);
        assert_eq!(
            count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
            1
        );
        catalog.maintain_resumable_sessions().await.unwrap();
        assert_eq!(
            count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
            1
        );
        config.lifecycle_execution = Some(crate::config::LifecycleExecution::Enabled);
        catalog.configure_lifecycle(&config);
        let result = catalog.run_lifecycle_now().await.unwrap();
        assert_eq!(serde_json::to_value(result).unwrap()["aborted"], 1);
        assert_eq!(
            count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
            0
        );
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn multipart_abort_keyset_pages_share_the_action_budget_with_object_deletion() {
    let (catalog, clock) = memory().await;
    create(
        &catalog,
        vec![
            abort(json!({"age":1})),
            json!({"action":{"type":"Delete"},"condition":{"age":1}}),
        ],
    )
    .await;
    for _ in 0..ACTION_LIMIT + 1 {
        begin(&catalog, "same-name").await;
    }
    catalog
        .put_object(
            permissive_upload("bucket", "object", 100),
            body(vec![Bytes::from_static(b"object")]),
        )
        .await
        .unwrap();
    clock.advance(DAY);
    assert_counts(
        execute(&catalog).await,
        ACTION_LIMIT,
        ACTION_LIMIT,
        0,
        Some(clock.now()),
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        1
    );
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 1);
    let second = execute(&catalog).await;
    assert_eq!(
        (second.scanned, second.aborted, second.deleted, second.next),
        (2, 1, 1, Some(clock.now() + COALESCE))
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        0
    );
    assert_eq!(count(&catalog, "SELECT count(*) FROM objects").await, 0);
    clock.advance(COALESCE);
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_aborts_transitions_deletes_and_archives_share_one_strict_action_limit() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![
        abort(json!({"age":1,"matchesPrefix":["abort/"]})),
        json!({"action":{"type":"Delete"},"condition":{"age":1,"matchesPrefix":["delete/"]}}),
        json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":1,"matchesPrefix":["transition/"]}}),
    ]).await;
    let mut versioned = bucket("versions", "project");
    versioned.versioning_enabled = true;
    versioned.lifecycle =
        vec![json!({"action":{"type":"Delete"},"condition":{"age":1,"isLive":true}})];
    catalog.insert_bucket(versioned).await.unwrap();
    for _ in 0..ACTION_LIMIT - 4 {
        begin(&catalog, "abort/value").await;
    }
    for (bucket, name) in [
        ("bucket", "transition/object"),
        ("bucket", "delete/object"),
        ("versions", "archive/a"),
        ("versions", "archive/b"),
        ("versions", "archive/c"),
    ] {
        catalog
            .put_object(
                permissive_upload(bucket, name, 100),
                body(vec![Bytes::from_static(b"object")]),
            )
            .await
            .unwrap();
    }
    clock.advance(DAY);
    let first = execute(&catalog).await;
    let actions = (
        first.aborted,
        first.transitioned,
        first.deleted,
        first.archived,
    );
    assert_eq!(actions, (ACTION_LIMIT - 4, 1, 1, 2));
    assert_eq!(first.applied(), ACTION_LIMIT);
    assert_eq!(first.scanned, ACTION_LIMIT);
    assert_eq!((first.blocked, first.skipped), (0, 0));
    assert_eq!(first.next, Some(clock.now()));
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        0
    );
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM objects WHERE bucket='bucket' AND name='delete/object'"
        )
        .await,
        0
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM objects WHERE bucket='bucket' AND name='transition/object' AND storage_class='COLDLINE'").await,
        1
    );
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM objects WHERE bucket='versions' AND time_deleted IS NOT NULL"
        )
        .await,
        2
    );
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM objects WHERE bucket='versions' AND time_deleted IS NULL"
        )
        .await,
        1
    );
    let second = execute(&catalog).await;
    let actions = (
        second.aborted,
        second.transitioned,
        second.deleted,
        second.archived,
    );
    assert_eq!(actions, (0, 0, 0, 1));
    assert_eq!(second.applied(), 1);
    assert_eq!(second.scanned, 4);
    assert_eq!((second.blocked, second.skipped), (0, 0));
    assert_eq!(second.next, Some(clock.now() + COALESCE));
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM objects WHERE bucket='versions' AND time_deleted IS NOT NULL"
        )
        .await,
        3
    );
    assert_eq!(
        count(
            &catalog,
            "SELECT count(*) FROM objects WHERE bucket='versions' AND time_deleted IS NULL"
        )
        .await,
        0
    );
    clock.advance(COALESCE);
    assert_counts(execute(&catalog).await, 4, 0, 0, None);
    assert_eq!(
        catalog.maintain_resumable_sessions().await.unwrap(),
        Some(clock.now() + COALESCE)
    );
    clock.advance(COALESCE);
    assert_eq!(catalog.maintain_resumable_sessions().await.unwrap(), None);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_abort_scan_seeks_the_existing_list_index_without_sorting() {
    let (catalog, _) = memory().await;
    let plan = catalog
        .run::<_, CatalogError>(|connection| {
            Ok(connection
                .prepare_cached(&format!("EXPLAIN QUERY PLAN {UPLOAD_SCAN}"))?
                .query_map(
                    params!["bucket", "", "", "incarnation", UPLOAD_PAGE],
                    |row| row.get::<_, String>(3),
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .unwrap();
    assert!(
        plan.iter().any(|step| step
            .contains("SEARCH xml_multipart_uploads USING INDEX xml_multipart_uploads_list (bucket=? AND (name,id)>(?,?))")),
        "{plan:?}"
    );
    assert!(
        !plan
            .iter()
            .any(|step| step.starts_with("SCAN") || step.contains("TEMP B-TREE")),
        "{plan:?}"
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_abort_does_not_scan_uploads_for_other_actions_or_quarantined_rules() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![]).await;
    begin(&catalog, "object").await;
    clock.advance(DAY);
    for configured in [
        vec![],
        vec![json!({"action":{"type":"Delete"},"condition":{"age":1}})],
        vec![
            json!({"action":{"type":"SetStorageClass","storageClass":"COLDLINE"},"condition":{"age":1}}),
        ],
    ] {
        rules(&catalog, configured).await;
        assert_counts(execute(&catalog).await, 0, 0, 0, None);
    }
    catalog
        .run::<_, CatalogError>(|connection| {
            sql::exec(
                connection,
                "UPDATE buckets SET lifecycle = '[{}]' WHERE name = 'bucket'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_counts(
        execute(&catalog).await,
        0,
        0,
        0,
        Some(clock.now() + COALESCE),
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        1
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM lifecycle_quarantine").await,
        1
    );
    rules(&catalog, vec![abort(json!({"age":1}))]).await;
    assert_eq!(execute(&catalog).await.aborted, 1);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_abort_rechecks_bucket_incarnation_after_recreation() {
    let (catalog, clock) = memory().await;
    create(&catalog, vec![abort(json!({"age":1}))]).await;
    let old = begin(&catalog, "object").await;
    let lease = catalog
        .lease_xml_part(old, 1, BucketAccess::Permissive)
        .await
        .unwrap();
    clock.advance(DAY);
    assert_counts(execute(&catalog).await, 1, 0, 1, None);
    catalog
        .delete_bucket("bucket", Default::default(), PERMISSIVE)
        .await
        .unwrap();
    create(&catalog, vec![abort(json!({"age":1}))]).await;
    begin(&catalog, "object").await;
    drop(lease);
    assert_counts(execute(&catalog).await, 1, 0, 0, Some(UNIX_EPOCH + DAY * 3));
    catalog.maintain_resumable_sessions().await.unwrap();
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        1
    );
    clock.advance(COALESCE);
    assert_counts(execute(&catalog).await, 1, 0, 0, Some(UNIX_EPOCH + DAY * 3));
    clock.set(UNIX_EPOCH + DAY * 3);
    assert_counts(
        execute(&catalog).await,
        1,
        1,
        0,
        Some(clock.now() + COALESCE),
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_abort_commit_failure_rolls_back_and_retries_without_losing_parts() {
    let data = TestDir::new();
    let (catalog, clock) = disk(&data).await;
    create(&catalog, vec![abort(json!({"age":1}))]).await;
    let target = begin(&catalog, "object").await;
    put(&catalog, &target).await;
    clock.advance(DAY);
    fault::arm(
        &data.0,
        Step::LifecycleCommit,
        0,
        Fault::Io(io::ErrorKind::Other),
    );
    assert!(catalog.run_lifecycle_now().await.is_err());
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_uploads").await,
        1
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_parts").await,
        1
    );
    assert_eq!(
        count(&catalog, "SELECT count(*) FROM xml_multipart_deletions").await,
        0
    );
    assert_eq!(execute(&catalog).await.aborted, 1);
    catalog.maintain_resumable_sessions().await.unwrap();
    assert_eq!(
        std::fs::read_dir(data.0.join("staging")).unwrap().count(),
        0
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn multipart_abort_restart_mid_pass_and_power_loss_recover_the_deletion_journal() {
    for crash in Crash::ALL {
        for step in [Step::LifecycleCommit, Step::LifecycleCommitted] {
            let data = TestDir::new();
            let snapshot = TestDir::new();
            let (catalog, clock) = disk(&data).await;
            create(&catalog, vec![abort(json!({"age":1}))]).await;
            for _ in 0..129 {
                let target = begin(&catalog, "object").await;
                put(&catalog, &target).await;
            }
            clock.advance(DAY);
            fault::arm(&data.0, step, 1, crash.fault(snapshot.0.clone()));
            assert_eq!(execute(&catalog).await.aborted, 129);
            assert_eq!(fault::pending(&data.0), 0);
            catalog.close().await.unwrap();
            let recovered = Catalog::open_with_clock(&snapshot.0, clock.clone())
                .await
                .unwrap();
            recovered.configure_lifecycle(&Default::default());
            assert_eq!(
                execute(&recovered).await.aborted,
                u64::from(step == Step::LifecycleCommit)
            );
            recovered.maintain_resumable_sessions().await.unwrap();
            assert_eq!(
                count(&recovered, "SELECT count(*) FROM xml_multipart_uploads").await,
                0
            );
            assert_eq!(
                count(&recovered, "SELECT count(*) FROM xml_multipart_deletions").await,
                0
            );
            assert_eq!(
                std::fs::read_dir(snapshot.0.join("staging"))
                    .unwrap()
                    .count(),
                0
            );
            assert_eq!(execute(&recovered).await.aborted, 0);
            recovered.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn multipart_abort_reclamation_failures_leave_a_retryable_journal() {
    for step in [
        Step::MultipartReclaim,
        #[cfg(unix)]
        Step::MultipartReclaimSync,
        Step::MultipartReclaimForget,
    ] {
        let data = TestDir::new();
        let (catalog, clock) = disk(&data).await;
        create(&catalog, vec![abort(json!({"age":1}))]).await;
        let target = begin(&catalog, "object").await;
        put(&catalog, &target).await;
        clock.advance(DAY);
        assert_eq!(execute(&catalog).await.aborted, 1);
        fault::arm(&data.0, step, 0, Fault::Io(io::ErrorKind::Other));
        assert!(catalog.maintain_resumable_sessions().await.is_err());
        assert_eq!(
            count(&catalog, "SELECT count(*) FROM xml_multipart_deletions").await,
            1
        );
        catalog.maintain_resumable_sessions().await.unwrap();
        assert_eq!(
            count(&catalog, "SELECT count(*) FROM xml_multipart_deletions").await,
            0
        );
        assert_eq!(
            std::fs::read_dir(data.0.join("staging")).unwrap().count(),
            0
        );
        catalog.close().await.unwrap();
    }
}
