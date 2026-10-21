use gcs_mimic::{
    bucket::{BucketRecord, CatalogError},
    storage::Catalog,
};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
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
        metageneration: 3,
        time_created: UNIX_EPOCH + Duration::from_micros(42_123_456),
        updated: UNIX_EPOCH + Duration::from_micros(43_987_654),
    }
}

#[tokio::test]
async fn create_get_delete_and_restart_preserve_every_field() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    assert_eq!(catalog.get_bucket("missing").await.unwrap(), None);
    let mut expected = bucket("bucket", "project");
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
        .args(["--exact", "catalog_child_process", "--nocapture"])
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
        command.args(["--exact", "catalog_child_process"]);
        // Only fixed-size socket I/O runs after fork; no allocation or locks are used.
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
async fn rejects_future_schema_unversioned_data_and_corruption() {
    for fixture in [
        "future",
        "unversioned",
        "corrupt",
        "missing-schema",
        "incompatible-schema",
        "v2-missing-objects",
        "v2-incompatible-objects",
    ] {
        let dir = TestDir::new();
        fs::create_dir_all(&dir.0).unwrap();
        let path = dir.0.join("catalog.sqlite3");
        match fixture {
            "future" => Connection::open(&path)
                .unwrap()
                .pragma_update(None, "user_version", 4)
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
        assert!(
            matches!(error, CatalogError::Unavailable(_)),
            "{fixture}: {error}"
        );
        if fixture == "future" {
            assert!(error.to_string().contains("schema version 4"));
        }
        if fixture == "unversioned" {
            assert!(error.to_string().contains("not empty"));
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
async fn rejected_checkpointed_wal_catalog_creates_no_sidecars_with_uri_characters() {
    let dir = TestDir::new();
    let directory = dir.0.join("catalog ?#%é");
    let catalog = Catalog::open(&directory).await.unwrap();
    catalog.close().await.unwrap();
    let path = directory.join("catalog.sqlite3");
    Connection::open(&path)
        .unwrap()
        .pragma_update(None, "user_version", 4)
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
                .contains("schema version 4")
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
            "future" => "PRAGMA user_version = 4;",
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
            .args(["--exact", "catalog_child_process", "--nocapture"])
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
        .args(["--exact", "catalog_child_process", "--nocapture"])
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
