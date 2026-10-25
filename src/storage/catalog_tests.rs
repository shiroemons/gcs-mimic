use super::catalog::test_support as common;
use crate as gcs_mimic;
use gcs_mimic::{
    bucket::{BucketAccess, BucketMetadataUpdate, BucketRecord, CatalogError},
    storage::Catalog,
};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, UNIX_EPOCH},
};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(std::env::temp_dir().join(format!(
            "gcs-mimic-catalog-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if self.0.exists() {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
}

fn bucket(name: &str, project: &str) -> BucketRecord {
    BucketRecord {
        name: name.into(),
        project: project.into(),
        location: "US".into(),
        location_type: "multi-region".into(),
        storage_class: "STANDARD".into(),
        labels: BTreeMap::from([("team".into(), "storage".into())]),
        versioning_enabled: false,
        default_event_based_hold: false,
        public_access_prevention_enforced: false,
        cors: Vec::new(),
        metageneration: 3,
        time_created: UNIX_EPOCH + Duration::from_millis(42_123),
        updated: UNIX_EPOCH + Duration::from_millis(43_987),
    }
}

#[tokio::test]
async fn timestamp_version_fourteen_reopen_preserves_timestamps() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let mut original = bucket("timestamp-reopen", "project");
    original.time_created = UNIX_EPOCH + Duration::from_micros(42_123_456);
    original.updated = UNIX_EPOCH + Duration::from_micros(43_987_654);
    let original = catalog.insert_bucket(original).await.unwrap();
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        15
    );
    drop(connection);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog.get_bucket("timestamp-reopen").await.unwrap(),
        Some(original)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn pap_migrates_version_twelve_and_persists_configured_state() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let original = catalog
        .insert_bucket(bucket("pap-migration", "project"))
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    common::downgrade_timestamp_precision(&connection);
    connection.execute_batch("ALTER TABLE buckets DROP COLUMN public_access_prevention_enforced; PRAGMA user_version = 12;").unwrap();
    drop(connection);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog.get_bucket("pap-migration").await.unwrap(),
        Some(original.clone())
    );
    let enforced = catalog
        .patch_bucket(
            "pap-migration",
            BucketMetadataUpdate {
                public_access_prevention_enforced: Some(true),
                ..Default::default()
            },
            Some(original.metageneration),
            None,
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        15
    );
    assert!(
        connection
            .execute(
                "UPDATE buckets SET public_access_prevention_enforced = 2",
                []
            )
            .is_err()
    );
    drop(connection);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog.get_bucket("pap-migration").await.unwrap(),
        Some(enforced)
    );
}

