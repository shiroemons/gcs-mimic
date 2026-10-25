#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::{storage::Catalog, test_support::TestDir};
use std::{fs, os::unix::fs::symlink, time::Duration};

async fn setup() -> (TestDir, TestDir, Catalog) {
    let data = TestDir::new();
    let root = TestDir::new();
    let catalog = Catalog::open(&data.0).await.unwrap();
    (data, root, catalog)
}

#[tokio::test]
async fn sync_root_activation_baseline_reopen_and_reinitialize_preserve_data() {
    let (data, root, catalog) = setup().await;
    let state = catalog.sync_state();
    let id = state.catalog_id().await.unwrap();
    catalog
        .run_admin(|connection| {
            connection
                .execute("UPDATE startup_seed SET complete = 1", [])
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
    fs::write(root.0.join("conflict"), b"preserve").unwrap();
    let lease = state
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    let root_id = lease.status().root_id;
    assert_eq!(lease.status().catalog_id, id);
    assert!(!lease.status().permits_external_deletions());
    assert!(
        lease
            .complete_baseline()
            .await
            .unwrap()
            .permits_external_deletions()
    );
    drop(lease);
    let unrelated = TestDir::new();
    let second = state
        .activate_root(&unrelated.0, Activation::Open)
        .await
        .unwrap();
    assert_ne!(second.status().root_id, root_id);
    second.complete_baseline().await.unwrap();
    drop(second);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let state = catalog.sync_state();
    let lease = state
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    assert_eq!(lease.status().root_id, root_id);
    assert!(lease.status().permits_external_deletions());
    drop(lease);
    assert!(matches!(
        state
            .activate_root(&root.0, Activation::Reinitialize { catalog_id: id })
            .await,
        Err(RootError::ReinitializeDenied)
    ));
    fs::remove_file(root.0.join(MARKER)).unwrap();
    assert!(matches!(
        state.activate_root(&root.0, Activation::Open).await,
        Err(RootError::MissingMarker)
    ));
    assert!(matches!(
        state
            .activate_root(
                &root.0,
                Activation::Reinitialize {
                    catalog_id: CatalogId::from_bytes([0; 16])
                }
            )
            .await,
        Err(RootError::ReinitializeDenied)
    ));
    let lease = state
        .activate_root(&root.0, Activation::Reinitialize { catalog_id: id })
        .await
        .unwrap();
    assert!(!lease.status().permits_external_deletions());
    assert_eq!(lease.status().root_id, root_id);
    assert_eq!(fs::read(root.0.join("conflict")).unwrap(), b"preserve");
    assert!(
        catalog
            .admin(crate::storage::admin::Origin::Seed)
            .seed_complete()
            .await
            .unwrap()
    );
    drop(lease);
    let second = state
        .activate_root(&unrelated.0, Activation::Open)
        .await
        .unwrap();
    assert!(second.status().permits_external_deletions());
    drop(second);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_root_exclusive_inode_ownership_blocks_catalogs_and_lock_replacement() {
    let (data, root, catalog) = setup().await;
    let other_data = TestDir::new();
    let other = Catalog::open(&other_data.0).await.unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    fs::write(root.0.join("catalog.lock"), b"arbitrary").unwrap();
    fs::remove_file(root.0.join("catalog.lock")).unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "storage::catalog::sync_state::root::tests::sync_root_lock_child",
        ])
        .env("GCS_MIMIC_ROOT_LOCK_TEST", &root.0)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert!(matches!(
        other
            .sync_state()
            .activate_root(&root.0, Activation::Open)
            .await,
        Err(RootError::InUse)
    ));
    assert!(matches!(
        catalog
            .sync_state()
            .activate_root(&root.0, Activation::Open)
            .await,
        Err(RootError::InUse)
    ));
    fs::remove_file(data.0.join("catalog.lock")).unwrap();
    assert!(Catalog::open(&data.0).await.is_err());
    drop(lease);
    assert!(matches!(
        other
            .sync_state()
            .activate_root(&root.0, Activation::Open)
            .await,
        Err(RootError::ForeignMarker)
    ));
    other.close().await.unwrap();
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_root_replacement_symlinks_and_marker_changes_fail_closed() {
    let (_data, root, catalog) = setup().await;
    let state = catalog.sync_state();
    let lease = state
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    let id = lease.status().catalog_id;
    let saved = root.0.with_extension("saved");
    fs::rename(&root.0, &saved).unwrap();
    fs::create_dir(&root.0).unwrap();
    assert_eq!(
        lease.file_job(|_| Ok(())).await,
        Err(RootError::RootChanged)
    );
    drop(lease);
    assert!(matches!(
        state.activate_root(&root.0, Activation::Open).await,
        Err(RootError::RootChanged)
    ));
    assert!(matches!(
        state
            .activate_root(&root.0, Activation::Reinitialize { catalog_id: id })
            .await,
        Err(RootError::RootChanged)
    ));
    fs::remove_dir(&root.0).unwrap();
    symlink(&saved, &root.0).unwrap();
    assert!(
        state
            .activate_root(&root.0, Activation::Open)
            .await
            .is_err()
    );
    fs::remove_file(&root.0).unwrap();
    fs::rename(&saved, &root.0).unwrap();
    let lease = state
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    let marker = fs::read(root.0.join(MARKER)).unwrap();
    fs::remove_file(root.0.join(MARKER)).unwrap();
    fs::write(root.0.join(MARKER), &marker).unwrap();
    assert_eq!(
        lease.file_job(|_| Ok(())).await,
        Err(RootError::InvalidMarker)
    );
    drop(lease);
    fs::remove_file(root.0.join(MARKER)).unwrap();
    let target = root.0.join("target");
    fs::write(&target, &marker).unwrap();
    symlink(&target, root.0.join(MARKER)).unwrap();
    assert!(matches!(
        state
            .activate_root(&root.0, Activation::Reinitialize { catalog_id: id })
            .await,
        Err(RootError::InvalidMarker)
    ));
    assert_eq!(fs::read(&target).unwrap(), marker);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_root_trailing_separators_and_dots_do_not_follow_symlinks() {
    let (_data, root, catalog) = setup().await;
    let aliases = TestDir::new();
    let alias = aliases.0.join("selected");
    symlink(&root.0, &alias).unwrap();
    for suffix in ["", "/", "/.", "/./", "//./."] {
        let selected = PathBuf::from(format!("{}{suffix}", alias.display()));
        assert!(
            catalog
                .sync_state()
                .activate_root(&selected, Activation::Open)
                .await
                .is_err()
        );
        assert!(!root.0.join(MARKER).exists());
    }
    let selected = PathBuf::from(format!("{}/./", root.0.display()));
    drop(
        catalog
            .sync_state()
            .activate_root(selected, Activation::Open)
            .await
            .unwrap(),
    );
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_root_selected_parent_alias_retarget_stops_owned_jobs() {
    let (_data, original, catalog) = setup().await;
    let replacement = TestDir::new();
    fs::create_dir(original.0.join("tree")).unwrap();
    fs::create_dir(replacement.0.join("tree")).unwrap();
    let aliases = TestDir::new();
    let alias = aliases.0.join("parent");
    symlink(&original.0, &alias).unwrap();
    let selected = PathBuf::from(format!("{}/tree/./", alias.display()));
    let lease = catalog
        .sync_state()
        .activate_root(&selected, Activation::Open)
        .await
        .unwrap();
    fs::remove_file(&alias).unwrap();
    symlink(&replacement.0, &alias).unwrap();
    assert_eq!(
        lease
            .file_job(|_| panic!("changed selected root must not run file work"))
            .await,
        Err::<(), _>(RootError::RootChanged)
    );
    assert_eq!(lease.complete_baseline().await, Err(RootError::RootChanged));
    assert!(!replacement.0.join("tree").join(MARKER).exists());
    fs::remove_file(&alias).unwrap();
    symlink(&original.0, &alias).unwrap();
    lease.file_job(|_| Ok(())).await.unwrap();
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_root_marker_invalid_foreign_and_size_boundaries_are_preserved() {
    let (_data, root, catalog) = setup().await;
    let state = catalog.sync_state();
    let lease = state
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    let status = lease.status();
    let valid = fs::read(root.0.join(MARKER)).unwrap();
    drop(lease);
    let mut cases = vec![
        b"{".to_vec(),
        vec![b'x'; 4097],
        b"null".to_vec(),
        b"{}".to_vec(),
    ];
    let text = String::from_utf8(valid.clone()).unwrap();
    cases.push(text.replace("\"version\":1", "\"version\":2").into_bytes());
    cases.push(
        text.replace("\"version\":1", "\"version\":1,\"extra\":true")
            .into_bytes(),
    );
    cases.push(
        text.replace("\"version\":1", "\"version\":1,\"version\":1")
            .into_bytes(),
    );
    cases.push(
        text.replace(&status.root_id.to_string(), "invalid")
            .into_bytes(),
    );
    for bytes in cases {
        fs::write(root.0.join(MARKER), &bytes).unwrap();
        for mode in [
            Activation::Open,
            Activation::Reinitialize {
                catalog_id: status.catalog_id,
            },
        ] {
            assert!(matches!(
                state.activate_root(&root.0, mode).await,
                Err(RootError::InvalidMarker)
            ));
            assert_eq!(fs::read(root.0.join(MARKER)).unwrap(), bytes);
        }
    }
    let mut exact = valid.clone();
    exact.resize(4096, b' ');
    fs::write(root.0.join(MARKER), &exact).unwrap();
    drop(
        state
            .activate_root(&root.0, Activation::Open)
            .await
            .unwrap(),
    );
    exact.push(b' ');
    fs::write(root.0.join(MARKER), &exact).unwrap();
    assert!(matches!(
        state.activate_root(&root.0, Activation::Open).await,
        Err(RootError::InvalidMarker)
    ));
    let foreign = text.replace(
        &status.catalog_id.to_string(),
        &CatalogId::from_bytes([1; 16]).to_string(),
    );
    fs::write(root.0.join(MARKER), foreign.as_bytes()).unwrap();
    assert!(matches!(
        state.activate_root(&root.0, Activation::Open).await,
        Err(RootError::ForeignMarker)
    ));
    assert!(
        state
            .activate_root(
                &root.0,
                Activation::Reinitialize {
                    catalog_id: status.catalog_id
                }
            )
            .await
            .is_err()
    );
    let fresh = TestDir::new();
    fs::write(fresh.0.join(MARKER), &valid).unwrap();
    assert!(matches!(
        state.activate_root(&fresh.0, Activation::Open).await,
        Err(RootError::ForeignMarker)
    ));
    fs::remove_file(root.0.join(MARKER)).unwrap();
    fs::create_dir(root.0.join(MARKER)).unwrap();
    assert!(matches!(
        state.activate_root(&root.0, Activation::Open).await,
        Err(RootError::InvalidMarker)
    ));
    fs::remove_dir(root.0.join(MARKER)).unwrap();
    crate::test_support::fifo(&root.0.join(MARKER));
    assert!(matches!(
        state.activate_root(&root.0, Activation::Open).await,
        Err(RootError::InvalidMarker)
    ));
    catalog.close().await.unwrap();
}

#[test]
fn sync_root_lock_child() {
    if let Some(path) = std::env::var_os("GCS_MIMIC_ROOT_LOCK_TEST") {
        assert!(
            matches!(lock::acquire_directory(Path::new(&path)), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
        );
    }
}

#[tokio::test]
async fn sync_root_sql_failures_never_publish_marker_or_complete_baseline() {
    let (_data, root, catalog) = setup().await;
    catalog.run_admin(|connection| {
        connection.execute_batch("CREATE TEMP TRIGGER refuse_root BEFORE INSERT ON sync_roots BEGIN SELECT RAISE(ABORT, 'injected root reservation failure'); END;").map_err(super::super::super::unavailable)
    }).await.unwrap();
    assert!(
        catalog
            .sync_state()
            .activate_root(&root.0, Activation::Open)
            .await
            .is_err()
    );
    assert!(!root.0.join(MARKER).exists());
    catalog
        .run_admin(|connection| {
            connection
                .execute_batch("DROP TRIGGER refuse_root")
                .map_err(super::super::super::unavailable)
        })
        .await
        .unwrap();
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    catalog.run_admin(|connection| connection.execute_batch("CREATE TEMP TRIGGER refuse_baseline BEFORE UPDATE ON sync_roots BEGIN SELECT RAISE(ABORT, 'injected baseline failure'); END;").map_err(super::super::super::unavailable)).await.unwrap();
    assert!(lease.complete_baseline().await.is_err());
    drop(lease);
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    assert!(!lease.status().permits_external_deletions());
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_root_interrupted_activation_requires_explicit_missing_marker_recovery() {
    let (data, root, catalog) = setup().await;
    let state = catalog.sync_state();
    let lease = state
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    let status = lease.status();
    drop(lease);
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let state = catalog.sync_state();
    let lease = state
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    assert_eq!(lease.status(), status);
    drop(lease);
    fs::remove_file(root.0.join(MARKER)).unwrap();
    catalog.close().await.unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    let state = catalog.sync_state();
    assert!(matches!(
        state.activate_root(&root.0, Activation::Open).await,
        Err(RootError::MissingMarker)
    ));
    let lease = state
        .activate_root(
            &root.0,
            Activation::Reinitialize {
                catalog_id: status.catalog_id,
            },
        )
        .await
        .unwrap();
    assert_eq!(lease.status(), status);
    drop(lease);
    catalog.close().await.unwrap();
}

#[tokio::test]
async fn sync_root_cancelled_file_job_keeps_both_ownerships_until_drain() {
    let (data, root, catalog) = setup().await;
    let lease = catalog
        .sync_state()
        .activate_root(&root.0, Activation::Open)
        .await
        .unwrap();
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::sync_channel(1);
    let job_lease = lease.clone();
    let job = tokio::spawn(async move {
        job_lease
            .file_job(move |_| {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                Ok(())
            })
            .await
    });
    started.await.unwrap();
    job.abort();
    drop(lease);
    let close_catalog = catalog.clone();
    let mut close = tokio::spawn(async move { close_catalog.close().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut close)
            .await
            .is_err()
    );
    assert!(Catalog::open(&data.0).await.is_err());
    assert!(lock::acquire_directory(&root.0).is_err());
    release.send(()).unwrap();
    close.await.unwrap().unwrap();
    drop(lock::acquire_directory(&root.0).unwrap());
    let reopened = Catalog::open(&data.0).await.unwrap();
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn sync_root_schema_migration_rolls_back_retries_and_preserves_state() {
    let (data, _root, catalog) = setup().await;
    let id = catalog.sync_state().catalog_id().await.unwrap();
    let admin = catalog.admin(crate::storage::admin::Origin::SyncIngest);
    let bucket = crate::storage::admin::BucketName::new("preserved-bucket").unwrap();
    admin
        .ensure_bucket(
            crate::storage::admin::BucketDefinition::default_for(
                &bucket,
                "owner",
                &crate::clock::SystemClock,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let before = admin.get_bucket(&bucket).await.unwrap();
    catalog.close().await.unwrap();
    let mut connection = Connection::open(data.0.join("catalog.sqlite3")).unwrap();
    connection
        .execute_batch(
            "DROP TABLE sync_writeback_ops; DROP TABLE sync_writeback_roots; DROP TABLE sync_roots; PRAGMA user_version = 18; CREATE TABLE sync_roots(collision);",
        )
        .unwrap();
    assert!(migrate(&mut connection).is_err());
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        18
    );
    connection.execute_batch("DROP TABLE sync_roots;").unwrap();
    let pages: i64 = connection
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    connection
        .pragma_update(None, "max_page_count", pages)
        .unwrap();
    connection
        .execute_batch("CREATE TABLE filler(value BLOB);")
        .unwrap();
    while connection
        .execute("INSERT INTO filler VALUES(zeroblob(4096))", [])
        .is_ok()
    {}
    assert!(migrate(&mut connection).is_err());
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        18
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name = 'sync_roots'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    connection
        .pragma_update(None, "max_page_count", 1_000_000)
        .unwrap();
    connection.execute_batch("DROP TABLE filler").unwrap();
    {
        let transaction = connection.transaction().unwrap();
        transaction.execute_batch(SCHEMA).unwrap();
        transaction.rollback().unwrap();
    }
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        18
    );
    assert_eq!(catalog_id(&connection).unwrap(), id);
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        18
    );
    drop(connection);
    let reopened = Catalog::open(&data.0).await.unwrap();
    assert_eq!(reopened.sync_state().catalog_id().await.unwrap(), id);
    assert_eq!(
        reopened
            .admin(crate::storage::admin::Origin::SyncIngest)
            .get_bucket(&bucket)
            .await
            .unwrap(),
        before
    );
    reopened.close().await.unwrap();
}

#[tokio::test]
async fn sync_root_activation_failure_boundaries_preserve_reservation_and_refuse_partial_marker() {
    for phase in ["reserved", "created", "file_synced", "directory_synced"] {
        let (data, root, catalog) = setup().await;
        let id = catalog.sync_state().catalog_id().await.unwrap();
        *FAILURE.lock().unwrap() = Some((root.0.canonicalize().unwrap(), phase));
        assert!(
            catalog
                .sync_state()
                .activate_root(&root.0, Activation::Open)
                .await
                .is_err()
        );
        *FAILURE.lock().unwrap() = None;
        catalog.close().await.unwrap();
        let catalog = Catalog::open(&data.0).await.unwrap();
        let state = catalog.sync_state();
        match phase {
            "reserved" => {
                assert!(matches!(
                    state.activate_root(&root.0, Activation::Open).await,
                    Err(RootError::MissingMarker)
                ));
                drop(
                    state
                        .activate_root(&root.0, Activation::Reinitialize { catalog_id: id })
                        .await
                        .unwrap(),
                );
            }
            "created" => {
                assert_eq!(fs::read(root.0.join(MARKER)).unwrap(), b"");
                assert!(matches!(
                    state.activate_root(&root.0, Activation::Open).await,
                    Err(RootError::InvalidMarker)
                ));
                assert!(matches!(
                    state
                        .activate_root(&root.0, Activation::Reinitialize { catalog_id: id })
                        .await,
                    Err(RootError::InvalidMarker)
                ));
            }
            _ => {
                let lease = state
                    .activate_root(&root.0, Activation::Open)
                    .await
                    .unwrap();
                assert!(!lease.status().permits_external_deletions());
                drop(lease);
            }
        }
        catalog.close().await.unwrap();
    }
}
