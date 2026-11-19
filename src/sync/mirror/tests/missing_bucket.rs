//! Exports into a bucket whose whole directory is missing while the tree tracks files in it.
//! Creating the directory for one export would turn the missing files into deletions counted one
//! by one, which the guard lets through below its threshold, so every export into the bucket
//! waits until the deletion is approved or the directory is back.
#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::baseline::exists;
use super::ingest_durability::Started;
use super::*;
use crate::storage::sync_state::deletions::DeletionSet;
use crate::sync::control::{WaitReason, WaitResult};
use crate::sync::outstanding::{Holdup, Outstanding};

const BODY: &[u8] = b"tracked bytes";
const BUCKET: &str = "test-bucket";
const TRACKED: [&str; 2] = ["first", "second"];
const ALL: [&str; 3] = ["first", "second", "third"];

/// A root whose bucket directory holds `first` and `second`, which a start tracked and whose
/// baseline it completed.
async fn tracked_pair() -> (TestDir, TestDir, BucketName) {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    drop(root);
    catalog.close().await.unwrap();
    write_pair(&tree.0);
    Started::start(&data.0, &tree.0).await.close().await;
    (data, tree, bucket)
}

/// Puts the bucket directory with `first` and `second` in place, as a user restores it.
fn write_pair(tree: &Path) {
    fs::create_dir(tree.join(BUCKET)).unwrap();
    for name in TRACKED {
        fs::write(tree.join(BUCKET).join(name), BODY).unwrap();
    }
}

async fn all_exist(catalog: &Catalog, bucket: &BucketName) {
    for name in ALL {
        assert!(exists(catalog, bucket, name).await, "{name}");
    }
}

/// The directory goes away while the server is stopped, and an object is written into its
/// bucket through the API meanwhile. The startup reconciliation does not export the new object,
/// so the directory stays missing and both tracked files wait for approval. Putting the
/// directory back releases the export.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_object_written_while_stopped_waits_with_its_missing_bucket_directory() {
    let (data, tree, bucket) = tracked_pair().await;
    fs::remove_dir_all(tree.0.join(BUCKET)).unwrap();
    let catalog = Catalog::open(&data.0).await.unwrap();
    publish_named(&catalog, &bucket, "third", BODY, Origin::Api).await;
    catalog.close().await.unwrap();

    let started = Started::start(&data.0, &tree.0).await;
    assert!(!tree.0.join(BUCKET).exists());
    let set = started.root.deletion_set().await.unwrap().unwrap();
    assert_eq!((set.candidate_count, set.guarded), (2, true));
    assert_eq!(set.completed_count, 0);
    all_exist(&started.catalog, &bucket).await;
    started.close().await;

    write_pair(&tree.0);
    let started = Started::start(&data.0, &tree.0).await;
    assert_eq!(fs::read(tree.0.join(BUCKET).join("third")).unwrap(), BODY);
    let set = started.root.deletion_set().await.unwrap();
    assert_eq!(set.map_or(0, |set| set.candidate_count), 0);
    all_exist(&started.catalog, &bucket).await;
    started.close().await;
}

