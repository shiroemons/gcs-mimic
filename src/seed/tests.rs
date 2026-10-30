use super::*;
use crate::{clock::SystemClock, test_support::TestDir};
use std::sync::{Mutex, PoisonError};

/// Walk entry index at which the seed walk of a root panics, keyed by the canonical root
/// because tests seed their own directories in parallel.
static PANIC_AT: Mutex<Vec<(PathBuf, usize)>> = Mutex::new(Vec::new());

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
