use super::*;
use crate::storage::catalog::{
    fault::{Crash, Fault, Step},
    rewrite::RewriteStart,
    xml_multipart::{Options, PartUpload},
};
use crate::storage::test_fixtures::{PERMISSIVE, body, bucket, live, permissive_upload, version};
use crate::{
    bucket::{BucketAccess, RetentionPolicy, SoftDeletePolicy, Versioning},
    clock::SystemClock,
    notification::{EventTypes, NotificationConfig, PayloadFormat},
    object::{ObjectConditions, ObjectTarget, UploadChunk},
    storage::{DerivedDestination, ResumableOptions, SessionId, admin::LocalAdmin},
    test_support::{TestDir, eventually_async},
};
use bytes::Bytes;
use std::{
    io,
    path::Path,
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

const TARGET: &str = "purged-bucket";
const OTHER: &str = "kept-bucket";

/// Counts the rows of `TARGET` (bound as `?1`) in each table the purge deletes or cascades.
const TARGET_ROWS: &[(&str, &str)] = &[
    ("buckets", "SELECT count(*) FROM buckets WHERE name = ?1"),
    (
        "soft_deleted_buckets",
        "SELECT count(*) FROM soft_deleted_buckets WHERE name = ?1",
    ),
    (
        "objects",
        "SELECT count(*) FROM objects WHERE bucket = ?1 OR substr(bucket, 1, length(?1) + 1) = ?1 || '#'",
    ),
    (
        "acl_object_entries",
        "SELECT count(*) FROM acl_object_entries WHERE bucket = ?1 OR substr(bucket, 1, length(?1) + 1) = ?1 || '#'",
    ),
    (
        "acl_bucket_entries",
        "SELECT count(*) FROM acl_bucket_entries WHERE bucket = ?1",
    ),
    (
        "bucket_policies",
        "SELECT count(*) FROM bucket_policies WHERE bucket = ?1",
    ),
    (
        "seed_objects",
        "SELECT count(*) FROM seed_objects WHERE bucket = ?1",
    ),
    (
        "bucket_incarnations",
        "SELECT count(*) FROM bucket_incarnations WHERE bucket = ?1",
    ),
    (
        "bucket_notifications",
        "SELECT count(*) FROM bucket_notifications WHERE bucket = ?1",
    ),
    (
        "bucket_notification_ids",
        "SELECT count(*) FROM bucket_notification_ids WHERE bucket = ?1",
    ),
    (
        "bucket_file_entries",
        "SELECT count(*) FROM bucket_file_entries WHERE bucket = ?1",
    ),
    (
        "resumable_sessions",
        "SELECT count(*) FROM resumable_sessions WHERE bucket = ?1",
    ),
    (
        "xml_multipart_uploads",
        "SELECT count(*) FROM xml_multipart_uploads WHERE bucket = ?1",
    ),
    (
        "xml_multipart_parts",
        "SELECT count(*) FROM xml_multipart_parts p JOIN xml_multipart_uploads u ON u.id = p.upload_id WHERE u.bucket = ?1",
    ),
    (
        "appendable_objects",
        "SELECT count(*) FROM appendable_objects WHERE bucket = ?1",
    ),
    (
        "rewrite_sessions",
        "SELECT count(*) FROM rewrite_sessions WHERE json_extract(request, '$.path[0]') = ?1 OR json_extract(request, '$.path[2]') = ?1",
    ),
    (
        "upload_replays",
        "SELECT count(*) FROM upload_replays WHERE incarnation IN (SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1)",
    ),
    (
        "notification_outbox",
        "SELECT count(*) FROM notification_outbox WHERE json_extract(message, '$.messages[0].attributes.bucketId') = ?1",
    ),
];

/// Counts state that only the target's sessions create in these fixtures, and the journals that
/// cleanup after commit drains.
const TARGET_ONLY_ROWS: &[(&str, &str)] = &[
    (
        "derived_transfers",
        "SELECT count(*) FROM derived_transfers",
    ),
    ("derived_inputs", "SELECT count(*) FROM derived_inputs"),
    (
        "pending_blob_deletions",
        "SELECT count(*) FROM pending_blob_deletions",
    ),
    (
        "xml_multipart_deletions",
        "SELECT count(*) FROM xml_multipart_deletions",
    ),
];

async fn open(path: &Path) -> (Catalog, Admin) {
    let catalog = Catalog::open_for_server(path, Arc::new(SystemClock), LocalAdmin::Enabled)
        .await
        .unwrap();
    let admin = catalog.local_admin().unwrap();
    (catalog, admin)
}

async fn open_memory(catalog_bytes: u64) -> (Catalog, Admin) {
    let catalog = Catalog::open_memory_for_server(
        64 * 1024 * 1024,
        catalog_bytes,
        Arc::new(SystemClock),
        LocalAdmin::Enabled,
    )
    .await
    .unwrap();
    let admin = catalog.local_admin().unwrap();
    (catalog, admin)
}

fn target() -> BucketName {
    BucketName::new(TARGET).unwrap()
}

fn options() -> ResumableOptions {
    ResumableOptions::new(BucketAccess::Permissive)
}

async fn put(catalog: &Catalog, bucket: &str, name: &str, bytes: &'static [u8]) -> i64 {
    catalog
        .put_object(
            permissive_upload(bucket, name, 1024),
            body(vec![Bytes::from_static(bytes)]),
        )
        .await
        .unwrap()
        .generation
}

/// Starts a resumable upload, which stages a file on disk or a draft in RAM.
async fn start_session(catalog: &Catalog, bucket: &str) -> SessionId {
    catalog
        .create_resumable_session(bucket, "pending", options(), None)
        .await
        .unwrap()
        .id
}

/// Runs one statement, binding the target bucket's name as `?1` when it takes a parameter.
async fn exec(catalog: &Catalog, statement: &'static str) {
    catalog
        .run::<_, CatalogError>(move |connection| {
            let mut statement = connection.prepare_cached(statement)?;
            if statement.parameter_count() == 0 {
                statement.execute([])?;
            } else {
                statement.execute([TARGET])?;
            }
            Ok(())
        })
        .await
        .unwrap();
}

async fn counts(
    catalog: &Catalog,
    queries: &'static [(&'static str, &'static str)],
) -> Vec<(&'static str, i64)> {
    catalog
        .run::<_, CatalogError>(move |connection| {
            let mut counts = Vec::new();
            for (table, query) in queries {
                let mut statement = connection.prepare_cached(query)?;
                let count = if statement.parameter_count() == 0 {
                    statement.query_row([], |row| row.get(0))?
                } else {
                    statement.query_row([TARGET], |row| row.get(0))?
                };
                counts.push((*table, count));
            }
            Ok(counts)
        })
        .await
        .unwrap()
}

async fn target_objects(catalog: &Catalog) -> u64 {
    u64::try_from(counts(catalog, &TARGET_ROWS[2..3]).await[0].1).unwrap()
}

async fn assert_purged(catalog: &Catalog) {
    let mut remaining = counts(catalog, TARGET_ROWS).await;
    remaining.extend(counts(catalog, TARGET_ONLY_ROWS).await);
    for (table, count) in remaining {
        assert_eq!(count, 0, "the purge left rows in {table}");
    }
}

/// The queued notification events that name `bucket`.
async fn outbox_rows(catalog: &Catalog, bucket: &'static str) -> i64 {
    catalog
        .run::<_, CatalogError>(move |connection| {
            Ok(sql::scalar(
                connection,
                "SELECT count(*) FROM notification_outbox WHERE json_extract(message, '$.messages[0].attributes.bucketId') = ?1",
                [bucket],
            )?)
        })
        .await
        .unwrap()
}

fn notification(topic: &str) -> NotificationConfig {
    NotificationConfig {
        topic: topic.parse().unwrap(),
        payload_format: PayloadFormat::JsonApiV1,
        event_types: EventTypes::default(),
        custom_attributes: Default::default(),
        object_name_prefix: String::new(),
    }
}

/// Every generation a bucket name can hold: a soft-deleted version that retains a soft-deleted
/// object, then a live versioned bucket with a current, a noncurrent and a soft-deleted generation.
async fn generations(catalog: &Catalog) {
    let soft_delete = Some(SoftDeletePolicy {
        retention_duration_seconds: 3600,
        effective_time: UNIX_EPOCH,
    });
    let mut first = bucket(TARGET, "project");
    first.soft_delete_policy = soft_delete.clone();
    catalog.insert_bucket(first).await.unwrap();
    put(catalog, TARGET, "gone", b"gone").await;
    catalog
        .delete_object(live(TARGET, "gone"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    catalog
        .delete_bucket(TARGET, Default::default(), PERMISSIVE)
        .await
        .unwrap();
    let mut second = bucket(TARGET, "project");
    second.versioning = Versioning::Enabled;
    second.soft_delete_policy = soft_delete;
    catalog.insert_bucket(second).await.unwrap();
    put(catalog, TARGET, "value", b"first").await;
    put(catalog, TARGET, "value", b"second").await;
    let removed = put(catalog, TARGET, "removed", b"removed").await;
    for target in [live(TARGET, "removed"), version(TARGET, "removed", removed)] {
        catalog
            .delete_object(target, ObjectConditions::default(), PERMISSIVE)
            .await
            .unwrap();
    }
    protect(catalog).await;
}

/// Gives the live generations both holds and a locked retention, under a locked bucket retention
/// policy, so the API cannot delete any of them.
async fn protect(catalog: &Catalog) {
    exec(catalog, "UPDATE objects SET temporary_hold = 1, event_based_hold = 1, retention_mode = 'Locked', retain_until_time = 9000000000000000 WHERE bucket = ?1").await;
    let policy = serde_json::to_string(&RetentionPolicy {
        retention_period: 3_000_000_000,
        effective_time: UNIX_EPOCH,
        is_locked: true,
    })
    .unwrap();
    catalog
        .run::<_, CatalogError>(move |connection| {
            sql::exec(
                connection,
                "UPDATE buckets SET retention_policy = ?2 WHERE name = ?1",
                rusqlite::params![TARGET, policy],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let refused = catalog
        .delete_object(live(TARGET, "value"), Default::default(), PERMISSIVE)
        .await;
    assert!(refused.is_err());
}

/// One session of each kind on the target, each with its bytes staged.
async fn sessions(catalog: &Catalog) {
    start_session(catalog, TARGET).await;
    let upload = catalog
        .begin_xml_multipart(
            TARGET,
            "parts",
            Options {
                content_type: "text/plain".into(),
                metadata: Default::default(),
            },
            BucketAccess::Permissive,
        )
        .await
        .unwrap();
    let lease = catalog
        .lease_xml_part(upload.target, 1, BucketAccess::Permissive)
        .await
        .unwrap();
    catalog
        .put_xml_part(
            lease,
            PartUpload {
                expected: Default::default(),
                max_bytes: 1024,
            },
            body(vec![Bytes::from_static(b"part")]),
        )
        .await
        .unwrap();
    let mut writer = catalog
        .create_appendable(TARGET, "append", options())
        .await
        .unwrap();
    writer
        .append(0, Bytes::from_static(b"append"), 1024)
        .await
        .unwrap();
    writer.flush().await.unwrap();
    drop(writer);
    rewrite(catalog).await;
}

/// Starts a rewrite of the target's object that copies one byte per call, so it stays in progress
/// with its source pinned.
async fn rewrite(catalog: &Catalog) {
    let source = catalog
        .resolve_derived_source(live(TARGET, "value"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    let start = RewriteStart {
        binding: crate::storage::rewrite::RewriteBinding {
            source: ObjectTarget::new(TARGET, "value", source.object.generation),
            destination: ObjectTarget::new(TARGET, "copy", ()),
            parameters: Default::default(),
            metadata: None,
        },
        destination: DerivedDestination {
            bucket: TARGET.into(),
            name: "copy".into(),
            content_type: "text/plain".into(),
            ..DerivedDestination::new(BucketAccess::Permissive)
        },
        source_conditions: ObjectConditions::default(),
        source_metageneration: source.object.metageneration,
        source_access: BucketAccess::Permissive,
        source_key_sha256: None,
        budget: std::num::NonZeroU64::new(1).unwrap(),
    };
    let size = source.object.size;
    let (_, mut owner) = catalog
        .begin_rewrite_session(vec![source.whole()], size, start)
        .await
        .unwrap();
    owner.disarm();
}

/// The target's records that no request path in these tests writes: a notification configuration
/// and its queued event, a bucket ACL, an initialization record and a replay record.
async fn records(catalog: &Catalog) {
    let config = notification("projects/local/topics/purged");
    catalog
        .insert_notification(TARGET, config, PERMISSIVE)
        .await
        .unwrap();
    put(catalog, TARGET, "announced", b"announced").await;
    for statement in [
        "INSERT INTO acl_bucket_entries VALUES (?1, 'bucket', '[]')",
        "INSERT INTO bucket_file_entries VALUES (?1, zeroblob(32), 0)",
        "INSERT INTO upload_replays SELECT randomblob(32), randomblob(32), incarnation, 0, 600000000, randomblob(32), '{}' FROM bucket_incarnations WHERE bucket = ?1",
        "INSERT INTO seed_objects VALUES (?1, 'announced', NULL, NULL, NULL, NULL, 9, zeroblob(16), 0, zeroblob(8), zeroblob(8), zeroblob(8), zeroblob(16), zeroblob(16), 0, NULL)",
    ] {
        exec(catalog, statement).await;
    }
}

/// Another bucket with an object and a session, which the purge must leave alone.
async fn other(catalog: &Catalog) -> SessionId {
    catalog
        .insert_bucket(bucket(OTHER, "project"))
        .await
        .unwrap();
    put(catalog, OTHER, "kept", b"kept").await;
    start_session(catalog, OTHER).await
}

async fn assert_other_intact(catalog: &Catalog, session: SessionId) {
    let record = catalog
        .get_object(live(OTHER, "kept"), Default::default(), PERMISSIVE)
        .await
        .unwrap();
    assert_eq!(record.size, 4);
    catalog
        .resumable_session(&session.to_string(), OTHER)
        .await
        .unwrap();
}

/// Regular files in `directory`, counting one level of subdirectories as blob shards are laid out.
fn file_count(directory: &Path) -> usize {
    std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                std::fs::read_dir(entry.path()).unwrap().count()
            } else {
                1
            }
        })
        .sum()
}

fn ram_blobs(catalog: &Catalog) -> usize {
    let crate::storage::catalog::BlobBackend::Memory(store) = &catalog.0.backend else {
        panic!("a RAM catalog stores its bytes in RAM");
    };
    store.blob_count()
}

#[tokio::test]
async fn purge_classifies_every_catalog_table() {
    let dir = TestDir::new();
    let (catalog, _admin) = open(&dir.0).await;
    let tables = catalog
        .run::<_, CatalogError>(|connection| {
            connection
                .prepare_cached("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(CatalogError::from)
        })
        .await
        .unwrap();
    let classes = [DELETED, CASCADED, INCARNATION_BOUND, INSTANCE_WIDE];
    for table in &tables {
        let classified = classes
            .iter()
            .filter(|class| class.contains(&table.as_str()))
            .count();
        assert_eq!(classified, 1, "ambiguous or missing purge policy: {table}");
    }
    for table in classes.iter().copied().flatten() {
        assert!(
            tables.iter().any(|existing| existing == table),
            "obsolete purge policy: {table}"
        );
    }
    // Every table whose rows of the bucket go has a check that they did.
    for table in DELETED.iter().chain(CASCADED) {
        assert!(
            TARGET_ROWS.iter().any(|(checked, _)| checked == table),
            "unchecked purge deletion: {table}"
        );
    }
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn purge_removes_every_generation_session_and_record_past_holds_and_spares_other_buckets() {
    let dir = TestDir::new();
    let (catalog, admin) = open(&dir.0).await;
    generations(&catalog).await;
    records(&catalog).await;
    sessions(&catalog).await;
    let session = other(&catalog).await;
    let config = notification("projects/local/topics/kept");
    catalog
        .insert_notification(OTHER, config, PERMISSIVE)
        .await
        .unwrap();
    put(&catalog, OTHER, "announced", b"announced").await;
    for (table, count) in counts(&catalog, TARGET_ROWS).await {
        assert!(count > 0, "the fixture leaves {table} without bucket rows");
    }
    for (table, count) in counts(&catalog, &TARGET_ONLY_ROWS[..2]).await {
        assert!(count > 0, "the fixture leaves {table} empty");
    }
    let stored = target_objects(&catalog).await;
    let outcome = admin.purge_bucket(&target()).await.unwrap();
    assert_eq!(
        serde_json::to_value(&outcome).unwrap(),
        serde_json::json!({"objects": stored, "cleanupPending": false})
    );
    assert_purged(&catalog).await;
    assert_other_intact(&catalog, session).await;
    assert_eq!(outbox_rows(&catalog, OTHER).await, 1);
    // Only the other bucket's objects and session staging remain on disk.
    assert_eq!(file_count(&dir.0.join("blobs")), 2);
    assert_eq!(file_count(&dir.0.join("staging")), 1);
    assert_eq!(file_count(&dir.0.join("appendable")), 0);
    assert_eq!(
        admin.purge_bucket(&target()).await.unwrap_err(),
        PurgeError::NotFound
    );
    catalog.close().await.unwrap();
    let (reopened, admin) = open(&dir.0).await;
    assert_other_intact(&reopened, session).await;
    reopened
        .insert_bucket(bucket(TARGET, "project"))
        .await
        .unwrap();
    assert_eq!(target_objects(&reopened).await, 0);
    assert_eq!(admin.purge_bucket(&target()).await.unwrap().objects, 0);
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn purge_on_memory_returns_the_buckets_ram_and_keeps_other_uploads() {
    let (catalog, admin) = open_memory(8 * 1024 * 1024).await;
    generations(&catalog).await;
    sessions(&catalog).await;
    let session = other(&catalog).await;
    let stored = target_objects(&catalog).await;
    let outcome = admin.purge_bucket(&target()).await.unwrap();
    assert_eq!((outcome.objects, outcome.cleanup_pending), (stored, false));
    assert_purged(&catalog).await;
    assert_other_intact(&catalog, session).await;
    assert_eq!(ram_blobs(&catalog), 1);
    catalog.close().await.unwrap();
}

/// Holds an upload to the other bucket open, purges the target, then lets the upload finish.
#[tokio::test]
async fn purge_does_not_wait_for_an_upload_to_another_bucket() {
    let dir = TestDir::new();
    for memory in [false, true] {
        let (catalog, admin) = if memory {
            open_memory(8 * 1024 * 1024).await
        } else {
            open(&dir.0).await
        };
        generations(&catalog).await;
        other(&catalog).await;
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        let uploader = catalog.clone();
        let upload = crate::task::spawn_supervised("purge-test-upload", async move {
            uploader
                .put_object(permissive_upload(OTHER, "late", 1024), receiver)
                .await
        });
        sender
            .send(Ok(UploadChunk::Data(Bytes::from_static(b"late"))))
            .await
            .unwrap();
        eventually_async("the upload to hold its lease", || async {
            catalog.active_uploads() == 1
        })
        .await;
        let purge = tokio::time::timeout(Duration::from_secs(30), admin.purge_bucket(&target()))
            .await
            .expect("a purge does not wait for another bucket's upload");
        assert!(purge.is_ok(), "{purge:?}");
        sender.send(Ok(UploadChunk::End)).await.unwrap();
        assert_eq!(upload.await.unwrap().unwrap().size, 4);
        catalog.close().await.unwrap();
    }
}

#[tokio::test]
async fn purge_refuses_without_changes_a_missing_name_an_import_and_writers() {
    let dir = TestDir::new();
    let (catalog, admin) = open(&dir.0).await;
    generations(&catalog).await;
    let stored = target_objects(&catalog).await;
    let missing = BucketName::new("missing-bucket").unwrap();
    let refusal = admin.purge_bucket(&missing).await.unwrap_err();
    assert_eq!(refusal, PurgeError::NotFound);
    // An import starts only into an empty catalog, so this records one as an interrupted run would.
    exec(&catalog, "INSERT INTO import_journal (singleton, source, fingerprint, expected, cursor, active) VALUES (1, zeroblob(32), zeroblob(32), 1, 0, 1)").await;
    assert!(catalog.import_journal().in_progress().await.unwrap());
    let refusal = admin.purge_bucket(&target()).await.unwrap_err();
    assert_eq!(refusal, PurgeError::Importing);
    let finished = "UPDATE import_journal SET active = 0, cursor = expected";
    exec(&catalog, finished).await;
    let session = start_session(&catalog, TARGET).await;
    let lease = catalog
        .lock_resumable_session(&session.to_string(), TARGET)
        .await
        .unwrap();
    let refusal = admin.purge_bucket(&target()).await.unwrap_err();
    assert_eq!(refusal, PurgeError::WriterBusy);
    drop(lease);
    let writer = catalog
        .create_appendable(TARGET, "append", options())
        .await
        .unwrap();
    let refusal = admin.purge_bucket(&target()).await.unwrap_err();
    assert_eq!(refusal, PurgeError::WriterBusy);
    drop(writer);
    assert_eq!(target_objects(&catalog).await, stored);
    catalog
        .resumable_session(&session.to_string(), TARGET)
        .await
        .unwrap();
    // The claims a refusal took are gone with it, so the next purge gets them.
    assert_eq!(admin.purge_bucket(&target()).await.unwrap().objects, stored);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn purge_failed_commit_changes_nothing_and_an_error_after_commit_resolves_as_success() {
    let dir = TestDir::new();
    let (catalog, admin) = open(&dir.0).await;
    generations(&catalog).await;
    let session = other(&catalog).await;
    let stored = target_objects(&catalog).await;
    catalog.inject(Step::BeforeCommit, 0, Fault::Io(io::ErrorKind::StorageFull));
    assert!(matches!(
        admin.purge_bucket(&target()).await,
        Err(PurgeError::Admin(AdminError::DiskFull(_)))
    ));
    assert_eq!(target_objects(&catalog).await, stored);
    assert_other_intact(&catalog, session).await;
    catalog.inject(Step::AfterCommit, 0, Fault::Io(io::ErrorKind::Other));
    let outcome = admin.purge_bucket(&target()).await.unwrap();
    assert_eq!(outcome.objects, stored);
    assert_eq!(catalog.pending_faults(), 0);
    assert_eq!(target_objects(&catalog).await, 0);
    assert_other_intact(&catalog, session).await;
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn failed_purge_cleanup_answers_pending_and_maintenance_or_restart_finishes_it() {
    let dir = TestDir::new();
    let (catalog, admin) = open(&dir.0).await;
    generations(&catalog).await;
    let session = other(&catalog).await;
    catalog.inject(Step::ReclaimUnlink, 0, Fault::Io(io::ErrorKind::Other));
    assert!(admin.purge_bucket(&target()).await.unwrap().cleanup_pending);
    assert!(file_count(&dir.0.join("blobs")) > 1);
    // Maintenance drains the journal the failed reclamation kept.
    catalog.maintain_deletion_journal().await.unwrap();
    assert_eq!(file_count(&dir.0.join("blobs")), 1);
    assert_eq!(counts(&catalog, TARGET_ONLY_ROWS).await[2].1, 0);
    // A staging file of a deleted session is named by no journal, so the next open removes it.
    generations(&catalog).await;
    start_session(&catalog, TARGET).await;
    // Only unix syncs the staging directory, so elsewhere the failure is the blob unlink.
    #[cfg(unix)]
    let step = Step::SessionCleanupSync;
    #[cfg(not(unix))]
    let step = Step::ReclaimUnlink;
    catalog.inject(step, 0, Fault::Io(io::ErrorKind::Other));
    assert!(admin.purge_bucket(&target()).await.unwrap().cleanup_pending);
    catalog.close().await.unwrap();
    let (reopened, _admin) = open(&dir.0).await;
    assert_eq!(file_count(&dir.0.join("staging")), 1);
    assert_eq!(file_count(&dir.0.join("blobs")), 1);
    assert_other_intact(&reopened, session).await;
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn purge_crash_and_power_loss_snapshots_recover_the_bucket_whole_or_not_at_all() {
    // The directory syncs exist only on unix, where the fault seam models them.
    #[cfg(unix)]
    let steps: &[Step] = &[
        Step::BeforeCommit,
        Step::AfterCommit,
        Step::SessionCleanupSync,
        Step::SessionCleanupSynced,
        Step::ReclaimUnlink,
        Step::ReclaimSync,
        Step::ReclaimForget,
    ];
    #[cfg(not(unix))]
    let steps: &[Step] = &[
        Step::BeforeCommit,
        Step::AfterCommit,
        Step::ReclaimUnlink,
        Step::ReclaimForget,
    ];
    for &step in steps {
        for &crash in Crash::ALL {
            let dir = TestDir::new();
            let (catalog, admin) = open(&dir.0).await;
            generations(&catalog).await;
            start_session(&catalog, TARGET).await;
            let session = other(&catalog).await;
            let before = counts(&catalog, TARGET_ROWS).await;
            let snapshot = TestDir::new();
            catalog.inject(step, 0, crash.fault(snapshot.0.clone()));
            admin.purge_bucket(&target()).await.unwrap();
            assert_eq!(catalog.pending_faults(), 0, "{step:?} {crash:?}");
            catalog.close().await.unwrap();
            let (recovered, _admin) = open(&snapshot.0).await;
            let kept = step == Step::BeforeCommit;
            let expected = before
                .iter()
                .map(|&(table, count)| (table, if kept { count } else { 0 }))
                .collect::<Vec<_>>();
            let remaining = counts(&recovered, TARGET_ROWS).await;
            assert_eq!(remaining, expected, "{step:?} {crash:?}");
            assert_other_intact(&recovered, session).await;
            let staged = file_count(&snapshot.0.join("staging"));
            assert_eq!(staged, 1 + usize::from(kept), "{step:?} {crash:?}");
            if !kept {
                let blobs = file_count(&snapshot.0.join("blobs"));
                assert_eq!(blobs, 1, "{step:?} {crash:?}");
            }
            recovered.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn purge_empties_a_ram_catalog_at_its_page_budget() {
    let (catalog, admin) = open_memory(1024 * 1024).await;
    catalog
        .insert_bucket(bucket(TARGET, "project"))
        .await
        .unwrap();
    for index in 0..300 {
        put(&catalog, TARGET, &format!("object-{index:04}"), b"bytes").await;
    }
    catalog
        .run::<_, CatalogError>(|connection| {
            crate::storage::catalog::test_fixtures::limit_pages(connection, 0)?;
            Ok(())
        })
        .await
        .unwrap();
    // The catalog takes no new row now: journaling every blob before deleting the objects fails.
    let journal_first = catalog
        .run::<_, CatalogError>(|connection| {
            let statement = "INSERT INTO pending_blob_deletions SELECT blob_id FROM objects";
            sql::exec(connection, statement, [])?;
            Ok(())
        })
        .await;
    assert!(journal_first.is_err());
    assert_eq!(admin.purge_bucket(&target()).await.unwrap().objects, 300);
    assert_eq!(ram_blobs(&catalog), 0);
    catalog.close().await.unwrap();
}
