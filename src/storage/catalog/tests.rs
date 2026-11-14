use super::*;
use crate::storage::test_fixtures::PERMISSIVE;

#[tokio::test]
async fn unknown_catalog_classes_fail_bucket_and_object_read_paths() {
    use crate::object::{ObjectError, UploadChunk};
    use crate::storage::test_fixtures::{bucket, live, permissive_upload};
    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    let (sender, body) = tokio::sync::mpsc::channel(1);
    sender.try_send(Ok(UploadChunk::End)).unwrap();
    catalog
        .put_object_prechecked(
            permissive_upload("bucket", "object", 1),
            body,
            catalog.admit_object_upload().unwrap(),
        )
        .await
        .unwrap();
    catalog
        .run(|connection| {
            sql::exec(
                connection,
                "UPDATE buckets SET storage_class='bogus' WHERE name='bucket'",
                [],
            )?;
            Ok::<_, CatalogError>(())
        })
        .await
        .unwrap();
    let error = catalog.get_bucket("bucket", PERMISSIVE).await.unwrap_err();
    assert!(matches!(error, CatalogError::Corrupt(_)));
    assert_eq!(
        crate::json::JsonError::from(error).message,
        "Bucket catalog is unavailable"
    );
    let error = catalog
        .run(|connection| objects::authorized_bucket(connection, "bucket", PERMISSIVE))
        .await
        .unwrap_err();
    assert!(matches!(error, ObjectError::Corrupt(_)));
    catalog
        .run(|connection| {
            sql::exec(
                connection,
                "UPDATE buckets SET storage_class='STANDARD' WHERE name='bucket'",
                [],
            )?;
            sql::exec(
                connection,
                "UPDATE objects SET storage_class='bogus' WHERE bucket='bucket'",
                [],
            )?;
            Ok::<_, CatalogError>(())
        })
        .await
        .unwrap();
    let error = catalog
        .get_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .unwrap_err();
    assert!(matches!(error, ObjectError::Corrupt(_)));
    assert_eq!(
        crate::json::JsonError::from(error).message,
        "Object storage is unavailable: stored data is corrupt; restore a valid data directory"
    );
    let error = catalog
        .open_object(live("bucket", "object"), Default::default(), PERMISSIVE)
        .await
        .err()
        .unwrap();
    assert!(matches!(error, ObjectError::Corrupt(_)));
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn publication_keeps_unset_holds_and_default_object_acl_json() {
    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(crate::storage::test_fixtures::bucket("bucket", "project"))
        .await
        .unwrap();
    let entries = vec![crate::acl::AclEntry {
        entity: "allUsers".into(),
        role: crate::acl::AclRole::Reader,
    }];
    catalog
        .patch_bucket(
            "bucket",
            BucketMetadataUpdate {
                default_object_acl: Some(entries),
                ..Default::default()
            },
            Default::default(),
            PERMISSIVE,
        )
        .await
        .unwrap();
    let (sender, body) = tokio::sync::mpsc::channel(1);
    sender
        .try_send(Ok(crate::object::UploadChunk::End))
        .unwrap();
    let record = catalog
        .put_object_prechecked(
            crate::storage::test_fixtures::permissive_upload("bucket", "object", 1),
            body,
            catalog.admit_object_upload().unwrap(),
        )
        .await
        .unwrap();
    let resource =
        crate::json::object_resource(&record, "http://localhost", crate::json::Projection::NoAcl)
            .unwrap();
    assert!(resource.get("eventBasedHold").is_none());
    assert!(resource.get("temporaryHold").is_none());
    let stored = catalog.run(|connection| {
        let default: String = sql::scalar(connection, "SELECT entries FROM acl_bucket_entries WHERE bucket='bucket' AND scope='defaultObject'", [])?;
        let object: String = sql::scalar(connection, "SELECT entries FROM acl_object_entries WHERE bucket='bucket' AND name='object'", [])?;
        Ok::<_, CatalogError>((default, object))
    }).await.unwrap();
    assert_eq!(stored.0, r#"[{"entity":"allUsers","role":"READER"}]"#);
    assert_eq!(
        stored.1,
        r#"[{"entity":"project-owners-0","role":"OWNER"},{"entity":"allUsers","role":"READER"}]"#
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn acl_and_storage_class_keep_catalog_text() {
    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    for (index, class) in [
        "STANDARD",
        "NEARLINE",
        "COLDLINE",
        "ARCHIVE",
        "MULTI_REGIONAL",
        "DURABLE_REDUCED_AVAILABILITY",
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("bucket-{index}");
        let mut record = crate::storage::test_fixtures::bucket(&name, "project");
        record.storage_class = class.parse().unwrap();
        catalog.insert_bucket(record).await.unwrap();
        for role in ["READER", "WRITER", "OWNER"] {
            catalog
                .change_acl(
                    crate::acl::AclTarget::Bucket(name.clone()),
                    acl::AclChange::Upsert {
                        entity: "allUsers".into(),
                        role: Some(role.parse().unwrap()),
                    },
                    PERMISSIVE,
                )
                .await
                .unwrap();
            let selected = name.clone();
            let stored = catalog
                .run(move |connection| {
                    let class: String = sql::scalar(
                        connection,
                        "SELECT storage_class FROM buckets WHERE name=?1",
                        [&selected],
                    )?;
                    let entries: String = sql::scalar(
                        connection,
                        "SELECT entries FROM acl_bucket_entries WHERE bucket=?1 AND scope='bucket'",
                        [&selected],
                    )?;
                    Ok::<_, CatalogError>((class, entries))
                })
                .await
                .unwrap();
            assert_eq!(stored.0, class, "{class} {role}");
            let entries: serde_json::Value = serde_json::from_str(&stored.1).unwrap();
            assert_eq!(
                entries
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["entity"] == "allUsers")
                    .unwrap(),
                &serde_json::json!({"entity":"allUsers","role":role}),
                "{class} {role}"
            );
        }
    }
    catalog.close().await.unwrap();
}

/// Shutdown must not look like overload: only a full catalog asks the client to retry.
#[tokio::test]
async fn a_closed_catalog_reports_closed_and_a_full_one_reports_its_limit() {
    use crate::object::ObjectError;

    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog
            .run::<(), ObjectError>(|_| Err(ObjectError::ObjectNotFound))
            .await
            .unwrap_err(),
        ObjectError::ObjectNotFound
    );
    let record =
        crate::json::parse_insert(br#"{"name":"admission"}"#, "project", UNIX_EPOCH).unwrap();
    catalog.close().await.unwrap();
    let closed = catalog.insert_bucket(record).await.unwrap_err();
    assert!(
        matches!(&closed, CatalogError::Unavailable(cause) if *cause == "catalog is closed"),
        "{closed}"
    );
    let closed_object = catalog
        .run::<(), ObjectError>(|_| Ok(()))
        .await
        .unwrap_err();
    assert_eq!(
        closed_object,
        ObjectError::from(CatalogError::Unavailable("catalog is closed".into()))
    );
}

#[tokio::test]
async fn bucket_patch_rechecks_revocation_and_bucket_recreation() {
    bucket_metadata_rechecks_revocation(BucketWrite::Patch).await;
}

#[tokio::test]
async fn bucket_put_rechecks_revocation_and_bucket_recreation() {
    bucket_metadata_rechecks_revocation(BucketWrite::Replace).await;
}

async fn bucket_metadata_rechecks_revocation(mode: BucketWrite) {
    use crate::auth::{BUCKETS_UPDATE, Binding};
    let directory = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&directory.0).await.unwrap();
    let record =
        crate::json::parse_insert(br#"{"name":"patch-policy"}"#, "project", UNIX_EPOCH).unwrap();
    catalog.insert_bucket(record.clone()).await.unwrap();
    catalog
        .set_policy(
            "patch-policy",
            Policy {
                bindings: vec![Binding {
                    role: "roles/storage.admin".into(),
                    members: vec!["allUsers".into()],
                }],
                etag: None,
            },
            PERMISSIVE,
        )
        .await
        .unwrap();
    let access = catalog
        .bucket_access("patch-policy", None, BUCKETS_UPDATE)
        .await
        .unwrap();
    catalog
        .set_policy(
            "patch-policy",
            Policy {
                bindings: vec![],
                etag: None,
            },
            PERMISSIVE,
        )
        .await
        .unwrap();
    let update = BucketMetadataUpdate {
        labels: Some(None),
        ..Default::default()
    };
    let before = catalog
        .get_bucket("patch-policy", PERMISSIVE)
        .await
        .unwrap();
    let conditions = BucketConditions {
        if_metageneration_match: Some(99),
        ..Default::default()
    };
    assert_eq!(
        catalog
            .update_bucket("patch-policy", update.clone(), conditions, &access, mode)
            .await,
        Err(CatalogError::Denied(access.denied("patch-policy")))
    );
    assert_eq!(
        catalog
            .get_bucket("patch-policy", PERMISSIVE)
            .await
            .unwrap(),
        before
    );
    let invalid = BucketMetadataUpdate {
        labels: Some(Some(std::collections::BTreeMap::from([(
            "Bad".into(),
            Some("value".into()),
        )]))),
        ..Default::default()
    };
    assert_eq!(
        catalog
            .update_bucket("patch-policy", invalid.clone(), conditions, &access, mode)
            .await,
        Err(CatalogError::Denied(access.denied("patch-policy")))
    );
    assert_eq!(
        catalog
            .get_bucket("patch-policy", PERMISSIVE)
            .await
            .unwrap(),
        before
    );
    catalog
        .delete_bucket("patch-policy", Default::default(), PERMISSIVE)
        .await
        .unwrap();
    let mut replacement = record;
    replacement.project = "other".into();
    let replacement = catalog.insert_bucket(replacement).await.unwrap();
    assert_eq!(
        catalog
            .update_bucket("patch-policy", update, Default::default(), &access, mode)
            .await,
        Err(CatalogError::Denied(access.denied("patch-policy")))
    );
    assert_eq!(
        catalog
            .get_bucket("patch-policy", PERMISSIVE)
            .await
            .unwrap(),
        Some(replacement.clone())
    );
    assert_eq!(
        catalog
            .update_bucket("patch-policy", invalid, Default::default(), &access, mode)
            .await,
        Err(CatalogError::Denied(access.denied("patch-policy")))
    );
    assert_eq!(
        catalog
            .get_bucket("patch-policy", PERMISSIVE)
            .await
            .unwrap(),
        Some(replacement)
    );
    catalog.close().await.unwrap();
}

#[test]
fn bucket_settings_migration_rolls_back_columns_and_version_after_late_failure() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection.execute_batch(BUCKET_SCHEMA).unwrap();
    connection.pragma_update(None, "user_version", 7).unwrap();
    // The first added column fits; the second exceeds this connection-local limit.
    #[expect(
        unsafe_code,
        reason = "Test-only SQLite limits avoid enabling the limits feature in the production dependency."
    )]
    // SAFETY: The connection owns the SQLite handle throughout this synchronous call.
    unsafe {
        rusqlite::ffi::sqlite3_limit(connection.handle(), rusqlite::ffi::SQLITE_LIMIT_COLUMN, 10);
    }
    let error = Migration::Statements(BUCKET_SETTINGS_SCHEMA)
        .apply(&mut connection)
        .unwrap_err();
    assert!(error.to_string().contains("too many columns"), "{error}");
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        7
    );
    let columns: Vec<String> = connection
        .prepare("PRAGMA table_info(buckets)")
        .unwrap()
        .query_map([], |row| row.get(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        !columns
            .iter()
            .any(|name| name == "versioning_enabled" || name == "default_event_based_hold")
    );
    #[expect(
        unsafe_code,
        reason = "Restore the test-only SQLite limit without adding production dependency features."
    )]
    // SAFETY: Restore the limit on the same live connection before retrying the migration.
    unsafe {
        rusqlite::ffi::sqlite3_limit(
            connection.handle(),
            rusqlite::ffi::SQLITE_LIMIT_COLUMN,
            2000,
        );
    }
    Migration::Statements(BUCKET_SETTINGS_SCHEMA)
        .apply(&mut connection)
        .unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        8
    );
}

