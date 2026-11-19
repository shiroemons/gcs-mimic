#![expect(
    clippy::unwrap_used,
    reason = "platform-gated fixtures fail at the operation under test"
)]

use super::*;
use crate::test_support::TestDir;
use std::fs;

/// The names a range read takes in are exactly those below the name: never the name itself and
/// never a sibling that only starts with it, whatever byte follows.
#[test]
fn the_range_below_a_name_takes_in_no_sibling() {
    let (first, last) = below("a");
    let within = |name: &str| first.as_str() <= name && name < last.as_str();
    for inside in ["a/", "a/x", "a/x/y", "a/\u{10ffff}"] {
        assert!(within(inside), "{inside}");
    }
    for outside in ["a", "a.txt", "a-b", "a0", "ab", "a\u{10ffff}", "b/a/x", ""] {
        assert!(!within(outside), "{outside}");
    }
}

/// Proves `path` under the directory `parent` of `tree`.
fn prove(tree: &TestDir, parent: &str, path: &str) -> Result<RemovableDirectory, Kept> {
    RemovableDirectory::prove(
        File::open(tree.0.join(parent)).unwrap(),
        RootNaming::posix(),
        Unreferenced { path: path.into() },
    )
    .unwrap()
}

/// A bucket's own directory, and a directory below the names the root reserves, is no object's
/// directory, so no proof names one, however empty it is.
#[test]
fn no_proof_names_a_bucket_directory_or_a_reserved_one() {
    let tree = TestDir::new();
    fs::create_dir_all(tree.0.join("test-bucket")).unwrap();
    fs::create_dir_all(tree.0.join(mapping::WORKSPACE).join("empty")).unwrap();
    assert_eq!(
        prove(&tree, "", "test-bucket").err(),
        Some(Kept::NotAnObject)
    );
    let reserved = format!("{}/empty", mapping::WORKSPACE);
    assert_eq!(
        prove(&tree, mapping::WORKSPACE, &reserved).err(),
        Some(Kept::NotAnObject)
    );
    assert!(tree.0.join("test-bucket").is_dir());
    assert!(tree.0.join(mapping::WORKSPACE).join("empty").is_dir());
}

/// A proof names only a directory reached under the target's exact spelling without following a
/// symlink: a symlink to a directory, a dangling one, a file and another spelling of a directory
/// all stay out of it.
#[test]
fn no_proof_names_a_symlink_a_file_or_another_spelling() {
    let tree = TestDir::new();
    let bucket = tree.0.join("test-bucket");
    fs::create_dir_all(bucket.join("pointed")).unwrap();
    fs::create_dir(bucket.join("Cased")).unwrap();
    fs::create_dir(bucket.join("empty")).unwrap();
    std::os::unix::fs::symlink(bucket.join("pointed"), bucket.join("link")).unwrap();
    std::os::unix::fs::symlink(bucket.join("gone"), bucket.join("dangling")).unwrap();
    fs::write(bucket.join("file"), b"bytes").unwrap();
    for name in ["link", "dangling", "file", "cased", "missing"] {
        let path = format!("test-bucket/{name}");
        assert_eq!(
            prove(&tree, "test-bucket", &path).err(),
            Some(Kept::NotDirectory),
            "{name}"
        );
    }
    let proof = prove(&tree, "test-bucket", "test-bucket/empty").unwrap();
    assert_eq!(proof.name, "empty");
    assert!(bucket.join("link").is_symlink());
    assert!(bucket.join("dangling").is_symlink());
    assert!(bucket.join("pointed").is_dir());
    assert!(bucket.join("Cased").is_dir());
}
