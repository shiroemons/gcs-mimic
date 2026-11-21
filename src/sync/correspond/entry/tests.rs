#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::*;
use crate::{sync::mapping::Obstruction, test_support::TestDir};
use std::fs;

/// A tree to observe paths in, through lookups that know nothing of its naming rules.
struct Tree {
    root: File,
    dir: TestDir,
}

impl Tree {
    fn new() -> Self {
        let dir = TestDir::new();
        fs::create_dir(dir.0.join("bucket")).unwrap();
        Self {
            root: fsutil::open_directory_nofollow(&dir.0).unwrap(),
            dir,
        }
    }

    fn observe(&self, path: &str) -> PathEntry {
        observe_path(
            &self.root,
            RootNaming::posix(),
            &Lookup::unprobed(),
            Path::new(path),
        )
    }
}

/// A regular file under its exact spelling is the file, with the directory that holds it; an
/// empty path tells whether only the entry or a directory above it is missing.
#[test]
fn a_path_holds_its_file_or_tells_how_far_down_it_is_there() {
    let tree = Tree::new();
    fs::write(tree.dir.0.join("bucket/file"), b"bytes").unwrap();
    assert!(matches!(
        tree.observe("bucket/file"),
        PathEntry::File { .. }
    ));
    assert!(matches!(
        tree.observe("bucket/missing"),
        PathEntry::Absent {
            above: Above::Present,
            ..
        }
    ));
    assert!(matches!(
        tree.observe("bucket/gone/file"),
        PathEntry::Absent {
            above: Above::Missing,
            ..
        }
    ));
    assert!(matches!(
        tree.observe("other/file"),
        PathEntry::Absent {
            above: Above::Missing,
            ..
        }
    ));
}

/// What holds the name in place of a regular file is told apart, and so is what holds the place
/// of a directory above it.
#[test]
fn another_kind_of_entry_at_the_path_or_above_it_is_told_apart() {
    let tree = Tree::new();
    fs::create_dir(tree.dir.0.join("bucket/directory")).unwrap();
    std::os::unix::fs::symlink("directory", tree.dir.0.join("bucket/link")).unwrap();
    fs::write(tree.dir.0.join("bucket/file"), b"bytes").unwrap();
    assert!(matches!(
        tree.observe("bucket/directory"),
        PathEntry::Directory(_)
    ));
    assert!(matches!(tree.observe("bucket/link"), PathEntry::Special(_)));
    assert!(matches!(
        tree.observe("bucket/file/below"),
        PathEntry::Unplaced(Unplaced::Obstructed(Obstruction::Collision))
    ));
    assert!(matches!(
        tree.observe("bucket/link/below"),
        PathEntry::Unplaced(Unplaced::Obstructed(Obstruction::UnsafeEntry))
    ));
    assert!(matches!(
        tree.observe("bucket/../file"),
        PathEntry::Unplaced(Unplaced::Unmapped(LexicalReason::InvalidName))
    ));
    assert!(matches!(
        tree.observe(&format!("bucket/{}", "n".repeat(4096))),
        PathEntry::NameRejected(_)
    ));
}

/// A name the file system refuses (`EINVAL` or `EILSEQ`) is rejected, whatever entry it holds:
/// the refusal is not taken for the kind of entry there.
#[test]
fn a_name_the_file_system_refuses_is_rejected_whatever_it_holds() {
    let tree = Tree::new();
    let bucket = tree.dir.0.join("bucket");
    fs::write(bucket.join("file"), b"bytes").unwrap();
    fs::create_dir(bucket.join("directory")).unwrap();
    for errno in [libc::EINVAL, libc::EILSEQ] {
        for name in ["file", "directory"] {
            let _refused = fsutil::FailingOpens::refusing(&bucket, name, errno);
            let observed = tree.observe(&format!("bucket/{name}"));
            assert!(
                matches!(&observed, PathEntry::NameRejected(error) if error.raw_os_error() == Some(errno)),
                "{errno} {name}: {observed:?}"
            );
        }
    }
}

