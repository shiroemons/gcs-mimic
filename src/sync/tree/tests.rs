use super::*;
use crate::test_support::TestDir;
use std::{collections::BTreeSet, fs, num::NonZeroUsize};

/// A scan reads a name the file system refuses as invalid (`EINVAL`) as an entry of a kind it
/// does not take in, as it reads a special file, and a name refused as such (`EILSEQ`) as a
/// failed read of the entry.
#[test]
fn a_scan_reads_an_invalid_name_as_another_kind_and_a_refused_name_as_a_failed_read() {
    for (errno, failed) in [(libc::EINVAL, false), (libc::EILSEQ, true)] {
        let tree = TestDir::new();
        fs::create_dir(tree.0.join("bucket")).unwrap();
        fs::write(tree.0.join("bucket/refused"), b"bytes").unwrap();
        let _refused = fsutil::FailingOpens::refusing(&tree.0.join("bucket"), "refused", errno);
        let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
        let mut scan = TreeScan::new(TreeLimits::default(), Lookup::unprobed());
        let mut refused = Vec::new();
        while !scan.is_empty() {
            let (next, page) = scan.page(&root).unwrap();
            scan = next;
            refused.extend(
                page.into_iter()
                    .filter(|entry| entry.path == Path::new("bucket/refused")),
            );
        }
        let [entry] = refused.as_slice() else {
            panic!("{errno}: {} entries", refused.len());
        };
        assert!(!entry.directory && entry.identity.is_none(), "{errno}");
        assert_eq!(
            entry.error,
            failed.then_some(EntryError::Io(EntryIo::Os(errno))),
            "{errno}"
        );
    }
}

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
            assert!(
                parent(
                    &root,
                    RootNaming::posix(),
                    &Lookup::unprobed(),
                    Path::new("bucket/nested"),
                    ParentOpen::Create
                )
                .is_ok()
            );
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
        parent(
            &root,
            RootNaming::posix(),
            &Lookup::unprobed(),
            Path::new("bucket"),
            ParentOpen::Create
        ),
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
        parent(
            &root,
            RootNaming::posix(),
            &Lookup::unprobed(),
            Path::new("bucket/nested"),
            ParentOpen::Create
        ),
        Err(ParentError::Io(_))
    ));
    assert!(
        parent(
            &root,
            RootNaming::posix(),
            &Lookup::unprobed(),
            Path::new("bucket/nested"),
            ParentOpen::Create
        )
        .is_ok()
    );
    assert!(snapshot.0.join("bucket/nested").is_dir());
    fault::arm(
        &tree.0,
        Step::SyncDirectorySync,
        0,
        Fault::Io(io::ErrorKind::PermissionDenied),
    );
    assert!(
        parent(
            &root,
            RootNaming::posix(),
            &Lookup::unprobed(),
            Path::new("bucket/nested"),
            ParentOpen::Existing
        )
        .is_ok()
    );
    assert!(matches!(
        parent(
            &root,
            RootNaming::posix(),
            &Lookup::unprobed(),
            Path::new("bucket/nested"),
            ParentOpen::Create
        ),
        Err(ParentError::Io(_))
    ));
}

