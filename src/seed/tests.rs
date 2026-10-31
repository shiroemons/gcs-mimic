use super::*;
use crate::{clock::SystemClock, test_support::TestDir};
use std::sync::{Mutex, PoisonError};

/// Walk entry index at which the seed walk of a root panics, keyed by the canonical root
/// because tests seed their own directories in parallel.
static PANIC_AT: Mutex<Vec<(PathBuf, usize)>> = Mutex::new(Vec::new());

type SourceSwap = (PathBuf, Box<dyn FnOnce() + Send>);
static SOURCE_SWAPS: Mutex<Vec<SourceSwap>> = Mutex::new(Vec::new());

pub(super) fn injected_source_swap(path: &Path) {
    let mut swaps = SOURCE_SWAPS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(index) = swaps.iter().position(|(target, _)| target == path) {
        let (_, swap) = swaps.remove(index);
        swap();
    }
}

pub(super) fn injected_walk_panic(root: &Path, index: usize) {
    let armed = PANIC_AT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .any(|(armed, at)| armed == root && *at == index);
    assert!(!armed, "injected seed walk panic at entry {index}");
}

fn fixture() -> (TestDir, Config) {
    let dir = TestDir::new();
    let seed = dir.0.join("seed");
    std::fs::create_dir_all(seed.join("bucket")).unwrap();
    for name in ["a", "b", "c"] {
        std::fs::write(seed.join("bucket").join(name), name).unwrap();
    }
    let data = dir.0.join("data");
    std::fs::create_dir(&data).unwrap();
    let config = Config {
        data_dir: data,
        seed_dir: Some(seed),
        ..Config::default()
    };
    (dir, config)
}

async fn seed_complete(catalog: &Catalog) -> bool {
    catalog.admin(Origin::Seed).seed_complete().await.unwrap()
}

#[tokio::test]
async fn a_complete_walk_imports_every_fixture_and_marks_the_seed_complete() {
    let (_dir, config) = fixture();
    let catalog = Catalog::open(&config.data_dir).await.unwrap();
    let outcome = import(&catalog, &config, &SystemClock).await.unwrap();
    assert!(matches!(
        outcome,
        SeedOutcome::Imported {
            buckets: 1,
            objects: 3,
            ..
        }
    ));
    assert!(seed_complete(&catalog).await);
    catalog.close().await.unwrap();
}

/// A walk thread that dies drops its sender; the import must not read the closed channel as
/// the end of the tree and mark a partial seed complete.
#[tokio::test]
async fn a_walk_that_panics_midway_fails_the_seed_and_leaves_it_incomplete() {
    let (_dir, config) = fixture();
    let root = std::fs::canonicalize(config.seed_dir.as_ref().unwrap()).unwrap();
    // Entry 0 is the bucket directory and entry 1 its first object.
    PANIC_AT
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((root, 2));
    let catalog = Catalog::open(&config.data_dir).await.unwrap();
    let error = import(&catalog, &config, &SystemClock).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("seed directory walk stopped before it finished"),
        "{error}"
    );
    assert!(!seed_complete(&catalog).await);
    assert!(
        crate::task::tests::reported_panics("seed-walk")
            .iter()
            .any(|message| message == "injected seed walk panic at entry 2")
    );
    catalog.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn seed_root_leaf_and_ancestor_symlink_swaps_never_publish_external_canaries() {
    for level in [0, 1, 2] {
        let (_dir, config) = fixture();
        let catalog = Catalog::open(&config.data_dir).await.unwrap();
        import(&catalog, &config, &SystemClock).await.unwrap();
        let admin = catalog.admin(Origin::Seed);
        let bucket = BucketName::new("bucket").unwrap();
        let name = ObjectName::new("a").unwrap();
        let old = admin.get_object(&bucket, &name, None).await.unwrap();
        let root = std::fs::canonicalize(config.seed_dir.as_ref().unwrap()).unwrap();
        let source = root.join("bucket/a");
        let outside = root.parent().unwrap().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("a"), b"external-canary").unwrap();
        let (key, selected, target) = match level {
            0 => (source.clone(), source, outside.join("a")),
            1 => (source, root.join("bucket"), outside.clone()),
            _ => {
                std::fs::create_dir(outside.join("bucket")).unwrap();
                std::fs::write(outside.join("bucket/a"), b"external-canary").unwrap();
                (
                    config.seed_dir.as_ref().unwrap().clone(),
                    root.clone(),
                    outside.clone(),
                )
            }
        };
        SOURCE_SWAPS.lock().unwrap().push((
            key,
            Box::new(move || {
                std::fs::rename(&selected, selected.with_extension("original")).unwrap();
                std::os::unix::fs::symlink(target, selected).unwrap();
            }),
        ));
        assert!(import(&catalog, &config, &SystemClock).await.is_err());
        assert_eq!(admin.get_object(&bucket, &name, None).await.unwrap(), old);
        let (_, mut reader) = admin.open_object(&bucket, &name, None).await.unwrap();
        assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"a"[..]);
        drop(reader);
        assert_eq!(
            std::fs::read(outside.join("a")).unwrap(),
            b"external-canary"
        );
        catalog.close().await.unwrap();
    }
}