#[tokio::test]
async fn bucket_cors_migrates_version_eleven_and_rejects_invalid_library_updates() {
    use gcs_mimic::bucket::CorsRule;
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let original = catalog
        .insert_bucket(bucket("cors-migration", "project"))
        .await
        .unwrap();
    catalog.close().await.unwrap();
    let connection = Connection::open(dir.0.join("catalog.sqlite3")).unwrap();
    common::downgrade_timestamp_precision(&connection);
    connection
        .execute_batch("ALTER TABLE buckets DROP COLUMN public_access_prevention_enforced; ALTER TABLE buckets DROP COLUMN cors; PRAGMA user_version = 11;")
        .unwrap();
    drop(connection);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog.get_bucket("cors-migration").await.unwrap(),
        Some(original.clone())
    );
    let rule = CorsRule {
        origin: vec!["https://app.example".into()],
        method: vec!["GET".into()],
        response_header: vec!["ETag".into()],
        max_age_seconds: Some(0),
    };
    let stored = catalog
        .patch_bucket(
            "cors-migration",
            BucketMetadataUpdate {
                cors: Some(vec![rule.clone()]),
                ..Default::default()
            },
            Some(original.metageneration),
            None,
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    let mut invalid = rule.clone();
    invalid.response_header = vec!["invalid header".into()];
    let result = catalog
        .replace_bucket(
            "cors-migration",
            BucketMetadataUpdate {
                cors: Some(vec![invalid.clone()]),
                ..Default::default()
            },
            None,
            None,
            &BucketAccess::Permissive,
        )
        .await;
    assert!(matches!(result, Err(CatalogError::InvalidMetadata(_))));
    assert_eq!(
        catalog.get_bucket("cors-migration").await.unwrap(),
        Some(stored.clone())
    );
    let mut invalid_bucket = bucket("invalid-cors", "project");
    invalid_bucket.cors = vec![invalid];
    assert!(matches!(
        catalog.insert_bucket(invalid_bucket).await,
        Err(CatalogError::InvalidMetadata(_))
    ));
    assert!(catalog.get_bucket("invalid-cors").await.unwrap().is_none());
    for rules in [
        vec![CorsRule {
            origin: vec!["a".repeat(65_536)],
            ..Default::default()
        }],
        vec![CorsRule::default(); 32_768],
        vec![CorsRule {
            origin: vec!["\\".repeat(40_000)],
            ..Default::default()
        }],
    ] {
        let mut oversized = bucket("oversized-cors", "project");
        oversized.cors = rules;
        assert!(matches!(
            catalog.insert_bucket(oversized).await,
            Err(CatalogError::InvalidMetadata(_))
        ));
        assert!(
            catalog
                .get_bucket("oversized-cors")
                .await
                .unwrap()
                .is_none()
        );
    }
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog.get_bucket("cors-migration").await.unwrap(),
        Some(stored)
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn bucket_put_uses_clock_and_rolls_back_invalid_replacement() {
    struct FixedClock;
    impl gcs_mimic::clock::Clock for FixedClock {
        fn now(&self) -> std::time::SystemTime {
            UNIX_EPOCH + Duration::from_micros(123_456_789)
        }
    }
    let dir = TestDir::new();
    let catalog = Catalog::open_with_clock(&dir.0, Arc::new(FixedClock))
        .await
        .unwrap();
    let mut original = bucket("put-clock", "project");
    original.storage_class = "ARCHIVE".into();
    original.versioning_enabled = true;
    original.default_event_based_hold = true;
    let original = catalog.insert_bucket(original).await.unwrap();
    let update = BucketMetadataUpdate {
        labels: Some(Some(BTreeMap::from([("new".into(), Some("value".into()))]))),
        storage_class: Some("NEARLINE".into()),
        ..Default::default()
    };
    let record = catalog
        .replace_bucket(
            "put-clock",
            update.clone(),
            Some(3),
            Some(2),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(
        record.labels,
        BTreeMap::from([("new".into(), "value".into())])
    );
    assert_eq!(record.storage_class, "NEARLINE");
    assert!(!record.versioning_enabled);
    assert!(!record.default_event_based_hold);
    assert_eq!(
        record.updated,
        UNIX_EPOCH + Duration::from_micros(123_456_789)
    );
    assert_eq!(record.time_created, original.time_created);
    assert_eq!(record.project, original.project);
    assert_eq!(record.location, original.location);
    assert_eq!(record.metageneration, 4);
    let labels = (0..64)
        .map(|i| (format!("key{i}"), Some("v".into())))
        .collect();
    let full = catalog
        .replace_bucket(
            "put-clock",
            BucketMetadataUpdate {
                labels: Some(Some(labels)),
                ..update.clone()
            },
            None,
            None,
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(full.labels.len(), 64);
    let invalid = [
        BucketMetadataUpdate {
            labels: Some(Some(
                (0..65)
                    .map(|i| (format!("key{i}"), Some("v".into())))
                    .collect(),
            )),
            ..update.clone()
        },
        BucketMetadataUpdate {
            storage_class: Some("UNKNOWN".into()),
            ..update.clone()
        },
        BucketMetadataUpdate {
            labels: Some(Some(BTreeMap::from([("Bad".into(), Some("v".into()))]))),
            ..update.clone()
        },
        BucketMetadataUpdate {
            labels: Some(Some(BTreeMap::from([("key".into(), None)]))),
            ..update.clone()
        },
    ];
    for invalid in invalid {
        assert_eq!(
            catalog
                .replace_bucket(
                    "put-clock",
                    invalid.clone(),
                    Some(3),
                    None,
                    &BucketAccess::Permissive
                )
                .await,
            Err(CatalogError::PreconditionFailed)
        );
        assert!(matches!(
            catalog
                .replace_bucket("put-clock", invalid, None, None, &BucketAccess::Permissive)
                .await,
            Err(CatalogError::InvalidMetadata(_))
        ));
        assert_eq!(
            catalog.get_bucket("put-clock").await.unwrap(),
            Some(full.clone())
        );
    }
    assert!(matches!(
        catalog
            .replace_bucket(
                "put-clock",
                BucketMetadataUpdate {
                    storage_class: Some("RAPID".into()),
                    ..Default::default()
                },
                None,
                None,
                &BucketAccess::Permissive
            )
            .await,
        Err(CatalogError::UnsupportedMetadata(_))
    ));
    assert_eq!(
        catalog.get_bucket("put-clock").await.unwrap(),
        Some(full.clone())
    );
    let next = catalog
        .replace_bucket(
            "put-clock",
            Default::default(),
            Some(5),
            None,
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert!(next.labels.is_empty());
    assert_eq!(next.storage_class, "STANDARD");
    assert_eq!(next.updated, record.updated);
    assert_eq!(next.metageneration, 6);
    let mut exhausted = bucket("put-exhausted", "project");
    exhausted.metageneration = i64::MAX;
    let exhausted = catalog.insert_bucket(exhausted).await.unwrap();
    assert!(matches!(
        catalog
            .replace_bucket(
                "put-exhausted",
                update,
                None,
                None,
                &BucketAccess::Permissive
            )
            .await,
        Err(CatalogError::Unavailable(_))
    ));
    assert_eq!(
        catalog.get_bucket("put-exhausted").await.unwrap(),
        Some(exhausted)
    );
}

#[tokio::test]
async fn bucket_patch_uses_clock_and_rolls_back_invalid_merge() {
    struct FixedClock;
    impl gcs_mimic::clock::Clock for FixedClock {
        fn now(&self) -> std::time::SystemTime {
            UNIX_EPOCH + Duration::from_micros(123_456_789)
        }
    }
    let dir = TestDir::new();
    let catalog = Catalog::open_with_clock(&dir.0, Arc::new(FixedClock))
        .await
        .unwrap();
    let original = catalog
        .insert_bucket(bucket("patch-clock", "project"))
        .await
        .unwrap();
    let update = BucketMetadataUpdate {
        labels: Some(Some(BTreeMap::from([("new".into(), Some("value".into()))]))),
        storage_class: Some("ARCHIVE".into()),
        versioning_enabled: Some(true),
        default_event_based_hold: Some(true),
        ..Default::default()
    };
    let record = catalog
        .patch_bucket(
            "patch-clock",
            update.clone(),
            Some(3),
            Some(2),
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(
        record.updated,
        UNIX_EPOCH + Duration::from_micros(123_456_789)
    );
    assert_eq!(record.time_created, original.time_created);
    assert_eq!(record.metageneration, 4);
    let next = catalog
        .patch_bucket(
            "patch-clock",
            Default::default(),
            Some(4),
            None,
            &BucketAccess::Permissive,
        )
        .await
        .unwrap();
    assert_eq!(next.updated, record.updated);
    assert_eq!(next.metageneration, 5);
    let too_many = BucketMetadataUpdate {
        labels: Some(Some(
            (0..64)
                .map(|i| (format!("key{i}"), Some("v".into())))
                .collect(),
        )),
        ..update.clone()
    };
    assert!(matches!(
        catalog
            .patch_bucket(
                "patch-clock",
                too_many.clone(),
                None,
                None,
                &BucketAccess::Permissive
            )
            .await,
        Err(CatalogError::InvalidMetadata(_))
    ));
    assert_eq!(
        catalog
            .patch_bucket(
                "patch-clock",
                too_many,
                Some(3),
                None,
                &BucketAccess::Permissive
            )
            .await,
        Err(CatalogError::PreconditionFailed)
    );
    for update in [
        BucketMetadataUpdate {
            storage_class: Some("UNKNOWN".into()),
            ..update.clone()
        },
        BucketMetadataUpdate {
            labels: Some(Some(BTreeMap::from([("Bad".into(), None)]))),
            ..update.clone()
        },
    ] {
        assert!(matches!(
            catalog
                .patch_bucket("patch-clock", update, None, None, &BucketAccess::Permissive)
                .await,
            Err(CatalogError::InvalidMetadata(_))
        ));
    }
    assert_eq!(catalog.get_bucket("patch-clock").await.unwrap(), Some(next));
    let mut exhausted = bucket("exhausted", "project");
    exhausted.metageneration = i64::MAX;
    let exhausted = catalog.insert_bucket(exhausted).await.unwrap();
    assert!(matches!(
        catalog
            .patch_bucket("exhausted", update, None, None, &BucketAccess::Permissive)
            .await,
        Err(CatalogError::Unavailable(_))
    ));
    assert_eq!(
        catalog.get_bucket("exhausted").await.unwrap(),
        Some(exhausted)
    );
}

#[tokio::test]
async fn create_get_delete_and_restart_preserve_every_field() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(catalog.get_bucket("missing").await.unwrap(), None);
    let mut expected = bucket("bucket", "project");
    expected.versioning_enabled = true;
    expected.default_event_based_hold = true;
    let created = catalog.insert_bucket(expected.clone()).await.unwrap();
    expected.time_created = UNIX_EPOCH + Duration::from_millis(42_123);
    expected.updated = UNIX_EPOCH + Duration::from_millis(43_987);
    assert_eq!(created, expected);
    assert_eq!(
        catalog.get_bucket("bucket").await.unwrap(),
        Some(expected.clone())
    );
    assert_eq!(
        catalog
            .insert_bucket(bucket("bucket", "other-project"))
            .await,
        Err(CatalogError::AlreadyExists)
    );
    drop(catalog);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(catalog.get_bucket("bucket").await.unwrap(), Some(expected));
    catalog.delete_bucket("bucket", None, None).await.unwrap();
    assert_eq!(catalog.get_bucket("bucket").await.unwrap(), None);
    assert_eq!(
        catalog.delete_bucket("bucket", Some(9), None).await,
        Err(CatalogError::NotFound)
    );
    drop(catalog);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(catalog.get_bucket("bucket").await.unwrap(), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_duplicate_creates_have_exactly_one_winner() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let catalog = catalog.clone();
        tasks.spawn(async move { catalog.insert_bucket(bucket("same", "project")).await });
    }
    let mut winners = 0;
    while let Some(result) = tasks.join_next().await {
        match result.unwrap() {
            Ok(_) => winners += 1,
            Err(error) => assert_eq!(error, CatalogError::AlreadyExists),
        }
    }
    assert_eq!(winners, 1);
    assert_eq!(
        catalog
            .list_buckets("project", None, None, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn indexed_listing_orders_filters_pages_and_limits() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    for name in [
        "b", "a_z", "aa", "a%z", "a_z2", "a%z2", "a", "aé", "aê", "aé2",
    ] {
        catalog
            .insert_bucket(bucket(name, "project"))
            .await
            .unwrap();
    }
    catalog
        .insert_bucket(bucket("a_other", "other-project"))
        .await
        .unwrap();
    let list = async |prefix: Option<&str>, start: Option<&str>, limit| {
        catalog
            .list_buckets("project", prefix, start, limit)
            .await
            .unwrap()
            .into_iter()
            .map(|b| b.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        list(None, None, 20).await,
        [
            "a", "a%z", "a%z2", "a_z", "a_z2", "aa", "aé", "aé2", "aê", "b"
        ]
    );
    assert_eq!(list(Some("a_"), None, 20).await, ["a_z", "a_z2"]);
    assert_eq!(list(Some("a%"), None, 20).await, ["a%z", "a%z2"]);
    assert_eq!(list(Some("aé"), None, 20).await, ["aé", "aé2"]);
    assert_eq!(list(None, None, 2).await, ["a", "a%z"]);
    assert_eq!(list(None, Some("a%z"), 2).await, ["a%z2", "a_z"]);
    assert_eq!(list(Some("a_"), Some("a_z"), 2).await, ["a_z2"]);
    assert_eq!(list(Some("a_"), Some("a"), 2).await, ["a_z", "a_z2"]);
    assert_eq!(list(Some("a_"), Some("b"), 2).await, Vec::<String>::new());
    assert_eq!(
        list(Some("a_"), Some("a_z2"), 2).await,
        Vec::<String>::new()
    );
    assert_eq!(list(None, Some("z"), 2).await, Vec::<String>::new());
    assert_eq!(list(None, None, 0).await, Vec::<String>::new());
    assert_eq!(list(Some(""), None, 1).await, ["a"]);
    assert_eq!(
        catalog
            .list_buckets("other-project", None, None, 10)
            .await
            .unwrap()
            .into_iter()
            .map(|b| b.name)
            .collect::<Vec<_>>(),
        ["a_other"]
    );
    assert!(matches!(
        catalog
            .list_buckets("project", None, None, usize::MAX)
            .await,
        Err(CatalogError::Unavailable(_))
    ));
}

#[tokio::test]
async fn bucket_list_metadata_budget_preserves_large_rows_and_cursor_boundaries() {
    use gcs_mimic::bucket::CorsRule;
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    for name in ["page-a", "page-b", "page-c", "page-d"] {
        let mut record = bucket(name, "project");
        if name == "page-b" || name == "page-c" {
            record.cors = vec![CorsRule::default(); 20_000];
        }
        catalog.insert_bucket(record).await.unwrap();
    }
    let first = catalog
        .list_buckets_page("project", Some("page-"), None, 1000)
        .await
        .unwrap();
    assert_eq!(
        first
            .items
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        ["page-a"]
    );
    assert!(first.has_more);
    for (cursor, name, more) in [
        ("page-a", "page-b", true),
        ("page-b", "page-c", true),
        ("page-c", "page-d", false),
    ] {
        let page = catalog
            .list_buckets_page("project", Some("page-"), Some(cursor), 1000)
            .await
            .unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].name, name);
        assert_eq!(page.has_more, more);
        if name != "page-d" {
            assert_eq!(page.items[0].cors, vec![CorsRule::default(); 20_000]);
        }
    }
    let last = catalog
        .list_buckets_page("project", Some("page-"), Some("page-c"), 1)
        .await
        .unwrap();
    assert_eq!(last.items[0].name, "page-d");
    assert!(!last.has_more);
    let empty = catalog
        .list_buckets_page("project", Some("page-"), Some("page-d"), 1)
        .await
        .unwrap();
    assert!(empty.items.is_empty());
    assert!(!empty.has_more);
    let zero = catalog
        .list_buckets_page("project", None, None, 0)
        .await
        .unwrap();
    assert!(zero.items.is_empty());
    assert!(!zero.has_more);
}

#[tokio::test]
async fn delete_preconditions_are_atomic_and_preserve_failed_records() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(bucket("conditional", "project"))
        .await
        .unwrap();
    for (matching, not_matching) in [(Some(2), None), (None, Some(3)), (Some(3), Some(3))] {
        assert_eq!(
            catalog
                .delete_bucket("conditional", matching, not_matching)
                .await,
            Err(CatalogError::PreconditionFailed)
        );
        assert!(catalog.get_bucket("conditional").await.unwrap().is_some());
    }
    catalog
        .delete_bucket("conditional", Some(3), Some(2))
        .await
        .unwrap();
    catalog
        .insert_bucket(bucket("conditional", "project"))
        .await
        .unwrap();
    catalog
        .delete_bucket("conditional", None, Some(4))
        .await
        .unwrap();
    catalog
        .insert_bucket(bucket("conditional", "project"))
        .await
        .unwrap();
    catalog
        .delete_bucket("conditional", Some(3), None)
        .await
        .unwrap();
}

#[tokio::test]
async fn directory_lock_lives_until_the_last_handle_drops() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let clone = catalog.clone();
    drop(catalog);
    let error = Catalog::open(&dir.0).await.err().unwrap();
    assert!(error.to_string().contains("already in use"), "{error}");
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "storage::catalog_tests::catalog_child_process",
            "--nocapture",
        ])
        .env("GCS_MIMIC_TEST_LOCK_DIR", &dir.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    drop(clone);
    Catalog::open(&dir.0).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_concurrent_use_close_and_reopen_preserves_data() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let expected = catalog
        .insert_bucket(bucket("persisted", "project"))
        .await
        .unwrap();
    catalog.close().await.unwrap();
    for _ in 0..64 {
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let catalog = catalog.clone();
            tasks.spawn(async move { catalog.get_bucket("persisted").await });
        }
        while let Some(result) = tasks.join_next().await {
            assert_eq!(result.unwrap().unwrap(), Some(expected.clone()));
        }
        catalog.close().await.unwrap();
        let reopened = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(
            reopened.get_bucket("persisted").await.unwrap(),
            Some(expected.clone())
        );
        catalog.close().await.unwrap();
        drop(catalog);
        assert!(Catalog::open(&dir.0).await.is_err());
        reopened.close().await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test]
async fn drop_releases_lock_while_a_forked_child_has_not_execed() {
    use std::{
        io::{Read, Write},
        os::unix::{net::UnixStream, process::CommandExt},
    };

    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let (mut parent, mut child) = UnixStream::pair().unwrap();
    parent
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    child
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let process = std::thread::spawn(move || {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "storage::catalog_tests::catalog_child_process"]);
        // SAFETY: Only fixed-size socket I/O runs after fork; no allocation or locks are used.
        #[allow(unsafe_code)]
        unsafe {
            command.pre_exec(move || {
                child.write_all(&[1])?;
                child.read_exact(&mut [0])?;
                Ok(())
            });
        }
        command.output().unwrap()
    });
    parent.read_exact(&mut [0]).unwrap();
    drop(catalog);
    let reopened = Catalog::open(&dir.0).await;
    parent.write_all(&[1]).unwrap();
    assert!(process.join().unwrap().status.success());
    reopened.unwrap();
}

