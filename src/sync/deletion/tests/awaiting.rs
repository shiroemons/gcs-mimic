//! A set that waits for approval keeps waiting until someone approves or rejects it, or until
//! every file it holds is back. Putting some files back, or adding files that make the deleted
//! ones fewer than half, once replaced it with a set below the threshold, which then ran
//! without anyone deciding.

use super::*;

const FILES: usize = 150;
const DELETED: usize = 120;

fn path(tree: &TestDir, index: usize) -> std::path::PathBuf {
    tree.0.join(format!("test-bucket/{index:04}"))
}

fn pass() -> Reconciliation {
    Reconciliation {
        baseline_complete: true,
        ..Default::default()
    }
}

/// Deletes the first `DELETED` files and inventories them, which takes them to approval.
async fn awaiting(tree: &TestDir, root: &RootLease, config: &Config) -> DeletionSet {
    for index in 0..DELETED {
        fs::remove_file(path(tree, index)).unwrap();
    }
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let set = inventory(root, config, pass().gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((set.candidate_count, set.guarded), (DELETED as u64, true));
    set
}

fn put_back(tree: &TestDir, indices: std::ops::Range<usize>) {
    for index in indices {
        fs::write(path(tree, index), b"immutable").unwrap();
    }
}

/// Inventories again, runs the set, and returns it with how many objects it deleted.
async fn inventory_and_run(
    catalog: &Catalog,
    root: &RootLease,
    config: &Config,
) -> (DeletionSet, u64) {
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let set = inventory(root, config, pass().gate(), &stopping)
        .await
        .unwrap()
        .unwrap();
    let deleted = execute(catalog, root, set.clone(), &stopping)
        .await
        .unwrap();
    (set, deleted)
}

async fn all_objects_exist(catalog: &Catalog) {
    let bucket = BucketName::new("test-bucket").unwrap();
    for index in 0..FILES {
        let name = ObjectName::new(format!("{index:04}")).unwrap();
        assert!(
            catalog
                .admin(Origin::Api)
                .get_object(&bucket, &name, None)
                .await
                .is_ok(),
            "{index}"
        );
    }
}

/// Forty of the deleted files come back by hand. The eighty still missing are below the
/// threshold on their own, but they were part of the set that waits, so they keep waiting.
#[tokio::test]
async fn files_put_back_by_hand_leave_the_rest_waiting() {
    let (_data, tree, catalog, root, config) = fixture(FILES).await;
    awaiting(&tree, &root, &config).await;
    put_back(&tree, 0..40);
    let (set, deleted) = inventory_and_run(&catalog, &root, &config).await;
    assert_eq!((set.candidate_count, set.guarded), (80, true));
    assert_eq!(deleted, 0);
    all_objects_exist(&catalog).await;
    drop(root);
    catalog.close().await.unwrap();
}

/// A hundred new files make the deleted ones fewer than half of the tracked files. The set
/// still waits, under the revision someone may already be about to approve or reject.
#[tokio::test]
async fn files_added_beside_a_waiting_set_leave_it_waiting() {
    let (_data, tree, catalog, root, config) = fixture(FILES).await;
    let waiting = awaiting(&tree, &root, &config).await;
    for index in 0..100 {
        fs::write(
            tree.0.join(format!("test-bucket/added-{index:04}")),
            b"added",
        )
        .unwrap();
    }
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let scanned = crate::sync::ingest::rescan(&catalog, &root, &config, &stopping)
        .await
        .unwrap();
    assert_eq!(scanned.ingested, 100);
    let (set, deleted) = inventory_and_run(&catalog, &root, &config).await;
    assert_eq!((set.candidate_count, set.guarded), (DELETED as u64, true));
    assert_eq!(set.revision, waiting.revision);
    assert_eq!(deleted, 0);
    all_objects_exist(&catalog).await;
    drop(root);
    catalog.close().await.unwrap();
}

/// Opens the catalog in `data` again over `tree`, as a restart does.
async fn reopen(data: &TestDir, tree: &TestDir) -> (Catalog, RootLease) {
    let catalog = Catalog::open(&data.0).await.unwrap();
    catalog.enable_change_journal().await.unwrap();
    let root = catalog
        .sync_state()
        .activate_root(&tree.0, Activation::Open)
        .await
        .unwrap();
    (catalog, crate::sync::test_support::probed(root).await)
}

/// Forty files come back after a restart, and the eighty still missing keep waiting under a
/// new revision.
async fn restarted_with_some_back(data: &TestDir, tree: &TestDir, config: &Config) {
    let (catalog, root) = reopen(data, tree).await;
    put_back(tree, 0..40);
    let (set, deleted) = inventory_and_run(&catalog, &root, config).await;
    assert_eq!((set.candidate_count, set.guarded), (80, true));
    assert_eq!(deleted, 0);
    all_objects_exist(&catalog).await;
    drop(root);
    catalog.close().await.unwrap();
}

/// A restart withdraws the waiting set's revision, but not the wait itself.
#[tokio::test]
async fn a_restart_keeps_the_rest_of_a_waiting_set_waiting() {
    let (data, tree, catalog, root, config) = fixture(FILES).await;
    awaiting(&tree, &root, &config).await;
    drop(root);
    catalog.close().await.unwrap();
    restarted_with_some_back(&data, &tree, &config).await;
}

/// A catalog upgraded to keep the files of a waiting set apart, while a set waits, keeps that
/// set's files waiting.
#[tokio::test]
async fn an_upgrade_keeps_the_rest_of_a_waiting_set_waiting() {
    let (data, tree, catalog, root, config) = fixture(FILES).await;
    awaiting(&tree, &root, &config).await;
    drop(root);
    crate::storage::test_fixtures::before_held_files(&data, &catalog).await;
    restarted_with_some_back(&data, &tree, &config).await;
}

/// Once every file of the waiting set is back, nothing waits any more: a few files deleted
/// afterwards are below the threshold and go without approval, as any would.
#[tokio::test]
async fn every_file_back_ends_the_wait() {
    let (_data, tree, catalog, root, config) = fixture(FILES).await;
    awaiting(&tree, &root, &config).await;
    put_back(&tree, 0..DELETED);
    let (set, deleted) = inventory_and_run(&catalog, &root, &config).await;
    assert_eq!((set.candidate_count, deleted), (0, 0));
    for index in 0..10 {
        fs::remove_file(path(&tree, index)).unwrap();
    }
    let (set, deleted) = inventory_and_run(&catalog, &root, &config).await;
    assert_eq!((set.candidate_count, set.guarded), (10, false));
    assert_eq!(deleted, 10);
    drop(root);
    catalog.close().await.unwrap();
}
