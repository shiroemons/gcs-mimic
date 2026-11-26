use super::*;
use crate::test_support::TestDir;
use std::fs;

/// A fixture tree holding the bucket directory `bucket` with the file `a`, and the tree that
/// reads it, positioned as a walk is below `bucket` once it listed it.
fn tree() -> (TestDir, Tree) {
    let dir = TestDir::new();
    let root = dir.0.join("root");
    fs::create_dir_all(root.join("bucket")).unwrap();
    fs::write(root.join("bucket/a"), b"a").unwrap();
    let root = fs::canonicalize(root).unwrap();
    #[cfg(unix)]
    let tree = {
        // A lookup's bounded wait passes at once on this clock, so it takes no real time.
        let mut tree = Tree::new(
            root.clone(),
            std::sync::Arc::new(File::open(&root).unwrap()),
            std::sync::Arc::new(crate::clock::ManualClock::new(std::time::SystemTime::now())),
        );
        tree.listed.push(NodeId::from(
            &fs::symlink_metadata(root.join("bucket")).unwrap(),
        ));
        tree
    };
    #[cfg(not(unix))]
    let tree = Tree::new(root);
    (dir, tree)
}

/// What the walk makes of the entry `relative`, which it listed but could not inspect.
fn unread(tree: &mut Tree, relative: &str, error: io::ErrorKind) -> Fixture {
    tree.see(Err(WalkError::Entry {
        path: tree.root.join(relative),
        error: error.into(),
    }))
    .unwrap()
    .fixture
}

/// What the walk makes of the top-level entry `name`, which it inspected.
fn top_level(tree: &mut Tree, name: &str) -> Fixture {
    let path = tree.root.join(name);
    let metadata = fs::symlink_metadata(&path).unwrap();
    tree.see(Ok(WalkEntry {
        path,
        metadata,
        depth: 0,
    }))
    .unwrap()
    .fixture
}

/// A top-level directory names its bucket only by a valid bucket name; one with any other name
/// is unusable as the walk finds it.
#[test]
fn a_top_level_directory_names_a_bucket_only_by_a_valid_bucket_name() {
    let (_dir, mut tree) = tree();
    fs::create_dir(tree.root.join("Not_A_Bucket")).unwrap();
    assert!(matches!(
        top_level(&mut tree, "bucket"),
        Fixture::Bucket(name) if name.as_str() == "bucket"
    ));
    assert!(matches!(
        top_level(&mut tree, "Not_A_Bucket"),
        Fixture::Unusable(Rejected::BucketName(_))
    ));
}

fn unknown_with(fixture: &Fixture, kind: io::ErrorKind) -> bool {
    matches!(fixture, Fixture::Unknown(Unknown::Io(error)) if error.kind() == kind)
}

/// An entry the walk listed and then found nothing under is absent only once a lookup proves its
/// name empty, as it does for a name below a directory that is gone. A name that still holds an
/// entry, and one whose inspection failed in another way, are unknown.
#[test]
fn a_listed_entry_is_absent_only_when_a_lookup_proves_its_name_empty() {
    let (_dir, mut tree) = tree();
    #[cfg(unix)]
    for relative in ["bucket/gone", "gone/a"] {
        let fixture = unread(&mut tree, relative, io::ErrorKind::NotFound);
        assert!(matches!(fixture, Fixture::Absent(_)), "{relative}");
    }
    // Nothing on this platform proves a name absent.
    #[cfg(not(unix))]
    assert!(unknown_with(
        &unread(&mut tree, "bucket/gone", io::ErrorKind::NotFound),
        io::ErrorKind::NotFound
    ));
    assert!(unknown_with(
        &unread(&mut tree, "bucket/a", io::ErrorKind::NotFound),
        io::ErrorKind::NotFound
    ));
    assert!(unknown_with(
        &unread(&mut tree, "bucket/gone", io::ErrorKind::PermissionDenied),
        io::ErrorKind::PermissionDenied
    ));
}

/// A lookup below a directory that another replaced after the walk listed it proves nothing
/// about the names the walk listed there, so a name it finds empty is unknown, never absent,
/// and so is a file it opens there.
#[cfg(unix)]
#[test]
fn a_name_below_a_directory_replaced_after_the_listing_is_unknown() {
    let (dir, mut tree) = tree();
    fs::rename(tree.root.join("bucket"), dir.0.join("listed")).unwrap();
    fs::create_dir(tree.root.join("bucket")).unwrap();
    fs::write(tree.root.join("bucket/a"), b"other").unwrap();
    for relative in ["bucket/gone", "bucket/a"] {
        assert!(
            matches!(
                tree.open(Path::new(relative)),
                Source::Unknown(Unknown::Replaced)
            ),
            "{relative}"
        );
    }
    assert!(matches!(
        unread(&mut tree, "bucket/gone", io::ErrorKind::NotFound),
        Fixture::Unknown(Unknown::Io(_))
    ));
}