#[tokio::test]
async fn bucket_settings_migrate_supported_catalogs() {
    for version in [4, 5, 6, 7] {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        let expected = catalog
            .insert_bucket(bucket("bucket", "project"))
            .await
            .unwrap();
        catalog.close().await.unwrap();
        let path = dir.0.join("catalog.sqlite3");
        let connection = Connection::open(&path).unwrap();
        let objects_sql: String = connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name = 'objects'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        connection.execute_batch("ALTER TABLE buckets DROP COLUMN versioning_enabled; ALTER TABLE buckets DROP COLUMN default_event_based_hold;").unwrap();
        common::downgrade_object_schema(&connection);
        if version < 7 {
            connection
                .execute_batch("DROP TABLE resumable_sessions; DROP TABLE bucket_incarnations;")
                .unwrap();
        }
        if version < 6 {
            connection
                .execute_batch("DROP TABLE bucket_policies; DROP TABLE policy_revision;")
                .unwrap();
        }
        if version < 5 {
            connection
                .execute_batch("DROP TABLE startup_seed;")
                .unwrap();
        }
        connection
            .pragma_update(None, "user_version", version)
            .unwrap();
        drop(connection);
        let catalog = Catalog::open(&dir.0).await.unwrap();
        assert_eq!(
            catalog.get_bucket("bucket").await.unwrap(),
            Some(expected),
            "version {version}"
        );
        for (index, (versioning, hold)) in
            [(false, false), (false, true), (true, false), (true, true)]
                .into_iter()
                .enumerate()
        {
            let mut record = bucket(&format!("settings-{index}"), "project");
            record.versioning_enabled = versioning;
            record.default_event_based_hold = hold;
            catalog.insert_bucket(record).await.unwrap();
        }
        catalog.close().await.unwrap();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        for (index, settings) in [(false, false), (false, true), (true, false), (true, true)]
            .into_iter()
            .enumerate()
        {
            let record = catalog
                .get_bucket(&format!("settings-{index}"))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                (record.versioning_enabled, record.default_event_based_hold),
                settings
            );
        }
        catalog.close().await.unwrap();
        let mut connection = Connection::open(path).unwrap();
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            15
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT sql FROM sqlite_schema WHERE name = 'objects'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            objects_sql
        );
        let transaction = connection.transaction().unwrap();
        transaction
            .execute(
                "UPDATE buckets SET versioning_enabled = 1 WHERE name = 'bucket'",
                [],
            )
            .unwrap();
        assert_eq!(
            Catalog::bucket_settings_in_transaction(&transaction, "bucket").unwrap(),
            (true, false)
        );
        assert_eq!(
            Catalog::bucket_settings_in_transaction(&transaction, "missing"),
            Err(CatalogError::NotFound)
        );
        assert!(
            transaction
                .execute("UPDATE buckets SET default_event_based_hold = 2", [])
                .is_err()
        );
        assert!(
            transaction
                .execute("UPDATE buckets SET versioning_enabled = NULL", [])
                .is_err()
        );
        transaction.rollback().unwrap();
    }
}

