use super::*;
use crate::{
    bucket::BucketConditions,
    storage::{
        Catalog,
        catalog::{schema::validate_schema, test_fixtures},
    },
    test_support::TestDir,
};
use std::time::{Duration, UNIX_EPOCH};

fn sample(owned: Option<Version>, deleted: Option<Version>) -> SeedRecord {
    let identity = FileIdentity {
        device: u64::MAX,
        inode: 1 << 63,
        size: 5,
        modified_ns: -1,
        changed_ns: i128::MAX,
    };
    SeedRecord {
        owned,
        deleted,
        body: Body {
            size: 5,
            hashes: Hashes {
                md5: [7; 16],
                crc32c: u32::MAX,
            },
        },
        fingerprint: Fingerprint::stored(identity, true),
        absent: Some(Absence {
            since: UNIX_EPOCH + Duration::from_micros(1_700_000_000_000_001),
            pass: i64::MAX.unsigned_abs(),
        }),
    }
}

async fn run<T: Send + 'static>(
    catalog: &Catalog,
    operation: impl FnOnce(&mut Connection) -> Result<T, CatalogError> + Send + 'static,
) -> T {
    catalog.run(operation).await.unwrap()
}

/// A catalog at schema 58 gains the empty record and mark tables, and the upgraded catalog
/// validates at schema 59.
#[tokio::test]
async fn schema_58_upgrades_to_the_record_and_mark_tables() {
    let dir = TestDir::new();
    Catalog::open(&dir.0).await.unwrap().close().await.unwrap();
    let path = dir.0.join("catalog.sqlite3");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("DROP TABLE seed_buckets; DROP TABLE seed_objects;")
        .unwrap();
    connection
        .pragma_update(None, "user_version", SCHEMA_VERSION - 1)
        .unwrap();
    validate_schema(&connection, SCHEMA_VERSION - 1).unwrap();
    drop(connection);

    let catalog = Catalog::open(&dir.0).await.unwrap();
    let records = run(&catalog, |connection| all(connection)).await;
    assert!(records.is_empty());
    catalog.close().await.unwrap();
    let connection = Connection::open(&path).unwrap();
    test_fixtures::assert_current_schema(&connection);
    let columns: Vec<String> = connection
        .prepare("SELECT name FROM pragma_table_info('seed_objects')")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(
        columns
            .iter()
            .any(|column| column == "owned_metageneration")
    );
}

/// Every field a decision reads comes back as it was saved, including values that use the whole
/// width of their type; clearing a proof of absence and forgetting touch only their name, and
/// clearing every proof touches only the proofs.
#[tokio::test]
async fn a_saved_record_reads_back_whole() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let owned = Version {
        generation: 3,
        metageneration: 2,
    };
    let first = sample(Some(owned), None);
    let second = sample(None, Some(owned));
    let records = run(&catalog, move |connection| {
        save(connection, "bucket", "a", &first)?;
        save(connection, "bucket", "b/c", &second)?;
        let read = get(connection, "bucket", "a")?;
        found(connection, "bucket", "a")?;
        let cleared = get(connection, "bucket", "a")?;
        clear_absences(connection)?;
        let cleared_all = get(connection, "bucket", "b/c")?;
        forget(connection, "bucket", "b/c")?;
        Ok((read, cleared, cleared_all, all(connection)?))
    })
    .await;
    assert_eq!(records.0, Some(first));
    assert_eq!(records.1, Some(first.found()));
    assert_eq!(records.2, Some(second.found()));
    assert_eq!(
        records.3.keys().cloned().collect::<Vec<_>>(),
        [("bucket".to_owned(), "a".to_owned())]
    );
    let refused = run(&catalog, |connection| {
        Ok(connection
            .execute(
                "UPDATE seed_objects SET owned_metageneration = NULL WHERE name = 'a'",
                [],
            )
            .is_err())
    })
    .await;
    assert!(refused, "an owned generation needs its metageneration");
    catalog.close().await.unwrap();
}

/// A mark tells a bucket seeding took from one an API request deleted or created again, and a
/// bucket deletion drops the records of the bucket's names with it.
#[tokio::test]
async fn a_mark_outlives_its_bucket_and_a_deletion_drops_its_records() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    catalog
        .insert_bucket(test_fixtures::bucket("bucket", "project"))
        .await
        .unwrap();
    let record = sample(None, None);
    let marked = run(&catalog, move |connection| {
        let before = mark_of(connection, "bucket")?;
        mark(connection, "bucket")?;
        save(connection, "bucket", "a", &record)?;
        Ok((before, mark_of(connection, "bucket")?))
    })
    .await;
    assert_eq!(marked, (Mark::Unmarked, Mark::Current));

    catalog
        .delete_bucket(
            "bucket",
            BucketConditions::default(),
            test_fixtures::PERMISSIVE,
        )
        .await
        .unwrap();
    let deleted = run(&catalog, |connection| {
        Ok((mark_of(connection, "bucket")?, all(connection)?.len()))
    })
    .await;
    assert_eq!(deleted, (Mark::Gone, 0));

    catalog
        .insert_bucket(test_fixtures::bucket("bucket", "project"))
        .await
        .unwrap();
    let recreated = run(&catalog, |connection| {
        let recreated = mark_of(connection, "bucket")?;
        let listed = marks(connection)?;
        unmark(connection, "bucket")?;
        Ok((recreated, listed, mark_of(connection, "bucket")?))
    })
    .await;
    assert_eq!(
        recreated,
        (Mark::Gone, vec!["bucket".to_owned()], Mark::Unmarked)
    );
    catalog.close().await.unwrap();
}

/// A mark keeps the proofs that its directory is gone until a pass clears them, a startup replay
/// or a reseed clears every one, or seeding marks the bucket again.
#[tokio::test]
async fn a_mark_keeps_its_proofs_of_absence_until_they_are_cleared() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    for name in ["one", "two", "three"] {
        catalog
            .insert_bucket(test_fixtures::bucket(name, "project"))
            .await
            .unwrap();
    }
    let proof = Absence {
        since: UNIX_EPOCH + Duration::from_micros(1_700_000_000_000_001),
        pass: 3,
    };
    let states = run(&catalog, move |connection| {
        for name in ["one", "two", "three"] {
            mark(connection, name)?;
            mark_absent(connection, name, proof)?;
        }
        let proven = mark_states(connection)?;
        mark_found(connection, "one")?;
        mark(connection, "two")?;
        let one_and_two = (
            mark_absence(connection, "one")?,
            mark_absence(connection, "two")?,
        );
        let three = mark_absence(connection, "three")?;
        clear_absences(connection)?;
        Ok((proven, one_and_two, three, mark_states(connection)?))
    })
    .await;
    let (proven, one_and_two, three, cleared) = states;
    let marked = |absent| Marked {
        mark: Mark::Current,
        absent,
    };
    assert_eq!(proven["two"], marked(Some(proof)));
    assert_eq!(one_and_two, (None, None));
    assert_eq!(three, Some(proof));
    assert_eq!(cleared["three"], marked(None));
    catalog.close().await.unwrap();
}
