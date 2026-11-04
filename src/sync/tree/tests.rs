use super::*;
use crate::test_support::TestDir;
use std::{collections::BTreeSet, fs};

#[test]
fn created_parents_survive_crash_and_power_loss_only_after_directory_sync() {
    use crate::storage::fault::{self, Fault, Step};
    for power_loss in [false, true] {
        for step in [Step::SyncDirectorySync, Step::SyncDirectoryDurable] {
            let tree = TestDir::new();
            let snapshot = TestDir::new();
            let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
            fault::arm(
                &tree.0,
                step,
                1,
                if power_loss {
                    Fault::PowerLoss(snapshot.0.clone())
                } else {
                    Fault::Snapshot(snapshot.0.clone())
                },
            );
            assert!(parent(&root, Path::new("bucket/nested"), ParentOpen::Create).is_ok());
            assert_eq!(
                snapshot.0.join("bucket/nested").is_dir(),
                !power_loss || step == Step::SyncDirectoryDurable
            );
        }
    }
    let tree = TestDir::new();
    let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
    fault::arm(
        &tree.0,
        Step::SyncDirectorySync,
        0,
        Fault::Io(io::ErrorKind::PermissionDenied),
    );
    assert!(matches!(
        parent(&root, Path::new("bucket"), ParentOpen::Create),
        Err(ParentError::Io(_))
    ));
}

#[test]
fn parent_sync_failure_retries_existing_ancestors_while_reads_never_sync() {
    use crate::storage::fault::{self, Fault, Step};
    let tree = TestDir::new();
    let snapshot = TestDir::new();
    let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
    fault::arm(
        &tree.0,
        Step::SyncDirectorySync,
        1,
        Fault::Io(io::ErrorKind::PermissionDenied),
    );
    fault::arm(
        &tree.0,
        Step::SyncDirectoryDurable,
        2,
        Fault::PowerLoss(snapshot.0.clone()),
    );
    assert!(matches!(
        parent(&root, Path::new("bucket/nested"), ParentOpen::Create),
        Err(ParentError::Io(_))
    ));
    assert!(parent(&root, Path::new("bucket/nested"), ParentOpen::Create).is_ok());
    assert!(snapshot.0.join("bucket/nested").is_dir());
    fault::arm(
        &tree.0,
        Step::SyncDirectorySync,
        0,
        Fault::Io(io::ErrorKind::PermissionDenied),
    );
    assert!(parent(&root, Path::new("bucket/nested"), ParentOpen::Existing).is_ok());
    assert!(matches!(
        parent(&root, Path::new("bucket/nested"), ParentOpen::Create),
        Err(ParentError::Io(_))
    ));
}

#[test]
fn existing_parents_never_create_missing_directories_and_reject_aliases_and_symlinks() {
    let tree = TestDir::new();
    let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
    assert!(matches!(
        parent(&root, Path::new("bucket/nested"), ParentOpen::Existing),
        Err(ParentError::Missing)
    ));
    assert!(!tree.0.join("bucket").exists());
    fs::create_dir(tree.0.join("Dir")).unwrap();
    if tree.0.join("dir").exists() {
        assert!(matches!(
            parent(&root, Path::new("dir"), ParentOpen::Existing),
            Err(ParentError::Finding(Reason::Collision))
        ));
    }
    std::os::unix::fs::symlink(tree.0.join("Dir"), tree.0.join("link")).unwrap();
    assert!(matches!(
        parent(&root, Path::new("link"), ParentOpen::Existing),
        Err(ParentError::Finding(Reason::UnsafeEntry))
    ));
    fs::write(tree.0.join("first"), b"hardlinked").unwrap();
    fs::hard_link(tree.0.join("first"), tree.0.join("second")).unwrap();
    for name in ["first", "second"] {
        assert!(matches!(
            fsutil::child_regular(&root, name.as_ref()).unwrap(),
            fsutil::RegularChild::File(_)
        ));
    }
}

#[test]
fn scans_cross_pages_once_and_reject_replaced_directories() {
    let tree = TestDir::new();
    fs::create_dir(tree.0.join("bucket")).unwrap();
    for index in 0..600 {
        fs::write(tree.0.join(format!("bucket/{index:04}")), b"body").unwrap();
    }
    let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
    let mut scan = TreeScan::new();
    let mut paths = BTreeSet::new();
    let mut pages = 0;
    while !scan.is_empty() {
        let (next, page) = scan.page(&root).unwrap();
        scan = next;
        pages += 1;
        for entry in page {
            assert!(entry.error.is_none());
            if !entry.directory {
                assert!(paths.insert(entry.path));
            }
        }
    }
    assert_eq!(paths.len(), 600);
    assert_eq!(pages, 3);
    let (scan, _) = TreeScan::new().page(&root).unwrap();
    fs::rename(tree.0.join("bucket"), tree.0.join("old")).unwrap();
    fs::create_dir(tree.0.join("bucket")).unwrap();
    let (_, page) = scan.page(&root).unwrap();
    assert!(page.iter().any(|entry| entry.path == Path::new("bucket")
        && entry.error == Some(io::ErrorKind::ResourceBusy)));
}