#[test]
fn existing_parents_never_create_missing_directories_and_reject_aliases_and_symlinks() {
    let tree = TestDir::new();
    let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
    assert!(matches!(
        parent(
            &root,
            RootNaming::posix(),
            &Lookup::unprobed(),
            Path::new("bucket/nested"),
            ParentOpen::Existing
        ),
        Err(ParentError::Missing(_))
    ));
    assert!(!tree.0.join("bucket").exists());
    fs::create_dir(tree.0.join("Dir")).unwrap();
    if tree.0.join("dir").exists() {
        assert!(matches!(
            parent(
                &root,
                RootNaming::posix(),
                &Lookup::unprobed(),
                Path::new("dir"),
                ParentOpen::Existing
            ),
            Err(ParentError::Finding(Unplaced::Obstructed(
                Obstruction::Collision
            )))
        ));
    }
    std::os::unix::fs::symlink(tree.0.join("Dir"), tree.0.join("link")).unwrap();
    assert!(matches!(
        parent(
            &root,
            RootNaming::posix(),
            &Lookup::unprobed(),
            Path::new("link"),
            ParentOpen::Existing
        ),
        Err(ParentError::Finding(Unplaced::Obstructed(
            Obstruction::UnsafeEntry
        )))
    ));
    fs::write(tree.0.join("first"), b"hardlinked").unwrap();
    fs::hard_link(tree.0.join("first"), tree.0.join("second")).unwrap();
    for name in ["first", "second"] {
        assert!(matches!(
            Lookup::unprobed().regular(&root, name.as_ref()).unwrap(),
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
    let mut scan = TreeScan::new(TreeLimits::default(), Lookup::unprobed());
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
    let (scan, _) = TreeScan::new(TreeLimits::default(), Lookup::unprobed())
        .page(&root)
        .unwrap();
    fs::rename(tree.0.join("bucket"), tree.0.join("old")).unwrap();
    fs::create_dir(tree.0.join("bucket")).unwrap();
    let (_, page) = scan.page(&root).unwrap();
    assert!(page.iter().any(|entry| entry.path == Path::new("bucket")
        && entry.error == Some(EntryError::Io(io::ErrorKind::ResourceBusy.into()))));
}

/// A scan opens each entry under the name its directory just listed and confirms no spelling
/// for it: where the platform cannot report one, confirming read the directory again for each
/// entry. Only a directory of an earlier page is confirmed when a page reopens it, from a listing
/// of its parent that the pass reads once.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_scan_confirms_no_spelling_of_the_names_it_lists() {
    let tree = TestDir::new();
    fs::create_dir(tree.0.join("bucket")).unwrap();
    for index in 0..600 {
        fs::write(tree.0.join(format!("bucket/File-{index:04}")), b"body").unwrap();
    }
    let _root_listed = fsutil::UncheckedSpelling::of(&tree.0);
    let _bucket_listed = fsutil::UncheckedSpelling::of(&tree.0.join("bucket"));
    let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
    let clock = crate::clock::ManualClock::new(
        std::time::SystemTime::now() + std::time::Duration::from_secs(3600),
    );
    let lookup = Lookup::probed(fsutil::Matching::Folding, std::sync::Arc::new(clock));
    let mut scan = TreeScan::new(TreeLimits::default(), lookup.for_job());
    let mut files = 0;
    let mut pages = 0;
    while !scan.is_empty() {
        let (next, page) = scan.page(&root).unwrap();
        scan = next;
        pages += 1;
        files += page.iter().filter(|entry| entry.identity.is_some()).count();
    }
    assert_eq!((files, pages), (600, 3));
    assert_eq!(lookup.reads(), 1);
}

/// Walks a whole tree, also proving that no page holds more than its bound.
fn scan_tree(tree: &TestDir, limits: TreeLimits) -> Result<usize, AdminError> {
    let root = fsutil::open_directory_nofollow(&tree.0)?;
    let mut scan = TreeScan::new(limits, Lookup::unprobed());
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
        entries: NonZeroUsize::new(entries),
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

/// A directory past the former inspection cap of 10,000 names has to work without a limit, and has
/// to fail on its first page, not after every page a named limit admits, otherwise.
#[test]
fn one_directory_may_hold_as_many_entries_as_the_tree_limit_allows() {
    const FILES: usize = 10_050;
    let tree = TestDir::new();
    fs::create_dir(tree.0.join("bucket")).unwrap();
    for index in 0..FILES {
        fs::write(tree.0.join(format!("bucket/{index:05}")), b"").unwrap();
    }
    assert_eq!(scan_tree(&tree, TreeLimits::default()).unwrap(), FILES + 1);
    let named = |entries| TreeLimits {
        entries: NonZeroUsize::new(entries),
        ..TreeLimits::default()
    };
    let failure = reason(scan_tree(&tree, named(10_000)).unwrap_err());
    assert!(failure.contains("10000"), "{failure}");
    assert!(failure.contains("max_sync_entries"), "{failure}");
    assert_eq!(scan_tree(&tree, named(FILES + 1)).unwrap(), FILES + 1);
    let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
    let first_page = TreeScan::new(named(100), Lookup::unprobed()).page(&root);
    let failure = reason(first_page.err().unwrap());
    assert!(failure.contains("exceeds 100 inspected"), "{failure}");
}

#[test]
fn every_walker_applies_the_same_depth_limit() {
    let tree = TestDir::new();
    fs::create_dir_all(tree.0.join("bucket/one/two")).unwrap();
    fs::write(tree.0.join("bucket/one/two/file"), b"").unwrap();
    let limits = |depth| TreeLimits {
        depth: NonZeroUsize::new(depth),
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

/// The former default stopped at 128 levels. A scan holds one descriptor per level, so this stays
/// far below what the descriptor limit of a test process allows.
#[test]
fn no_depth_limit_accepts_what_export_can_create_past_the_former_default() {
    const LEVELS: usize = 129;
    let tree = TestDir::new();
    let path = std::iter::repeat_n("d", LEVELS).collect::<PathBuf>();
    let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
    assert!(
        parent(
            &root,
            RootNaming::posix(),
            &Lookup::unprobed(),
            &path,
            ParentOpen::Create
        )
        .is_ok()
    );
    assert!(TreeLimits::default().admits_file(&path));
    assert_eq!(scan_tree(&tree, TreeLimits::default()).unwrap(), LEVELS);
    let named = TreeLimits {
        depth: NonZeroUsize::new(LEVELS - 1),
        ..TreeLimits::default()
    };
    let failure = reason(scan_tree(&tree, named).unwrap_err());
    assert!(failure.contains("128 levels"), "{failure}");
    assert!(failure.contains("max_sync_depth"), "{failure}");
}

/// Descriptors, not a limit of the operator's, are what `MAX_TREE_DEPTH` protects without one.
#[test]
fn no_depth_limit_still_bounds_the_descriptors_a_walk_holds() {
    let limits = TreeLimits::default();
    assert_eq!(limits.deepest_directory(), crate::config::MAX_TREE_DEPTH);
    let AdminError::Invalid(failure) = limits.too_deep() else {
        panic!("a tree past every name is invalid input");
    };
    assert!(failure.contains("1024 levels"), "{failure}");
    assert!(!failure.contains("max_sync_depth"), "{failure}");
}

#[test]
fn parent_walks_past_the_former_128_levels_and_refuses_only_what_no_limit_allows() {
    let tree = TestDir::new();
    let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
    let deep = std::iter::repeat_n("d", 200).collect::<PathBuf>();
    assert!(
        parent(
            &root,
            RootNaming::posix(),
            &Lookup::unprobed(),
            &deep,
            ParentOpen::Create
        )
        .is_ok()
    );
    assert!(
        parent(
            &root,
            RootNaming::posix(),
            &Lookup::unprobed(),
            &deep,
            ParentOpen::Existing
        )
        .is_ok()
    );
    let too_deep = std::iter::repeat_n("d", crate::config::MAX_TREE_DEPTH + 1).collect::<PathBuf>();
    for open in [ParentOpen::Existing, ParentOpen::Create] {
        assert!(matches!(
            parent(
                &root,
                RootNaming::posix(),
                &Lookup::unprobed(),
                &too_deep,
                open
            ),
            Err(ParentError::Finding(Unplaced::Unmapped(
                LexicalReason::HostRestriction
            )))
        ));
    }
}

#[test]
fn parent_refuses_what_the_root_rules_refuse_before_creating_any_level() {
    use crate::sync::naming::{Case, Colon, NameMax, Normalization, Observation, Spelling};
    let rules = |colon, name_max| {
        RootNaming::try_from(Observation {
            colon,
            case: Case::Sensitive,
            normalization: Normalization::Sensitive,
            spelling: Spelling::Preserved,
            name_max: NameMax::from(Some(name_max)),
        })
        .unwrap()
    };
    let tree = TestDir::new();
    let root = fsutil::open_directory_nofollow(&tree.0).unwrap();
    for (naming, path) in [
        (rules(Colon::Rejected, 255), "bucket/a:b"),
        (rules(Colon::Accepted, 8), "bucket/123456789"),
    ] {
        for open in [ParentOpen::Existing, ParentOpen::Create] {
            assert!(matches!(
                parent(&root, naming, &Lookup::unprobed(), Path::new(path), open),
                Err(ParentError::Finding(Unplaced::Unmapped(
                    LexicalReason::HostRestriction
                )))
            ));
        }
        assert!(!tree.0.join("bucket").exists(), "{path}");
    }
    assert!(
        parent(
            &root,
            rules(Colon::Accepted, 255),
            &Lookup::unprobed(),
            Path::new("bucket/a:b"),
            ParentOpen::Create
        )
        .is_ok()
    );
    assert!(tree.0.join("bucket/a:b").is_dir());
}
