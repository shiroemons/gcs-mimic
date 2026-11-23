//! Startup replays and reseeds that read what seeding recorded at each name.

use super::*;
use crate::{
    object::ObjectRecord,
    storage::admin::{AdminError, ObjectName},
};

fn object_name(name: &str) -> ObjectName {
    ObjectName::new(name).unwrap()
}

fn bucket_name() -> BucketName {
    BucketName::new("bucket").unwrap()
}

/// The live object at `bucket/name`, or `None` where the name holds none.
async fn live(catalog: &Catalog, name: &str) -> Option<ObjectRecord> {
    match catalog
        .admin(Origin::Seed)
        .get_object(&bucket_name(), &object_name(name), None)
        .await
    {
        Ok(record) => Some(record),
        Err(AdminError::NotFound) => None,
        Err(error) => panic!("{error}"),
    }
}

/// Writes `body` at `bucket/name` the way an API client would, with the content type and
/// metadata a replay would write, so that only the generation tells it from seeding's own.
async fn api_write(catalog: &Catalog, config: &Config, name: &str, body: &[u8]) -> ObjectRecord {
    let dir = TestDir::new();
    let path = dir.0.join("body");
    std::fs::write(&path, body).unwrap();
    catalog
        .admin(Origin::Seed)
        .publish_from_file(
            &bucket_name(),
            &object_name(name),
            Publication {
                content_type: config.seed_content_type.for_name(name).into(),
                expected: ExpectedHashes::default(),
                condition: None,
                metadata: Default::default(),
                max_bytes: config.max_object_bytes,
                mode: PublishMode::Replace,
            },
            std::fs::File::open(&path).unwrap(),
        )
        .await
        .unwrap()
}

fn remove(config: &Config, relative: &str) {
    let path = config.seed_dir.as_ref().unwrap().join(relative);
    if path.is_dir() {
        std::fs::remove_dir_all(path).unwrap();
    } else {
        std::fs::remove_file(path).unwrap();
    }
}

/// A restart deletes the object seeding published for a file removed while the server was
/// stopped, and keeps the others and the bucket.
#[cfg(unix)]
#[tokio::test]
async fn a_restart_deletes_the_object_of_a_file_removed_while_stopped() {
    let (_dir, config) = fixture();
    let catalog = Catalog::open(&config.data_dir).await.unwrap();
    import(&catalog, &config, &clock()).await.unwrap();
    remove(&config, "bucket/b");
    import(&catalog, &config, &clock()).await.unwrap();
    assert!(live(&catalog, "b").await.is_none());
    assert!(live(&catalog, "a").await.is_some());
    assert!(live(&catalog, "c").await.is_some());

    // A file back with the body seeding deleted is published again.
    std::fs::write(config.seed_dir.as_ref().unwrap().join("bucket/b"), "b").unwrap();
    import(&catalog, &config, &clock()).await.unwrap();
    assert!(live(&catalog, "b").await.is_some());
    catalog.close().await.unwrap();
}

/// A removed bucket directory deletes the objects seeding owns there but never the bucket.
#[cfg(unix)]
#[tokio::test]
async fn a_removed_bucket_directory_keeps_its_bucket() {
    let (_dir, config) = fixture();
    let catalog = Catalog::open(&config.data_dir).await.unwrap();
    import(&catalog, &config, &clock()).await.unwrap();
    remove(&config, "bucket");
    import(&catalog, &config, &clock()).await.unwrap();
    for name in ["a", "b", "c"] {
        assert!(live(&catalog, name).await.is_none(), "{name}");
    }
    let admin = catalog.admin(Origin::Seed);
    assert!(admin.get_bucket(&bucket_name()).await.unwrap().is_some());
    catalog.close().await.unwrap();
}

/// An object an API write replaced is never seeding's to delete, even with the body the file
/// held; the restart forgets the name instead.
#[cfg(unix)]
#[tokio::test]
async fn a_restart_keeps_an_api_write_at_the_name_of_a_removed_file() {
    let (_dir, config) = fixture();
    let catalog = Catalog::open(&config.data_dir).await.unwrap();
    import(&catalog, &config, &clock()).await.unwrap();
    let written = api_write(&catalog, &config, "b", b"b").await;
    remove(&config, "bucket/b");
    import(&catalog, &config, &clock()).await.unwrap();
    assert_eq!(
        live(&catalog, "b").await.unwrap().generation,
        written.generation
    );
    // Forgotten, the name is a new file when one returns, as it would have been had it stayed.
    std::fs::write(config.seed_dir.as_ref().unwrap().join("bucket/b"), "other").unwrap();
    import(&catalog, &config, &clock()).await.unwrap();
    assert_ne!(
        live(&catalog, "b").await.unwrap().generation,
        written.generation
    );
    catalog.close().await.unwrap();
}

/// The first start that keeps records takes no ownership of an identical object it did not
/// publish, as of a catalog an older release seeded: removing the file later keeps the object.
#[cfg(unix)]
#[tokio::test]
async fn the_first_start_with_records_claims_no_object_it_did_not_publish() {
    let (_dir, config) = fixture();
    let catalog = Catalog::open(&config.data_dir).await.unwrap();
    catalog
        .admin(Origin::Seed)
        .ensure_bucket(
            BucketDefinition::default_for(&bucket_name(), &config.seed_project, &SystemClock)
                .unwrap(),
        )
        .await
        .unwrap();
    let older = api_write(&catalog, &config, "a", b"a").await;
    let outcome = import(&catalog, &config, &clock()).await.unwrap();
    assert!(matches!(
        outcome,
        SeedOutcome::Imported {
            objects: 2,
            unchanged: 1,
            ..
        }
    ));
    remove(&config, "bucket/a");
    import(&catalog, &config, &clock()).await.unwrap();
    assert_eq!(
        live(&catalog, "a").await.unwrap().generation,
        older.generation
    );
    catalog.close().await.unwrap();
}

/// A reseed on disk aligns with the folder as a restart does.
#[cfg(unix)]
#[tokio::test]
async fn a_reseed_deletes_the_object_of_a_removed_file() {
    let (_dir, config) = fixture();
    let catalog = Catalog::open(&config.data_dir).await.unwrap();
    import(&catalog, &config, &clock()).await.unwrap();
    remove(&config, "bucket/c");
    let admin = catalog.admin(Origin::Seed);
    let deadline = crate::test_support::hang_deadline();
    reseed(&admin, &config, &clock(), deadline).await.unwrap();
    assert!(live(&catalog, "c").await.is_none());
    catalog.close().await.unwrap();
}
