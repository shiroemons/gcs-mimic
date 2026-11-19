//! A catalog that an earlier release let register several synchronized directories, whose
//! tracked files carry no root and so may be another directory's.
#![expect(
    clippy::unwrap_used,
    reason = "Clippy does not exempt platform-gated test modules; fixture unwraps must fail the test"
)]
use super::ingest_durability::Started;
use super::*;

const BUCKET: &str = "test-bucket";

async fn generation(catalog: &Catalog, bucket: &BucketName, name: &str) -> Option<i64> {
    match catalog
        .admin(Origin::Api)
        .get_object(bucket, &ObjectName::new(name).unwrap(), None)
        .await
    {
        Ok(object) => Some(object.generation),
        Err(AdminError::NotFound) => None,
        Err(error) => panic!("{error}"),
    }
}

/// The tracked files of the tree, `kept` and `gone`, describe another registered directory: its
/// `kept` differs from this tree's, which never lost its `gone`. Startup keeps only this root and
/// merges it without them, so this tree's `kept` publishes no version over the object, and the
/// missing `gone` is written back instead of deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_one_of_several_roots_neither_publishes_nor_deletes_by_the_others_files() {
    let (data, tree, catalog, bucket, root) = live_fixture().await;
    drop(root);
    catalog.close().await.unwrap();
    fs::create_dir(tree.0.join(BUCKET)).unwrap();
    for name in ["kept", "gone"] {
        fs::write(tree.0.join(BUCKET).join(name), b"first").unwrap();
    }
    let started = Started::start(&data.0, &tree.0).await;
    let kept = generation(&started.catalog, &bucket, "kept").await.unwrap();
    let gone = generation(&started.catalog, &bucket, "gone").await.unwrap();
    started.close().await;

    fs::write(tree.0.join(BUCKET).join("kept"), b"the other tree").unwrap();
    fs::remove_file(tree.0.join(BUCKET).join("gone")).unwrap();
    let database = data.0.join("catalog.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute(
            "INSERT INTO sync_roots VALUES (randomblob(16), CAST('/elsewhere' AS BLOB), zeroblob(8), x'0000000000000001', 1)",
            [],
        )
        .unwrap();
    drop(connection);

    let started = Started::start(&data.0, &tree.0).await;
    assert_eq!(
        generation(&started.catalog, &bucket, "kept").await,
        Some(kept)
    );
    assert_eq!(
        generation(&started.catalog, &bucket, "gone").await,
        Some(gone)
    );
    let set = started.root.deletion_set().await.unwrap();
    assert_eq!(set.map_or(0, |set| set.candidate_count), 0);
    assert_eq!(
        fs::read(tree.0.join(BUCKET).join("gone")).unwrap(),
        b"first"
    );
    started.close().await;
    let connection = rusqlite::Connection::open(&database).unwrap();
    let roots: i64 = connection
        .query_row("SELECT count(*) FROM sync_roots", [], |row| row.get(0))
        .unwrap();
    assert_eq!(roots, 1);
}
