use super::*;
use crate::{sync::naming::RootNaming, test_support::TestDir};
use std::fs;

/// Every name a directory may hold and still count as empty is one that scans skip as an
/// ignored file wherever it lies, so removing it with a directory loses nothing the tree would
/// publish as an object.
#[test]
fn every_finder_metadata_name_is_one_scans_ignore() {
    use crate::sync::entries::{EntryClass, Skip, classify};
    let identity = fsutil::FileIdentity {
        device: 1,
        inode: 1,
        size: 0,
        modified_ns: 0,
        changed_ns: 0,
    };
    for name in FINDER_METADATA {
        assert!(crate::source_tree::is_ignored_regular_file(name), "{name}");
        let entry = crate::sync::tree::TreeEntry {
            path: std::path::Path::new("test-bucket/a").join(name),
            identity: Some(identity),
            directory: false,
            error: None,
        };
        assert_eq!(
            classify(&entry, RootNaming::posix()),
            EntryClass::Skipped(Skip::IgnoredName),
            "{name}"
        );
    }
}

/// A directory with no entry, or with only a regular `.DS_Store` and its `._.DS_Store`, is empty;
/// a `.DS_Store` that is a directory or a symlink, another AppleDouble file, and more names than
/// are read each make it hold something.
#[test]
fn only_nothing_or_regular_finder_metadata_counts_as_empty() {
    let contents_of = |setup: &dyn Fn(&std::path::Path)| {
        let tree = TestDir::new();
        setup(&tree.0);
        contents(&File::open(&tree.0).unwrap()).unwrap()
    };
    assert!(matches!(contents_of(&|_| {}), Contents::Empty(found) if found.is_empty()));
    let finder = contents_of(&|path| fs::write(path.join(".DS_Store"), b"finder").unwrap());
    assert!(
        matches!(&finder, Contents::Empty(found) if found.len() == 1 && found[0].name == ".DS_Store")
    );
    let both = contents_of(&|path| {
        fs::write(path.join(".DS_Store"), b"finder").unwrap();
        fs::write(path.join("._.DS_Store"), b"apple double").unwrap();
    });
    let Contents::Empty(found) = both else {
        panic!("a .DS_Store and its ._.DS_Store hold something")
    };
    let mut names = found.iter().map(|file| file.name).collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, [".DS_Store", "._.DS_Store"]);
    let held: [&dyn Fn(&std::path::Path); 4] = [
        &|path| fs::create_dir(path.join(".DS_Store")).unwrap(),
        &|path| std::os::unix::fs::symlink("elsewhere", path.join(".DS_Store")).unwrap(),
        &|path| fs::write(path.join("._x"), b"apple double").unwrap(),
        &|path| {
            fs::write(path.join(".DS_Store"), b"finder").unwrap();
            for index in 0..LIST_LIMIT {
                fs::write(path.join(format!("f{index}")), b"").unwrap();
            }
        },
    ];
    for setup in held {
        assert!(matches!(contents_of(setup), Contents::Holds));
    }
}