#[tokio::test]
async fn rejects_future_schema_unversioned_data_and_corruption() {
    for fixture in [
        "future",
        "unversioned",
        "corrupt",
        "missing-schema",
        "incompatible-schema",
        "v2-missing-objects",
        "v2-incompatible-objects",
        "v6-missing-policy",
        "v6-orphan-policy",
        "v6-stale-policy-revision",
        "v8-missing-settings",
        "v8-altered-settings-constraint",
        "v8-altered-settings-default",
    ] {
        let dir = TestDir::new();
        fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("catalog.sqlite3");
        match fixture {
            "future" => Connection::open(&path)
                .unwrap()
                .pragma_update(None, "user_version", 16)
                .unwrap(),
            "unversioned" => {
                Connection::open(&path)
                    .unwrap()
                    .execute("CREATE TABLE unknown (value TEXT)", [])
                    .unwrap();
            }
            "missing-schema" => Connection::open(&path)
                .unwrap()
                .pragma_update(None, "user_version", 1)
                .unwrap(),
            "incompatible-schema" => Connection::open(&path)
                .unwrap()
                .execute_batch("CREATE TABLE buckets (name TEXT); PRAGMA user_version = 1;")
                .unwrap(),
            "v2-missing-objects" | "v2-incompatible-objects" => {
                let catalog = Catalog::open(&dir.0).await.unwrap();
                catalog.close().await.unwrap();
                let connection = Connection::open(&path).unwrap();
                connection.execute("DROP TABLE objects", []).unwrap();
                if fixture == "v2-incompatible-objects" {
                    connection
                        .execute("CREATE TABLE objects (blob_id TEXT)", [])
                        .unwrap();
                }
            }
            "v6-missing-policy" | "v6-orphan-policy" | "v6-stale-policy-revision" => {
                let catalog = Catalog::open(&dir.0).await.unwrap();
                catalog
                    .insert_bucket(bucket("bucket", "project"))
                    .await
                    .unwrap();
                catalog.close().await.unwrap();
                let connection = Connection::open(&path).unwrap();
                let sql = match fixture {
                    "v6-missing-policy" => "DELETE FROM bucket_policies",
                    "v6-orphan-policy" => "DELETE FROM buckets",
                    _ => "UPDATE policy_revision SET last_revision = 0",
                };
                connection.execute(sql, []).unwrap();
            }
            "v8-missing-settings"
            | "v8-altered-settings-constraint"
            | "v8-altered-settings-default" => {
                let catalog = Catalog::open(&dir.0).await.unwrap();
                catalog.close().await.unwrap();
                Connection::open(&path)
                    .unwrap()
                    .execute_batch("ALTER TABLE buckets DROP COLUMN default_event_based_hold;")
                    .unwrap();
                let connection = Connection::open(&path).unwrap();
                if fixture == "v8-altered-settings-constraint" {
                    connection.execute_batch("ALTER TABLE buckets ADD COLUMN default_event_based_hold INTEGER NOT NULL DEFAULT 0;").unwrap();
                } else if fixture == "v8-altered-settings-default" {
                    connection.execute_batch("ALTER TABLE buckets ADD COLUMN default_event_based_hold INTEGER NOT NULL DEFAULT 1 CHECK(default_event_based_hold IN (0, 1));").unwrap();
                }
            }
            _ => fs::write(&path, b"not a SQLite database").unwrap(),
        }
        let before = fs::read(&path).unwrap();
        let journal_mode = || {
            Connection::open(&path)
                .unwrap()
                .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
                .unwrap()
        };
        let mode = (fixture != "corrupt").then(journal_mode);
        let error = Catalog::open(&dir.0).await.err().unwrap();
        if fixture == "corrupt" {
            assert!(
                matches!(error, CatalogError::Corrupt(_)),
                "{fixture}: {error}"
            );
        } else {
            assert!(
                matches!(error, CatalogError::Unavailable(_)),
                "{fixture}: {error}"
            );
        }
        if fixture == "future" {
            assert!(error.to_string().contains("schema version 16"));
        }
        if fixture == "unversioned" {
            assert!(error.to_string().contains("not empty"));
        }
        if fixture.starts_with("v6-") {
            assert!(
                error
                    .to_string()
                    .contains("invalid bucket policy catalog state")
            );
        }
        // Failed opens release ownership too.
        assert!(
            Catalog::open(&dir.0)
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("catalog")
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            before,
            "{fixture}: database bytes changed"
        );
        assert!(!path.with_extension("sqlite3-wal").exists(), "{fixture}");
        assert!(!path.with_extension("sqlite3-shm").exists(), "{fixture}");
        if let Some(mode) = mode {
            assert_eq!(journal_mode(), mode, "{fixture}");
        }
    }
}

