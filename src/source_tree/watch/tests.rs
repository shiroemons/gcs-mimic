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
    let watcher = Watch::new(build(&directory, &root.0.canonicalize().unwrap()).unwrap()).unwrap();
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
    let mut watcher =
        Watch::new(build(&directory, &root.0.canonicalize().unwrap()).unwrap()).unwrap();
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
        watcher = Watch::new(build(&directory, &root.0.canonicalize().unwrap()).unwrap()).unwrap();
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
    let watcher = Watch::new(build(&directory, &root.0.canonicalize().unwrap()).unwrap()).unwrap();
    fs::write(outside.0.join("body"), b"unobserved outside").unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), watcher.changed())
            .await
            .is_err()
    );
    #[cfg(target_os = "linux")]
    {
        let mut full = inotify::Native::new().unwrap();
        full.count = inotify::MAX_WATCHES;
        assert!(
            full.register(&directory)
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
    }
}