/// The walk lists the root by its path, so it reads the pinned root only while that path names
/// it: a root replaced at its path is unknown.
#[cfg(unix)]
#[test]
fn a_root_replaced_at_its_path_is_unknown() {
    let (dir, tree) = tree();
    assert!(tree.check_root().is_ok());
    fs::rename(&tree.root, dir.0.join("pinned")).unwrap();
    fs::create_dir(&tree.root).unwrap();
    std::assert_matches!(tree.check_root(), Err(Unknown::Replaced));
}

/// The file the walk opened, with the fingerprint of the handle it reads through.
#[cfg(unix)]
fn opened(source: Source) -> Opened {
    match source {
        Source::File(opened) => *opened,
        Source::Absent(_) | Source::Unknown(_) => panic!("the fixture file opens"),
    }
}

/// A fixture file is fingerprinted through the handle it is read from. Once a lookup proves its
/// name, or a directory above it, empty, it is absent; a file that cannot be read is unknown.
#[cfg(unix)]
#[test]
fn a_fixture_file_opens_or_is_absent_only_where_a_lookup_proves_it() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, tree) = tree();
    let file = opened(tree.open(Path::new("bucket/a")));
    let path = File::open(tree.root.join("bucket/a")).unwrap();
    assert_eq!(
        file.fingerprint.identity(),
        crate::fsutil::uncached_identity(&path).unwrap()
    );
    assert!(file.metadata.is_file());
    for relative in ["bucket/gone", "gone/a"] {
        assert!(
            matches!(tree.open(Path::new(relative)), Source::Absent(_)),
            "{relative}"
        );
    }
    let locked = tree.root.join("bucket/a");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&locked).is_ok() {
        eprintln!("SKIP: a mode 000 file stays readable (for example when running as root)");
        return;
    }
    let source = tree.open(Path::new("bucket/a"));
    assert!(
        matches!(&source, Source::Unknown(Unknown::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied)
    );
}

/// A fixture file whose open fails with `ENOENT` for a moment while `fstatat` still finds it is
/// opened again within the lookup's bounded wait. One that keeps failing is unknown once the
/// wait runs out, never absent.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_fixture_file_that_fails_to_open_is_opened_again_until_the_wait_runs_out() {
    let (_dir, tree) = tree();
    let bucket = tree.root.join("bucket");
    let settling = crate::fsutil::FailingOpens::vanishing(&bucket, "a", 3);
    assert_eq!(
        opened(tree.open(Path::new("bucket/a")))
            .file
            .metadata()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(settling.left(), 0);
    drop(settling);
    let _vanishing = crate::fsutil::FailingOpens::vanishing(&bucket, "a", usize::MAX);
    assert!(matches!(
        tree.open(Path::new("bucket/a")),
        Source::Unknown(Unknown::Unopenable)
    ));
}

/// What one walked entry is, by name.
#[cfg(unix)]
fn kind(fixture: &Fixture) -> &'static str {
    match fixture {
        Fixture::Unknown(_) => "unknown",
        Fixture::Absent(_) => "absent",
        Fixture::Unusable(_) => "unusable",
        Fixture::OsMetadata => "os metadata",
        Fixture::TopLevelFile => "top-level file",
        Fixture::Bucket(_) => "bucket",
        Fixture::Directory => "directory",
        Fixture::Object(object) => match object.source {
            Source::File(_) => "file",
            Source::Absent(_) => "absent file",
            Source::Unknown(_) => "unknown file",
        },
    }
}

/// A directory the walk cannot list is unknown, never absent, and the walk goes on past it to
/// the end of the tree.
#[cfg(unix)]
#[test]
fn a_directory_the_walk_cannot_list_is_unknown_and_the_walk_goes_on() {
    use std::os::unix::fs::PermissionsExt;
    let (_dir, tree) = tree();
    let locked = tree.root.join("bucket/locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("hidden"), b"").unwrap();
    fs::write(tree.root.join("bucket/z"), b"z").unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked).is_ok() {
        eprintln!("SKIP: a mode 000 directory stays listable (for example when running as root)");
        return;
    }
    let mut walk = tree.walk(TreeLimits::default()).unwrap();
    let mut seen = Vec::new();
    loop {
        match walk.blocking_recv() {
            Some(Walked::Entry(entry)) => {
                let Seen { label, fixture } = (*entry).unwrap();
                seen.push((label, kind(&fixture)));
            }
            Some(Walked::Done) => break,
            Some(Walked::Unlisted(_)) => {
                panic!("the walk had no recorded name to look up")
            }
            None => panic!("the walk stopped before it finished"),
        }
    }
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    let expected = [
        ("bucket", "bucket"),
        ("bucket/a", "file"),
        ("bucket/locked", "unknown"),
        ("bucket/z", "file"),
    ];
    assert_eq!(seen, expected.map(|(label, kind)| (label.to_owned(), kind)));
}