#[tokio::test]
async fn policy_migration_visits_every_bucket_in_binary_name_order() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let names = ["000", "aaa", "aaa-0", "aaa.0", "aaa0", "aab", "zzz"];
    for (index, name) in names.iter().enumerate().rev() {
        catalog
            .insert_bucket(bucket(name, &format!("project-{index}")))
            .await
            .unwrap();
    }
    catalog.close().await.unwrap();
    let path = dir.0.join("catalog.sqlite3");
    let legacy = Connection::open(&path).unwrap();
    common::downgrade_object_schema(&legacy);
    legacy.execute_batch(
            "ALTER TABLE buckets DROP COLUMN versioning_enabled; ALTER TABLE buckets DROP COLUMN default_event_based_hold; DROP TABLE resumable_sessions; DROP TABLE bucket_incarnations; DROP TABLE bucket_policies; DROP TABLE policy_revision; PRAGMA user_version = 5;",
        )
        .unwrap();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog.close().await.unwrap();
    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        15
    );
    let policies: Vec<(String, String, i64)> = connection
        .prepare(
            "SELECT bucket, bindings, revision FROM bucket_policies ORDER BY bucket COLLATE BINARY",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(policies.len(), names.len());
    for (index, (name, bindings, revision)) in policies.iter().enumerate() {
        assert_eq!(name, names[index]);
        assert_eq!(*revision, index as i64 + 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(bindings).unwrap(),
            serde_json::json!([
                {"role":"roles/storage.legacyBucketOwner", "members":[format!("projectEditor:project-{index}"), format!("projectOwner:project-{index}")]},
                {"role":"roles/storage.legacyBucketReader", "members":[format!("projectViewer:project-{index}")]}
            ])
        );
    }
    assert_eq!(
        connection
            .query_row("SELECT last_revision FROM policy_revision", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        names.len() as i64
    );
}

#[tokio::test]
async fn failed_policy_migration_rolls_back_and_restarts_from_version_five() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let mut expected = Vec::new();
    for name in ["aaa", "zzz"] {
        expected.push(
            catalog
                .insert_bucket(bucket(name, "project"))
                .await
                .unwrap(),
        );
    }
    catalog.close().await.unwrap();
    let path = dir.0.join("catalog.sqlite3");
    let connection = Connection::open(&path).unwrap();
    common::downgrade_object_schema(&connection);
    connection
        .execute_batch(
            "ALTER TABLE buckets DROP COLUMN versioning_enabled; ALTER TABLE buckets DROP COLUMN default_event_based_hold; DROP TABLE resumable_sessions; DROP TABLE bucket_incarnations; DROP TABLE bucket_policies; DROP TABLE policy_revision; PRAGMA user_version = 5;",
        )
        .unwrap();
    // An oversized default policy fails the second insert, after the first policy and v6 marker.
    connection
        .execute(
            "UPDATE buckets SET project = ?1 WHERE name = 'zzz'",
            ["x".repeat(65536)],
        )
        .unwrap();
    drop(connection);
    let error = Catalog::open(&dir.0).await.err().unwrap();
    assert!(matches!(error, CatalogError::Unavailable(_)));
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "{error}"
    );
    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        5
    );
    assert_eq!(connection.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('bucket_policies', 'policy_revision')", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM buckets", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    connection
        .execute(
            "UPDATE buckets SET project = 'project' WHERE name = 'zzz'",
            [],
        )
        .unwrap();
    drop(connection);
    let catalog = Catalog::open(&dir.0).await.unwrap();
    for record in expected {
        assert_eq!(
            catalog.get_bucket(&record.name).await.unwrap(),
            Some(record)
        );
    }
    catalog.close().await.unwrap();
    let connection = Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        15
    );
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM bucket_policies", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[tokio::test]
async fn rejected_checkpointed_wal_catalog_creates_no_sidecars_with_uri_characters() {
    let dir = TestDir::new();
    let directory = dir.0.join("catalog ?#%é");
    let catalog = Catalog::open(&directory).await.unwrap();
    catalog.close().await.unwrap();
    let path = directory.join("catalog.sqlite3");
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 16)
        .unwrap();
    let before = fs::read(&path).unwrap();
    for _ in 0..2 {
        assert!(!path.with_extension("sqlite3-wal").exists());
        assert!(!path.with_extension("sqlite3-shm").exists());
        assert!(
            Catalog::open(&directory)
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("schema version 16")
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!path.with_extension("sqlite3-wal").exists());
        assert!(!path.with_extension("sqlite3-shm").exists());
    }
}