#[test]
fn immutable_uri_preserves_native_windows_paths() {
    for (path, encoded) in [
        (r"C:\catalog.sqlite3", "C:%5Ccatalog.sqlite3"),
        (
            r"\\server\share\catalog.sqlite3",
            "%5C%5Cserver%5Cshare%5Ccatalog.sqlite3",
        ),
        (
            r"\\?\C:\catalog.sqlite3",
            "%5C%5C%3F%5CC:%5Ccatalog.sqlite3",
        ),
        (
            r"\\?\UNC\server\share\catalog.sqlite3",
            "%5C%5C%3F%5CUNC%5Cserver%5Cshare%5Ccatalog.sqlite3",
        ),
    ] {
        assert_eq!(
            immutable_catalog_uri(Path::new(path)).unwrap(),
            format!("file:{encoded}?immutable=1")
        );
    }
}

#[test]
fn immutable_uri_reads_exact_unicode_and_reserved_filename() {
    let parent = crate::test_support::TestDir::new();
    let dir = parent.0.join("日本語 # percent%");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("catalog.sqlite3");
    let writable = Connection::open(&path).unwrap();
    writable
        .execute_batch("CREATE TABLE sentinel(value); INSERT INTO sentinel VALUES (42);")
        .unwrap();
    writable.close().unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let readonly = Connection::open_with_flags(
        immutable_catalog_uri(&path.canonicalize().unwrap()).unwrap(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .unwrap();
    assert_eq!(
        readonly
            .query_row("SELECT value FROM sentinel", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        42
    );
    assert!(
        readonly
            .execute("INSERT INTO sentinel VALUES (43)", [])
            .is_err()
    );
    readonly.close().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[tokio::test]
async fn close_waits_for_cancelled_blocking_work_and_invalidates_clones() {
    use std::{future::Future, task::Poll};

    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let worker = catalog.clone();
    let (started, running) = tokio::sync::oneshot::channel();
    let (release, finish) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        worker
            .run(move |_| {
                started.send(()).unwrap();
                finish.recv().unwrap();
                Ok::<_, CatalogError>(())
            })
            .await
    });
    running.await.unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    {
        let mut close = std::pin::pin!(catalog.close());
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(close.as_mut().poll(cx)))
                .await
                .is_pending()
        );
        assert!(!catalog.is_healthy());
        assert!(matches!(
            catalog.get_bucket("missing", PERMISSIVE).await,
            Err(CatalogError::Unavailable(_))
        ));
        assert!(Catalog::open(&dir.0).await.is_err());
        release.send(()).unwrap();
        close.await.unwrap();
    }
    let reopened = Catalog::open(&dir.0).await.unwrap();
    catalog.close().await.unwrap();
    drop(catalog);
    assert!(Catalog::open(&dir.0).await.is_err());
    reopened.close().await.unwrap();
}

