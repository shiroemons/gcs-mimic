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
    let mut scan = TreeScan::new(TreeLimits::default());
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
    let (scan, _) = TreeScan::new(TreeLimits::default()).page(&root).unwrap();
    fs::rename(tree.0.join("bucket"), tree.0.join("old")).unwrap();
    fs::create_dir(tree.0.join("bucket")).unwrap();
    let (_, page) = scan.page(&root).unwrap();
    assert!(page.iter().any(|entry| entry.path == Path::new("bucket")
        && entry.error == Some(io::ErrorKind::ResourceBusy)));
}

/// Walks a whole tree, also proving that no page holds more than its bound.
fn scan_tree(tree: &TestDir, limits: TreeLimits) -> Result<usize, AdminError> {
    let root = fsutil::open_directory_nofollow(&tree.0)?;
    let mut scan = TreeScan::new(limits);
    let mut entries = 0;
    while !scan.is_empty() {
        let (next, page) = scan.page(&root)?;
        assert!(page.len() <= 256);
        entries += page.iter().filter(|entry| entry.error.is_none()).count();
        scan = next;
    }
    Ok(entries)
}

fn reason(error: AdminError) -> String {
    match error {
        AdminError::Invalid(reason) => reason,
        error => panic!("a tree limit is invalid input: {error}"),
    }
}

#[test]
fn the_entry_limit_is_inclusive_and_reserved_root_entries_take_no_place() {
    let tree = TestDir::new();
    fs::create_dir(tree.0.join("bucket")).unwrap();
    for name in ["a", "b"] {
        fs::write(tree.0.join("bucket").join(name), b"").unwrap();
    }
    for name in [
        super::super::mapping::MARKER,
        super::super::mapping::MARKER_STAGING,
        super::super::mapping::WORKSPACE,
    ] {
        fs::write(tree.0.join(name), b"").unwrap();
    }
    let limits = |entries| TreeLimits {
        entries,
        ..TreeLimits::default()
    };
    assert_eq!(scan_tree(&tree, limits(3)).unwrap(), 3);
    let failure = reason(scan_tree(&tree, limits(2)).unwrap_err());
    assert!(failure.contains("exceeds 2 inspected entries"), "{failure}");
    assert!(failure.contains("max_sync_entries"), "{failure}");
    // Only the bucket fits, and the root lists it among the reserved names without failing there.
    let failure = reason(scan_tree(&tree, limits(1)).unwrap_err());
    assert!(failure.contains("exceeds 1 inspected entries"), "{failure}");
}

/// A directory past the former inspection cap of 10,000 names has to work once the limit allows
/// it, and has to fail on its first page, not after every page the limit admits, otherwise.
#[test]
fn one_directory_may_hold_as_many_entries_as_the_tree_limit_allows() {
    const FILES: usize = 10_050;
    let tree = TestDir::new();
    fs::create_dir(tree.0.join("bucket")).unwrap();
    for index in 0..FILES {
        fs::write(tree.0.join(format!("bucket/{index:05}")), b"").unwrap();
    }
    let failure = reason(scan_tree(&tree, TreeLimits::default()).unwrap_err());
    assert!(failure.contains("10000"), "{failure}");
    assert!(failure.contains("max_sync_entries"), "{failure}");
    let raised = TreeLimits {
        entries: FILES + 1,
        ..TreeLimits::default()
    };
    assert_eq!(scan_tree(&tree, raised).unwrap(), FILES + 1);
    let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
    let first_page = TreeScan::new(TreeLimits {
        entries: 100,
        ..TreeLimits::default()
    })
    .page(&root);
    let failure = reason(first_page.err().unwrap());
    assert!(failure.contains("exceeds 100 inspected"), "{failure}");
}

#[test]
fn every_walker_applies_the_same_depth_limit() {
    let tree = TestDir::new();
    fs::create_dir_all(tree.0.join("bucket/one/two")).unwrap();
    fs::write(tree.0.join("bucket/one/two/file"), b"").unwrap();
    let limits = |depth| TreeLimits {
        depth,
        ..TreeLimits::default()
    };
    let at_limit = limits(3);
    assert_eq!(scan_tree(&tree, at_limit).unwrap(), 4);
    assert!(at_limit.admits_file(Path::new("bucket/one/two/file")));
    assert!(!at_limit.admits_file(Path::new("bucket/one/two/three/file")));
    let failure = reason(scan_tree(&tree, limits(2)).unwrap_err());
    assert!(failure.contains("2 levels"), "{failure}");
    assert!(failure.contains("max_sync_depth"), "{failure}");
    assert!(!limits(2).admits_file(Path::new("bucket/one/two/file")));
}

#[test]
fn the_default_depth_accepts_exactly_what_export_can_create() {
    let levels = |count: usize| {
        let tree = TestDir::new();
        let path = std::iter::repeat_n("d", count).collect::<PathBuf>();
        let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
        assert!(parent(&root, &path, ParentOpen::Create).is_ok());
        assert!(TreeLimits::default().admits_file(&path));
        let walked = scan_tree(&tree, TreeLimits::default());
        (tree, walked)
    };
    let (_tree, walked) = levels(DEFAULT_DEPTH);
    assert_eq!(walked.unwrap(), DEFAULT_DEPTH);
    let (_tree, walked) = levels(DEFAULT_DEPTH + 1);
    let failure = reason(walked.unwrap_err());
    assert!(failure.contains("128 levels"), "{failure}");
}

#[test]
fn parent_walks_past_the_former_128_levels_and_refuses_only_what_no_limit_allows() {
    let tree = TestDir::new();
    let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
    let deep = std::iter::repeat_n("d", 200).collect::<PathBuf>();
    assert!(parent(&root, &deep, ParentOpen::Create).is_ok());
    assert!(parent(&root, &deep, ParentOpen::Existing).is_ok());
    let too_deep = std::iter::repeat_n("d", crate::config::MAX_TREE_DEPTH + 1).collect::<PathBuf>();
    for open in [ParentOpen::Existing, ParentOpen::Create] {
        assert!(matches!(
            parent(&root, &too_deep, open),
            Err(ParentError::Finding(Reason::HostRestriction))
        ));
    }
}
