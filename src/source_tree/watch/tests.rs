use super::*;
use crate::fsutil;
use crate::test_support::TestDir;
use std::{fs, time::Duration};

#[tokio::test]
async fn native_watch_covers_more_than_a_thousand_files_without_per_file_descriptors() {
    let root = TestDir::new();
    fs::create_dir(root.0.join("bucket")).unwrap();
    for index in 0..1100 {
        fs::write(root.0.join(format!("bucket/body-{index}")), b"initial").unwrap();
    }
    let directory = fsutil::open_directory_nofollow(&root.0).unwrap();
    let watcher = Watch::new(
        build(
            &directory,
            &root.0.canonicalize().unwrap(),
            TreeLimits::default(),
        )
        .unwrap(),
    )
    .unwrap();
    fs::write(root.0.join("bucket/body-1099"), b"in-place change").unwrap();
    tokio::time::timeout(Duration::from_secs(3), watcher.changed())
        .await
        .unwrap()
        .unwrap();
}

/// Asserts that an idle tree stops producing changes. FSEvents can still deliver
/// writes made just before a stream started ("since now" is not a hard cut), so
/// such stragglers are drained first; a watcher that polls never stays quiet for
/// the window and fails at the deadline.
async fn assert_idle(watcher: &Watch) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        match tokio::time::timeout(Duration::from_millis(500), watcher.changed()).await {
            Err(_) => return,
            Ok(Err(error)) => panic!("watch failed: {error}"),
            Ok(Ok(_)) => assert!(
                tokio::time::Instant::now() < deadline,
                "an idle tree kept producing changes"
            ),
        }
    }
}

#[tokio::test]
async fn native_watch_observes_nested_writes_atomic_replacement_and_directory_changes_without_idle_polling()
 {
    let root = TestDir::new();
    fs::create_dir_all(root.0.join("bucket/nested")).unwrap();
    let body = root.0.join("bucket/nested/body");
    fs::write(&body, b"initial").unwrap();
    let directory = fsutil::open_directory_nofollow(&root.0).unwrap();
    let mut watcher = Watch::new(
        build(
            &directory,
            &root.0.canonicalize().unwrap(),
            TreeLimits::default(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_idle(&watcher).await;
    for operation in ["write", "replace", "new-directory", "delete"] {
        match operation {
            "write" => fs::write(&body, b"changed").unwrap(),
            "replace" => {
                let replacement = root.0.join("bucket/nested/new");
                fs::write(&replacement, b"replacement").unwrap();
                fs::rename(replacement, &body).unwrap();
            }
            "new-directory" => fs::create_dir(root.0.join("bucket/added")).unwrap(),
            "delete" => fs::remove_file(&body).unwrap(),
            _ => unreachable!(),
        }
        tokio::time::timeout(Duration::from_secs(3), watcher.changed())
            .await
            .unwrap()
            .unwrap();
        watcher = Watch::new(
            build(
                &directory,
                &root.0.canonicalize().unwrap(),
                TreeLimits::default(),
            )
            .unwrap(),
        )
        .unwrap();
        // A straggler from this operation must not satisfy the next one's check.
        assert_idle(&watcher).await;
    }
}

#[tokio::test]
async fn native_watch_does_not_follow_symlinks_and_refuses_unobserved_capacity() {
    let root = TestDir::new();
    let outside = TestDir::new();
    fs::write(outside.0.join("body"), b"outside").unwrap();
    std::os::unix::fs::symlink(&outside.0, root.0.join("link")).unwrap();
    let directory = fsutil::open_directory_nofollow(&root.0).unwrap();
    let watcher = Watch::new(
        build(
            &directory,
            &root.0.canonicalize().unwrap(),
            TreeLimits::default(),
        )
        .unwrap(),
    )
    .unwrap();
    // Drain FSEvents stragglers from the setup first, so a change seen after the
    // outside write can only come from that write.
    assert_idle(&watcher).await;
    fs::write(outside.0.join("body"), b"unobserved outside").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(500), watcher.changed())
            .await
            .is_err()
    );
    #[cfg(target_os = "linux")]
    {
        let mut full = inotify::Native::new(1).unwrap();
        full.register(&directory).unwrap();
        let failure = full.register(&directory).unwrap_err().to_string();
        assert!(failure.contains("1 directories"), "{failure}");
        assert!(failure.contains("max_sync_entries"), "{failure}");
    }
}

fn built(root: &TestDir, limits: TreeLimits) -> io::Result<Native> {
    let directory = fsutil::open_directory_nofollow(&root.0)?;
    build(&directory, &root.0.canonicalize()?, limits)
}

/// Linux used to stop at 1022 watched directories whatever the tree limit allowed, so a tree the
/// scans accepted could not start. A watch holds no descriptor, so only the tree limit bounds it.
#[test]
fn a_watch_follows_the_tree_limit_instead_of_a_fixed_directory_count() {
    const DIRECTORIES: usize = 1100;
    let root = TestDir::new();
    fs::create_dir(root.0.join("bucket")).unwrap();
    for index in 1..DIRECTORIES {
        fs::create_dir(root.0.join(format!("bucket/{index:04}"))).unwrap();
    }
    built(&root, TreeLimits::default()).unwrap();
    let exact = TreeLimits {
        entries: DIRECTORIES,
        ..TreeLimits::default()
    };
    built(&root, exact).unwrap();
    let short = TreeLimits {
        entries: DIRECTORIES - 1,
        ..TreeLimits::default()
    };
    let failure = built(&root, short).err().unwrap().to_string();
    assert!(failure.contains("max_sync_entries"), "{failure}");
}

#[test]
fn a_watch_applies_the_depth_limit_inclusively_and_names_the_setting() {
    let root = TestDir::new();
    fs::create_dir_all(root.0.join("one/two/three")).unwrap();
    let limits = |depth| TreeLimits {
        depth,
        ..TreeLimits::default()
    };
    built(&root, limits(3)).unwrap();
    let failure = built(&root, limits(2)).err().unwrap().to_string();
    assert!(failure.contains("2 levels"), "{failure}");
    assert!(failure.contains("max_sync_depth"), "{failure}");
}

/// One directory larger than the former 10,000-name inspection cap must work under a raised limit.
#[test]
fn a_watch_inspects_a_directory_as_large_as_the_tree_limit() {
    const FILES: usize = 10_050;
    let root = TestDir::new();
    fs::create_dir(root.0.join("bucket")).unwrap();
    for index in 0..FILES {
        fs::write(root.0.join(format!("bucket/{index:05}")), b"").unwrap();
    }
    let failure = built(&root, TreeLimits::default())
        .err()
        .unwrap()
        .to_string();
    assert!(failure.contains("max_sync_entries"), "{failure}");
    let raised = TreeLimits {
        entries: FILES + 1,
        ..TreeLimits::default()
    };
    built(&root, raised).unwrap();
}