#[test]
fn catalog_operations_do_not_wait_for_a_saturated_blocking_pool() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let dir = crate::test_support::TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let (entered, blocked) = tokio::sync::oneshot::channel();
        // File I/O and gzip work share this pool; here it holds the only thread.
        let blocker = tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            held.recv()
        });
        blocked.await.unwrap();
        let lookup = tokio::time::timeout(
            Duration::from_secs(5),
            catalog.get_bucket("missing", PERMISSIVE),
        )
        .await;
        release.send(()).unwrap();
        blocker.await.unwrap().unwrap();
        assert!(lookup.unwrap().unwrap().is_none());
        catalog.close().await.unwrap();
    });
}

/// Opens a raw transaction that rusqlite will not roll back on unwind, then panics.
async fn panic_inside_transaction(catalog: &Catalog) -> CatalogError {
    catalog
        .run::<(), CatalogError>(|connection| {
            connection
                .execute_batch("BEGIN IMMEDIATE; CREATE TABLE panic_probe (x INTEGER);")
                .unwrap();
            panic!("injected catalog panic")
        })
        .await
        .unwrap_err()
}

async fn probe_exists(catalog: &Catalog) -> Result<bool, CatalogError> {
    catalog
        .run(|connection| {
            connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'panic_probe')",
                    [],
                    |row| row.get(0),
                )
                .map_err(unavailable)
        })
        .await
}

