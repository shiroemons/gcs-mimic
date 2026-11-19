//! Files directly in the root, such as a `README.md`, a `.gitkeep` or Finder's `.DS_Store`, and
//! the one classification that ingestion and the reconciliation share. Ingestion once took such
//! a file for an unmappable name and stopped deletion detection for the whole root over it,
//! while the reconciliation counted the same file as ignored.
#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::baseline::{COMPLETE, exists, ingest};
use super::*;
use crate::storage::sync_state::root::Baseline;
use crate::sync::entries::{EntryClass, EntryProblem, Skip, classify};

const ROOT_FILES: [&str; 3] = [".DS_Store", "README.md", ".gitkeep"];

fn put_root_files(tree: &Path) {
    for name in ROOT_FILES {
        fs::write(tree.join(name), name).unwrap();
    }
}

/// The root files warned about so far, sorted: the scan's order is the directory's.
fn warned(status: &Lifecycle) -> Vec<PathBuf> {
    let mut warned = status.entries().warned_root_files();
    warned.sort();
    warned
}

/// Every root file but the ignored `.DS_Store` is warned about, each once.
fn warned_once() -> Vec<PathBuf> {
    vec![PathBuf::from(".gitkeep"), PathBuf::from("README.md")]
}

/// Root files put in after the mirror started let the baseline complete and a deleted file
/// delete its object, and later passes do not warn about them again.
#[tokio::test]
async fn files_in_the_root_hold_nothing_back_and_are_warned_about_once() {
    let (_data, tree, catalog, bucket, root) = live_fixture().await;
    publish_named(&catalog, &bucket, "kept", b"kept", Origin::Api).await;
    publish_named(&catalog, &bucket, "deleted", b"deleted", Origin::Api).await;
    put_root_files(&tree.0);
    let status = Lifecycle::default();
    let mut baseline = BaselineProgress {
        gate: BaselineGate::Allowed,
        complete: false,
    };
    let summary = ingest(&catalog, &root, &status, &mut baseline).await;
    assert!(summary.baseline_complete);
    assert_eq!((summary.ignored, summary.unrepresentable()), (3, 0));
    assert!(tree.0.join("test-bucket/deleted").is_file());

    fs::remove_file(tree.0.join("test-bucket/deleted")).unwrap();
    let summary = ingest(&catalog, &root, &status, &mut baseline).await;
    assert!(summary.baseline_complete);
    assert!(!exists(&catalog, &bucket, "deleted").await);
    assert!(exists(&catalog, &bucket, "kept").await);
    ingest(&catalog, &root, &status, &mut baseline).await;
    assert_eq!(warned(&status), warned_once());
    for name in ROOT_FILES {
        assert!(tree.0.join(name).is_file(), "{name}");
    }
    drop(root);
    catalog.close().await.unwrap();
}

/// Root files already there when a root is first activated hold back neither the first
/// baseline nor a later deletion.
#[tokio::test]
async fn files_in_the_root_before_the_first_start_hold_nothing_back() {
    let data = TestDir::new();
    let tree = TestDir::new();
    fs::create_dir(tree.0.join("test-bucket")).unwrap();
    for name in ["kept", "deleted"] {
        fs::write(tree.0.join("test-bucket").join(name), name).unwrap();
    }
    put_root_files(&tree.0);
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
    let status = Lifecycle::default();
    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let live = initialize_live(
        LiveRun {
            catalog: &catalog,
            root: &root,
            config: &config,
            status: &status,
            resume: &mut None,
        },
        &stopping,
    )
    .await
    .unwrap();
    assert_eq!(root.current_baseline().await.unwrap(), Baseline::Complete);
    let bucket = BucketName::new("test-bucket").unwrap();
    assert!(exists(&catalog, &bucket, "deleted").await);

    fs::remove_file(tree.0.join("test-bucket/deleted")).unwrap();
    let mut baseline = COMPLETE;
    let summary = ingest(&catalog, &root, &status, &mut baseline).await;
    assert_eq!((summary.ignored, summary.unrepresentable()), (3, 0));
    assert!(!exists(&catalog, &bucket, "deleted").await);
    assert!(exists(&catalog, &bucket, "kept").await);
    assert_eq!(warned(&status), warned_once());
    drop(live);
    drop(root);
    catalog.close().await.unwrap();
}

