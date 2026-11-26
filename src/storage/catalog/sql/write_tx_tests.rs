use super::{exec, immediate, scalar};
use crate::storage::{Catalog, test_fixtures::bucket};
use crate::test_support::TestDir;
use rusqlite::{Connection, Error, ErrorCode};
use std::time::Duration;

fn table() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    exec(&connection, "CREATE TABLE entries (id INTEGER)", []).unwrap();
    connection
}

#[test]
fn a_failed_commit_rolls_back_the_transaction() {
    let mut connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE parents (id INTEGER PRIMARY KEY);
             CREATE TABLE children (parent INTEGER REFERENCES parents(id) DEFERRABLE INITIALLY DEFERRED);",
        )
        .unwrap();
    let transaction = immediate(&mut connection).unwrap();
    exec(&transaction, "INSERT INTO children VALUES (1)", []).unwrap();
    assert!(matches!(
        transaction.commit(),
        Err(Error::SqliteFailure(error, _))
            if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
    ));
    assert!(connection.is_autocommit());
    assert_eq!(
        scalar::<u32>(&connection, "SELECT count(*) FROM children", []).unwrap(),
        0
    );
    immediate(&mut connection).unwrap().commit().unwrap();
}

#[test]
fn dropping_an_uncommitted_write_rolls_back() {
    let mut connection = table();
    let transaction = immediate(&mut connection).unwrap();
    exec(&transaction, "INSERT INTO entries VALUES (1)", []).unwrap();
    drop(transaction);
    assert!(connection.is_autocommit());
    assert_eq!(
        scalar::<u32>(&connection, "SELECT count(*) FROM entries", []).unwrap(),
        0
    );
}

#[test]
fn rolling_back_a_savepoint_keeps_the_rest_of_the_write() {
    let mut connection = table();
    let transaction = immediate(&mut connection).unwrap();
    exec(&transaction, "INSERT INTO entries VALUES (1)", []).unwrap();
    exec(&transaction, "SAVEPOINT entry", []).unwrap();
    exec(&transaction, "INSERT INTO entries VALUES (2)", []).unwrap();
    exec(&transaction, "ROLLBACK TO entry", []).unwrap();
    exec(&transaction, "RELEASE entry", []).unwrap();
    transaction.commit().unwrap();
    assert!(connection.is_autocommit());
    assert_eq!(
        scalar::<u32>(&connection, "SELECT count(*) FROM entries", []).unwrap(),
        1
    );
    assert_eq!(
        scalar::<u32>(&connection, "SELECT id FROM entries", []).unwrap(),
        1
    );
}

#[test]
fn immediate_reports_a_busy_writer_and_can_retry() {
    let dir = TestDir::new();
    let path = dir.0.join("writer.sqlite3");
    let mut first = Connection::open(&path).unwrap();
    let mut second = Connection::open(&path).unwrap();
    second.busy_timeout(Duration::ZERO).unwrap();

    let held = immediate(&mut first).unwrap();
    assert!(matches!(
        immediate(&mut second),
        Err(Error::SqliteFailure(error, _)) if error.code == ErrorCode::DatabaseBusy
    ));
    assert!(second.is_autocommit());
    held.commit().unwrap();
    immediate(&mut second).unwrap().commit().unwrap();
}

#[tokio::test]
async fn committing_a_new_bucket_uses_one_writer_job() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let (writes, reads) = catalog.job_counts();
    catalog
        .insert_bucket(bucket("bucket", "project"))
        .await
        .unwrap();
    assert_eq!(catalog.job_counts(), (writes + 1, reads));
    catalog.close().await.unwrap();
}