#[tokio::test]
async fn panicked_operation_rolls_back_and_keeps_the_catalog_ready() {
    use crate::server::State;
    use std::sync::atomic::Ordering;

    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let state = State {
        api_base_url: "http://127.0.0.1:4443".into(),
        ..State::for_test(catalog.clone(), Default::default())
    };
    assert!(state.is_ready());
    assert!(matches!(
        panic_inside_transaction(&catalog).await,
        CatalogError::Unavailable(_)
    ));
    assert!(state.is_ready());
    assert!(!probe_exists(&catalog).await.unwrap());
    assert!(
        catalog
            .get_bucket("abc", PERMISSIVE)
            .await
            .unwrap()
            .is_none()
    );

    // When the rollback cannot be confirmed the catalog fails closed and
    // readiness is revoked for good, even after a later call recovers.
    catalog.0.fail_rollback.store(true, Ordering::Release);
    assert!(matches!(
        panic_inside_transaction(&catalog).await,
        CatalogError::Unavailable(_)
    ));
    assert!(!state.is_ready());
    assert!(!state.ready.load(Ordering::Acquire));
    assert!(matches!(
        catalog.get_bucket("abc", PERMISSIVE).await,
        Err(CatalogError::Unavailable(_))
    ));
    catalog.0.fail_rollback.store(false, Ordering::Release);
    assert!(!probe_exists(&catalog).await.unwrap());
    assert!(catalog.is_healthy());
    assert!(!state.is_ready());
    catalog.close().await.unwrap();
    Catalog::open(&dir.0).await.unwrap().close().await.unwrap();
    drop(state);
    drop(catalog);
}