/// What one entry is, without the parts a scan observed of it.
fn kind(class: &EntryClass) -> &'static str {
    match class {
        EntryClass::Bucket(_) => "bucket",
        EntryClass::Directory => "directory",
        EntryClass::File(_) => "file",
        EntryClass::Skipped(Skip::IgnoredName) => "ignored-name",
        EntryClass::Skipped(Skip::RootFile) => "root-file",
        EntryClass::Unreadable(_) => "unreadable",
        EntryClass::Unrepresentable(problem) => match problem {
            EntryProblem::UnmappableName => "unmappable-name",
            EntryProblem::UnsafeEntry => "unsafe-entry",
            EntryProblem::Unreadable => "unreadable",
            EntryProblem::PathCollision => "path-collision",
            EntryProblem::DeletedBucket => "deleted-bucket",
        },
    }
}

/// Every entry of the tree and what it is, sorted by path.
async fn classified(root: &RootLease) -> Vec<(PathBuf, &'static str)> {
    let naming = root.naming().unwrap();
    let mut scan = crate::sync::tree::TreeScan::new(root.tree_limits(), root.lookup().unwrap());
    let mut classified = Vec::new();
    while !scan.is_empty() {
        let (next, page) = root.file_job(move |root| scan.page(root)).await.unwrap();
        scan = next;
        for entry in page {
            let class = classify(&entry, naming);
            classified.push((entry.path, kind(&class)));
        }
    }
    classified.sort();
    classified
}

/// Ingestion and the reconciliation's count of external files read the same tree the same way:
/// both skip the same files, name the same entries and find the same new files.
#[tokio::test]
async fn ingestion_and_reconciliation_classify_every_entry_alike() {
    let (_data, tree, catalog, _bucket, root) = live_fixture().await;
    let outside = TestDir::new();
    fs::write(outside.0.join("target"), b"outside").unwrap();
    let link = |path: &str| {
        std::os::unix::fs::symlink(outside.0.join("target"), tree.0.join(path)).unwrap();
    };
    put_root_files(&tree.0);
    link("root-link");
    for directory in ["Not_A_Bucket", "test-bucket", "test-bucket/._assets"] {
        fs::create_dir(tree.0.join(directory)).unwrap();
    }
    for file in [
        "Not_A_Bucket/file",
        "test-bucket/.DS_Store",
        "test-bucket/4913",
        "test-bucket/editor~",
        "test-bucket/._assets/icon",
        "test-bucket/new",
        "test-bucket/back\\slash",
    ] {
        fs::write(tree.0.join(file), file).unwrap();
    }
    link("test-bucket/notes.swp");
    let mut expected = vec![
        (".DS_Store", "ignored-name"),
        (".gitkeep", "root-file"),
        ("Not_A_Bucket", "unmappable-name"),
        ("Not_A_Bucket/file", "unmappable-name"),
        ("README.md", "root-file"),
        ("root-link", "unsafe-entry"),
        ("test-bucket", "bucket"),
        ("test-bucket/.DS_Store", "ignored-name"),
        ("test-bucket/4913", "ignored-name"),
        ("test-bucket/._assets", "directory"),
        ("test-bucket/._assets/icon", "file"),
        ("test-bucket/back\\slash", "unmappable-name"),
        ("test-bucket/editor~", "ignored-name"),
        ("test-bucket/new", "file"),
        ("test-bucket/notes.swp", "unsafe-entry"),
    ]
    .into_iter()
    .map(|(path, kind)| (PathBuf::from(path), kind))
    .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(classified(&root).await, expected);

    let (_stop, stopping) = tokio::sync::watch::channel(false);
    let mut counted = Reconciliation::default();
    external_files(
        &root,
        &root.sync_admin(),
        Interrupt::new(&stopping),
        &mut counted,
    )
    .await
    .unwrap();
    let ingested = crate::sync::ingest::rescan(&catalog, &root, &Config::default(), &stopping)
        .await
        .unwrap();
    assert_eq!(
        (
            counted.ignored,
            counted.external_only,
            counted.unrepresentable()
        ),
        (6, 2, 5)
    );
    assert_eq!(
        (
            ingested.ignored,
            ingested.ingested,
            ingested.unrepresentable()
        ),
        (
            counted.ignored,
            counted.external_only,
            counted.unrepresentable()
        )
    );
    assert_eq!(ingested.entries(), counted.entries());
    drop(root);
    catalog.close().await.unwrap();
}