#[test]
fn catalog_child_process() {
    if let Some(dir) = std::env::var_os("GCS_MIMIC_TEST_REJECTED_WAL_DIR") {
        let connection = Connection::open(PathBuf::from(dir).join("catalog.sqlite3")).unwrap();
        connection
            .pragma_update(None, "wal_autocheckpoint", 0)
            .unwrap();
        let sql = match std::env::var("GCS_MIMIC_TEST_REJECTED_WAL_FIXTURE")
            .unwrap()
            .as_str()
        {
            "v2" => "PRAGMA user_version = 2;",
            "future" => "PRAGMA user_version = 16;",
            "inventory" => "DROP INDEX buckets_project_name;",
            _ => unreachable!(),
        };
        connection.execute_batch(sql).unwrap();
        // Preserve uncheckpointed committed frames with no remaining read-write connection.
        std::process::exit(0);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    if let Some(dir) = std::env::var_os("GCS_MIMIC_TEST_LOCK_DIR") {
        runtime.block_on(async {
            assert!(
                Catalog::open(dir)
                    .await
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("already in use")
            );
        });
    }
    if let Some(dir) = std::env::var_os("GCS_MIMIC_TEST_RECOVERY_DIR") {
        runtime.block_on(async {
            let catalog = Catalog::open(dir).await.unwrap();
            catalog
                .insert_bucket(bucket("committed", "project"))
                .await
                .unwrap();
            // Exit without Drop so SQLite must recover the committed WAL on the next open.
            std::process::exit(0);
        });
    }
}

#[tokio::test]
async fn rejected_catalog_with_hot_wal_preserves_database_and_wal_bytes() {
    for fixture in ["v2", "future", "inventory"] {
        let dir = TestDir::new();
        let catalog = Catalog::open(&dir.0).await.unwrap();
        catalog.close().await.unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "storage::catalog_tests::catalog_child_process",
                "--nocapture",
            ])
            .env("GCS_MIMIC_TEST_REJECTED_WAL_DIR", &dir.0)
            .env("GCS_MIMIC_TEST_REJECTED_WAL_FIXTURE", fixture)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let paths = [
            dir.0.join("catalog.sqlite3"),
            dir.0.join("catalog.sqlite3-wal"),
        ];
        let before = paths.each_ref().map(|path| fs::read(path).unwrap());
        assert!(!before[1].is_empty(), "{fixture}: committed WAL must exist");
        fs::write(dir.0.join("staging/sentinel"), b"untouched").unwrap();
        for _ in 0..2 {
            assert!(Catalog::open(&dir.0).await.is_err(), "{fixture}");
            for (path, bytes) in paths.iter().zip(&before) {
                assert_eq!(
                    fs::read(path).unwrap(),
                    *bytes,
                    "{fixture}: {}",
                    path.display()
                );
            }
            assert!(dir.0.join("staging/sentinel").exists(), "{fixture}");
        }
    }
}

#[tokio::test]
async fn recovers_committed_wal_after_process_exit_without_drop() {
    let dir = TestDir::new();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "storage::catalog_tests::catalog_child_process",
            "--nocapture",
        ])
        .env("GCS_MIMIC_TEST_RECOVERY_DIR", &dir.0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.0.join("catalog.sqlite3-wal").exists());
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(
        catalog.get_bucket("committed").await.unwrap().unwrap().name,
        "committed"
    );
    catalog
        .insert_bucket(bucket("after-recovery", "project"))
        .await
        .unwrap();
}