#[test]
fn timestamps_use_integer_unix_milliseconds() {
    assert_eq!(
        unix_millis(UNIX_EPOCH + Duration::from_micros(1999)).unwrap(),
        1
    );
    assert_eq!(
        unix_millis(UNIX_EPOCH - Duration::from_micros(1)).unwrap(),
        -1
    );
    for millis in [-1001, -1, 0, 1, 1001] {
        assert_eq!(
            unix_millis(system_time_millis(millis).unwrap()).unwrap(),
            millis
        );
    }
}

#[tokio::test]
async fn sqlite_settings_are_explicit() {
    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .run(|connection| {
            for (pragma, expected) in [
                ("synchronous", 2),
                ("foreign_keys", 1),
                ("cache_size", -2048),
                ("mmap_size", 0),
                ("busy_timeout", 5000),
                ("user_version", CATALOG_SCHEMA_VERSION),
            ] {
                assert_eq!(
                    connection.pragma_query_value(None, pragma, |row| row.get::<_, i64>(0))?,
                    expected,
                    "{pragma}"
                );
            }
            assert_eq!(
                connection
                    .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))?,
                "wal"
            );
            Ok::<_, CatalogError>(())
        })
        .await
        .unwrap();
    catalog.close().await.unwrap();
}

type StoredColumns = Vec<(&'static str, rusqlite::types::Value)>;

/// Each bucket column and its value as SQLite stores it, in `bucket_columns!` order.
async fn stored_bucket_row(catalog: &Catalog, name: &'static str) -> StoredColumns {
    let values = catalog
        .run(move |connection| {
            let row = sql::row(
                connection,
                concat!("SELECT ", bucket_columns!(), " FROM buckets WHERE name=?1"),
                [name],
                |row| {
                    (0..row.as_ref().column_count())
                        .map(|index| row.get(index))
                        .collect::<rusqlite::Result<Vec<_>>>()
                },
            )?;
            Ok::<_, CatalogError>(row)
        })
        .await
        .unwrap();
    bucket_columns!()
        .split(',')
        .map(str::trim)
        .zip(values)
        .collect()
}

/// `row` with the named columns replaced, so each step lists only what it changed.
fn changed_columns<const N: usize>(
    row: &StoredColumns,
    changes: [(&str, rusqlite::types::Value); N],
) -> StoredColumns {
    let mut row = row.clone();
    for (name, value) in changes {
        let column = row.iter_mut().find(|(column, _)| *column == name).unwrap();
        column.1 = value;
    }
    row
}

fn stored_text(text: &str) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(text.into())
}

async fn patch_stored_bucket(catalog: &Catalog, name: &str, body: &[u8]) {
    let update =
        crate::json::parse_bucket_patch(body, crate::config::CompatProfile::Native).unwrap();
    catalog
        .patch_bucket(name, update, Default::default(), PERMISSIVE)
        .await
        .unwrap();
}