/// A path whose file, or a directory above it, keeps failing to open while its name still holds
/// an entry is transient, never absent.
#[test]
fn a_path_whose_entries_keep_failing_to_open_is_never_absent() {
    let tree = Tree::new();
    fs::write(tree.dir.0.join("bucket/file"), b"bytes").unwrap();
    let clock = std::sync::Arc::new(crate::clock::ManualClock::new(std::time::SystemTime::now()));
    let lookup = Lookup::probed(fsutil::Matching::Exact, clock);
    let observe =
        |path: &str| observe_path(&tree.root, RootNaming::posix(), &lookup, Path::new(path));
    let file = fsutil::FailingOpens::vanishing(&tree.dir.0.join("bucket"), "file", usize::MAX);
    assert!(matches!(observe("bucket/file"), PathEntry::Transient));
    drop(file);
    let _bucket = fsutil::FailingOpens::vanishing(&tree.dir.0, "bucket", usize::MAX);
    assert!(matches!(observe("bucket/file"), PathEntry::Transient));
}

/// A listing that the lookups kept from before a change that left the directory's timestamps as
/// they were does not make a file under its exact spelling a respelled one: the listing read to
/// name the spelling decides.
#[test]
fn a_kept_listing_older_than_the_directory_leaves_a_file_its_spelling() {
    let tree = Tree::new();
    let bucket = tree.dir.0.join("bucket");
    fs::write(bucket.join("Actual"), b"bytes").unwrap();
    let _unchecked = fsutil::UncheckedSpelling::of(&bucket);
    let lookup = Lookup::probed(
        fsutil::Matching::Folding,
        std::sync::Arc::new(crate::clock::SystemClock),
    );
    lookup
        .keep_listing(&File::open(&bucket).unwrap(), &["actual"])
        .unwrap();
    assert!(matches!(
        observe_path(
            &tree.root,
            RootNaming::posix(),
            &lookup,
            Path::new("bucket/Actual")
        ),
        PathEntry::File { .. }
    ));
}

/// A name the file system reaches under another letter case names the spelling the directory
/// lists the file under, or no spelling when another name links the file too. A file system
/// that keeps letter case apart reaches nothing under the other spelling.
#[test]
fn a_file_reached_under_another_spelling_names_the_listed_one() {
    let tree = Tree::new();
    fs::write(tree.dir.0.join("bucket/Actual"), b"bytes").unwrap();
    if !tree.dir.0.join("bucket/actual").exists() {
        assert!(matches!(
            tree.observe("bucket/actual"),
            PathEntry::Absent {
                above: Above::Present,
                ..
            }
        ));
        return;
    }
    assert!(matches!(
        tree.observe("bucket/actual"),
        PathEntry::Respelled(ListedSpelling::Listed(spelling)) if spelling == "Actual"
    ));
    fs::hard_link(
        tree.dir.0.join("bucket/Actual"),
        tree.dir.0.join("bucket/Linked"),
    )
    .unwrap();
    assert!(matches!(
        tree.observe("bucket/actual"),
        PathEntry::Respelled(ListedSpelling::Unnamed)
    ));
}

/// A file reached under another letter case whose directory then fails to list is respelled with
/// no spelling to name, not a failed read: a pass meets an alias there, as it did before it read
/// the spelling, and reads the path again rather than failing.
#[test]
fn a_file_whose_spelling_cannot_be_listed_is_respelled_without_one() {
    let tree = Tree::new();
    let bucket = tree.dir.0.join("bucket");
    fs::write(bucket.join("Actual"), b"bytes").unwrap();
    if !bucket.join("actual").exists() {
        return;
    }
    let _unreadable = fsutil::UnreadableListing::of(&bucket);
    assert!(matches!(
        tree.observe("bucket/Actual"),
        PathEntry::File { .. }
    ));
    let observed = tree.observe("bucket/actual");
    assert!(
        matches!(observed, PathEntry::Respelled(ListedSpelling::Unnamed)),
        "{observed:?}"
    );
}