async fn wait_control(commands: &tokio::sync::mpsc::Sender<Command>) -> WaitResult {
    let (reply, result) = tokio::sync::oneshot::channel();
    commands
        .send(Command::Wait {
            deadline: crate::test_support::hang_deadline(),
            settling: Default::default(),
            reply,
            identity: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(HANG_GUARD, result)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

async fn approve_control(commands: &tokio::sync::mpsc::Sender<Command>, root: &RootLease) {
    let revision = root.deletion_set().await.unwrap().unwrap().revision;
    let (reply, approved) = tokio::sync::oneshot::channel();
    commands
        .send(Command::Approve {
            revision,
            reply,
            identity: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(HANG_GUARD, approved)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// The directory goes away while the server runs, and an API write into its bucket follows. The
/// replayed export waits instead of recreating the directory, a wait reports the pending
/// approval, and approving the set deletes the tracked files and then runs the export.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_api_write_into_a_missing_bucket_directory_waits_for_the_approval() {
    let (data, tree, bucket) = tracked_pair().await;
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
    let (stop, mut stopping) = tokio::sync::watch::channel(false);
    let (commands, mut requests) = tokio::sync::mpsc::channel(1);
    let worker_catalog = catalog.clone();
    let worker_root = root.clone();
    let worker = tokio::spawn(async move {
        run_live_controlled(&worker_catalog, &worker_root, &mut stopping, &mut requests).await
    });
    fs::remove_dir_all(tree.0.join(BUCKET)).unwrap();
    rescan_control(&commands).await;
    let set = root.deletion_set().await.unwrap().unwrap();
    assert_eq!((set.candidate_count, set.guarded), (2, true));

    publish_named(&catalog, &bucket, "third", BODY, Origin::Api).await;
    // The held export stays queued for the retry after the approval; every wait, not only the
    // first after the write, reads it as the pending approval, and so does the status.
    for _ in 0..2 {
        let waited = wait_control(&commands).await;
        assert_eq!(waited.reason, WaitReason::DeletionApproval);
    }
    let status = Outstanding::of_status(
        &root.observation().await.unwrap(),
        root.deletion_set().await.unwrap().as_ref(),
    );
    assert_eq!(status.first(), Holdup::DeletionApproval);
    assert!(!tree.0.join(BUCKET).exists());
    // The API write moved the journal, so the inventory offers the same members under a new
    // revision.
    let held = root.deletion_set().await.unwrap().unwrap();
    assert_eq!((held.candidate_count, held.guarded), (2, true));
    assert_eq!(root.automatic_deletion_count().await.unwrap(), 0);
    all_exist(&catalog, &bucket).await;

    approve_control(&commands, &root).await;
    eventually_async("the held export to run", || async {
        tree.0.join(BUCKET).join("third").is_file()
    })
    .await;
    assert_eq!(fs::read(tree.0.join(BUCKET).join("third")).unwrap(), BODY);
    for name in TRACKED {
        assert!(!exists(&catalog, &bucket, name).await, "{name}");
    }
    assert!(exists(&catalog, &bucket, "third").await);
    stop.send_replace(true);
    worker.await.unwrap().unwrap();
    drop(root);
    catalog.close().await.unwrap();
}

/// Inventories `root` again, as a later pass of the same run does, and returns its set.
async fn inventory_again(started: &Started, data: &Path, tree: &Path) -> DeletionSet {
    let config = Config {
        data_dir: data.to_owned(),
        sync_dir: Some(tree.to_owned()),
        ..Default::default()
    };
    let pass = Reconciliation {
        baseline_complete: true,
        ..Default::default()
    };
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    crate::sync::deletion::inventory(&started.root, &config, pass.gate(), &stopping)
        .await
        .unwrap()
        .unwrap()
}

/// Every tracked file of a bucket goes while the server is stopped and its directory stays, as
/// when a bind mount of the bucket comes up empty. The start takes the files to approval however
/// few they are, and a later pass of the run keeps them there while the bucket stays empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bucket_emptied_while_stopped_waits_for_approval() {
    let (data, tree, bucket) = tracked_pair().await;
    for name in TRACKED {
        fs::remove_file(tree.0.join(BUCKET).join(name)).unwrap();
    }
    let started = Started::start(&data.0, &tree.0).await;
    let set = started.root.deletion_set().await.unwrap().unwrap();
    assert_eq!((set.candidate_count, set.guarded), (2, true));
    assert_eq!(set.completed_count, 0);
    let again = inventory_again(&started, &data.0, &tree.0).await;
    assert_eq!((again.candidate_count, again.guarded), (2, true));
    for name in TRACKED {
        assert!(exists(&started.catalog, &bucket, name).await, "{name}");
    }
    assert!(tree.0.join(BUCKET).is_dir());
    started.close().await;
}

/// A bucket that keeps one of its tracked files is not emptied: the other file, deleted while
/// the server was stopped, deletes its object at the start without approval, as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_file_deleted_while_stopped_still_deletes_its_object() {
    let (data, tree, bucket) = tracked_pair().await;
    fs::remove_file(tree.0.join(BUCKET).join("first")).unwrap();
    let started = Started::start(&data.0, &tree.0).await;
    assert!(!exists(&started.catalog, &bucket, "first").await);
    assert!(exists(&started.catalog, &bucket, "second").await);
    started.close().await;
}

/// Emptying a bucket while the mirror runs is a deletion the user made in front of it, which the
/// counts alone take to approval, as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bucket_emptied_while_running_needs_no_approval() {
    let (data, tree, bucket) = tracked_pair().await;
    let started = Started::start(&data.0, &tree.0).await;
    for name in TRACKED {
        fs::remove_file(tree.0.join(BUCKET).join(name)).unwrap();
    }
    let set = inventory_again(&started, &data.0, &tree.0).await;
    assert_eq!((set.candidate_count, set.guarded), (2, false));
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    assert_eq!(
        crate::sync::deletion::execute(&started.catalog, &started.root, set, &stopping)
            .await
            .unwrap(),
        2
    );
    for name in TRACKED {
        assert!(!exists(&started.catalog, &bucket, name).await, "{name}");
    }
    started.close().await;
}