#[tokio::test]
async fn stored_bucket_row_columns_keep_their_text() {
    let dir = crate::test_support::TestDir::new();
    // A whole number of 100 ns, the finest time Windows keeps, so every platform stores it as is.
    let created = std::time::UNIX_EPOCH + std::time::Duration::new(1_700_000_000, 123_456_700);
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(created));
    let catalog = Catalog::open_with_clock(&dir.0, clock.clone())
        .await
        .unwrap();
    let record = crate::json::parse_insert(
        br#"{"name":"golden-bucket","location":"EU","storageClass":"STANDARD",
            "labels":{"team":"storage","env":"test"},
            "cors":[{"origin":["https://example.test"],"method":["GET","PUT"],
                "responseHeader":["Content-Type"],"maxAgeSeconds":3600}],
            "lifecycle":{"rule":[
                {"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},
                 "condition":{"age":30,"createdBefore":"2024-01-02","customTimeBefore":"2024-02-03",
                    "daysSinceCustomTime":7,"daysSinceNoncurrentTime":8,"isLive":true,
                    "matchesPrefix":["logs/"],"matchesStorageClass":["STANDARD"],
                    "matchesSuffix":[".log"],"noncurrentTimeBefore":"2024-03-04",
                    "numNewerVersions":2,"sizeAboveBytes":"1024","sizeBelowBytes":"1048576"}},
                {"action":{"type":"Delete"},"condition":{"age":365}}]},
            "retentionPolicy":{"retentionPeriod":"3600"},
            "softDeletePolicy":{"retentionDurationSeconds":"604800"},
            "versioning":{"enabled":true},
            "defaultEventBasedHold":true,
            "iamConfiguration":{"publicAccessPrevention":"enforced",
                "uniformBucketLevelAccess":{"enabled":true}}}"#,
        "project",
        created,
    )
    .unwrap();
    catalog.insert_bucket(record).await.unwrap();
    use rusqlite::types::Value::{Integer, Null};
    let inserted = vec![
        ("name", stored_text("golden-bucket")),
        ("project", stored_text("project")),
        ("location", stored_text("EU")),
        ("location_type", stored_text("multi-region")),
        ("storage_class", stored_text("STANDARD")),
        ("labels", stored_text(r#"{"env":"test","team":"storage"}"#)),
        ("metageneration", Integer(1)),
        ("time_created", Integer(1_700_000_000_123_456)),
        ("updated", Integer(1_700_000_000_123_456)),
        ("versioning_enabled", Integer(1)),
        ("default_event_based_hold", Integer(1)),
        (
            "cors",
            stored_text(
                r#"[{"origin":["https://example.test"],"method":["GET","PUT"],"responseHeader":["Content-Type"],"maxAgeSeconds":3600}]"#,
            ),
        ),
        ("public_access_prevention_enforced", Integer(1)),
        (
            "uniform_bucket_level_access_locked_time",
            Integer(1_707_776_000_123_456),
        ),
        (
            "lifecycle",
            stored_text(
                r#"[{"action":{"type":"SetStorageClass","storageClass":"NEARLINE"},"condition":{"age":30,"createdBefore":"2024-01-02","customTimeBefore":"2024-02-03","daysSinceCustomTime":7,"daysSinceNoncurrentTime":8,"isLive":true,"matchesPrefix":["logs/"],"matchesStorageClass":["STANDARD"],"matchesSuffix":[".log"],"noncurrentTimeBefore":"2024-03-04","numNewerVersions":2,"sizeAboveBytes":"1024","sizeBelowBytes":"1048576"}},{"action":{"type":"Delete"},"condition":{"age":365}}]"#,
            ),
        ),
        (
            "retention_policy",
            stored_text(
                r#"{"retention_period":3600,"effective_time":{"secs_since_epoch":1700000000,"nanos_since_epoch":123456700},"is_locked":false}"#,
            ),
        ),
        (
            "soft_delete_policy",
            stored_text(
                r#"{"retention_duration_seconds":604800,"effective_time":{"secs_since_epoch":1700000000,"nanos_since_epoch":123456700}}"#,
            ),
        ),
        ("object_retention_enabled", Integer(0)),
        ("autoclass", Null),
        ("time_created_nanos", Integer(0)),
        ("updated_nanos", Integer(0)),
        ("generation", Integer(1_700_000_000_123_456_700)),
        ("versioning_configured", Integer(1)),
    ];
    assert_eq!(stored_bucket_row(&catalog, "golden-bucket").await, inserted);
    clock.advance(std::time::Duration::new(60, 1_000));
    patch_stored_bucket(&catalog, "golden-bucket", br#"{"labels":{"env":"prod"}}"#).await;
    let labeled = changed_columns(
        &inserted,
        [
            ("labels", stored_text(r#"{"env":"prod","team":"storage"}"#)),
            ("metageneration", Integer(2)),
            ("updated", Integer(1_700_000_060_123_457)),
        ],
    );
    assert_eq!(stored_bucket_row(&catalog, "golden-bucket").await, labeled);
    // Autoclass refuses lifecycle rules that change the storage class, so it gets its own bucket.
    let record = crate::json::parse_insert(
        br#"{"name":"autoclass-bucket","autoclass":{"enabled":true,"terminalStorageClass":"ARCHIVE"}}"#,
        "project",
        created,
    )
    .unwrap();
    catalog.insert_bucket(record).await.unwrap();
    let enabled = vec![
        ("name", stored_text("autoclass-bucket")),
        ("project", stored_text("project")),
        ("location", stored_text("US")),
        ("location_type", stored_text("multi-region")),
        ("storage_class", stored_text("STANDARD")),
        ("labels", stored_text("{}")),
        ("metageneration", Integer(1)),
        ("time_created", Integer(1_700_000_000_123_456)),
        ("updated", Integer(1_700_000_000_123_456)),
        ("versioning_enabled", Integer(0)),
        ("default_event_based_hold", Integer(0)),
        ("cors", stored_text("[]")),
        ("public_access_prevention_enforced", Integer(0)),
        ("uniform_bucket_level_access_locked_time", Null),
        ("lifecycle", stored_text("[]")),
        ("retention_policy", Null),
        ("soft_delete_policy", Null),
        ("object_retention_enabled", Integer(0)),
        (
            "autoclass",
            stored_text(
                r#"{"enabled":true,"toggle_time":{"secs_since_epoch":1700000000,"nanos_since_epoch":123456000},"terminal_storage_class":"ARCHIVE","terminal_storage_class_update_time":{"secs_since_epoch":1700000000,"nanos_since_epoch":123456000}}"#,
            ),
        ),
        ("time_created_nanos", Integer(0)),
        ("updated_nanos", Integer(0)),
        ("generation", Integer(1_700_000_000_123_456_701)),
        ("versioning_configured", Integer(0)),
    ];
    assert_eq!(
        stored_bucket_row(&catalog, "autoclass-bucket").await,
        enabled
    );
    clock.advance(std::time::Duration::new(60, 1_000));
    patch_stored_bucket(
        &catalog,
        "autoclass-bucket",
        br#"{"autoclass":{"enabled":false}}"#,
    )
    .await;
    let disabled = changed_columns(
        &enabled,
        [
            ("metageneration", Integer(2)),
            ("updated", Integer(1_700_000_120_123_458)),
            (
                "autoclass",
                stored_text(
                    r#"{"enabled":false,"toggle_time":{"secs_since_epoch":1700000120,"nanos_since_epoch":123458000},"terminal_storage_class":null,"terminal_storage_class_update_time":null}"#,
                ),
            ),
        ],
    );
    assert_eq!(
        stored_bucket_row(&catalog, "autoclass-bucket").await,
        disabled
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn legacy_lifecycle_text_survives_unrelated_updates() {
    // Releases through 0.17.1 stored lifecycle rules in input order, not the canonical order.
    const LEGACY: &str = r#"[{"condition":{"matchesPrefix":["logs/"],"age":30},"action":{"storageClass":"NEARLINE","type":"SetStorageClass"}}]"#;
    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(crate::storage::test_fixtures::bucket(
            "legacy-bucket",
            "project",
        ))
        .await
        .unwrap();
    catalog
        .run(|connection| {
            sql::exec(
                connection,
                "UPDATE buckets SET lifecycle=?1 WHERE name='legacy-bucket'",
                [LEGACY],
            )?;
            Ok::<_, CatalogError>(())
        })
        .await
        .unwrap();
    patch_stored_bucket(&catalog, "legacy-bucket", br#"{"labels":{"env":"prod"}}"#).await;
    let lifecycle = catalog
        .run(|connection| {
            sql::scalar::<String>(
                connection,
                "SELECT lifecycle FROM buckets WHERE name='legacy-bucket'",
                [],
            )
            .map_err(CatalogError::from)
        })
        .await
        .unwrap();
    assert_eq!(lifecycle, LEGACY);
    catalog.close().await.unwrap();
}

/// SQLite draws incarnations as lowercase hex, and the typed value reads back the same text.
#[tokio::test]
async fn bucket_incarnations_read_as_the_text_sqlite_draws() {
    let dir = crate::test_support::TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(crate::storage::test_fixtures::bucket("bucket", "project"))
        .await
        .unwrap();
    let (typed, text) = catalog
        .run(|connection| {
            let typed = bucket_incarnation(connection, "bucket")?;
            let text: String = sql::scalar(
                connection,
                "SELECT lower(hex(unhex(incarnation))) FROM bucket_incarnations WHERE bucket='bucket'",
                [],
            )?;
            Ok::<_, CatalogError>((typed, text))
        })
        .await
        .unwrap();
    assert_eq!(typed.to_string(), text);
    catalog.close().await.unwrap();
}
