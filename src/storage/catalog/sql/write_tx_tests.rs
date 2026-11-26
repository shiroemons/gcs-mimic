use super::{deferred_write, exec, exec_on_writer, immediate, scalar};
use crate::storage::{Catalog, test_fixtures::bucket};
use crate::test_support::TestDir;
use rusqlite::trace::{TraceEvent, TraceEventCodes};
use rusqlite::{Connection, Error, ErrorCode};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

fn table() -> Connection {
    let mut connection = Connection::open_in_memory().unwrap();
    exec_on_writer(&mut connection, "CREATE TABLE entries (id INTEGER)", []).unwrap();
    connection
}

fn trace_statements(connection: &mut Connection, expected: &'static str) -> Arc<AtomicUsize> {
    let statements = Arc::new(AtomicUsize::new(0));
    let count = statements.clone();
    connection
        .trace_v2(
            TraceEventCodes::SQLITE_TRACE_STMT,
            Some(move |event: TraceEvent<'_>| {
                if let TraceEvent::Stmt(_, sql) = event
                    && sql == expected
                {
                    count.fetch_add(1, Ordering::Relaxed);
                }
            }),
        )
        .unwrap();
    statements
}

fn foreign_keys() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE parents (id INTEGER PRIMARY KEY);
             CREATE TABLE children (parent INTEGER REFERENCES parents(id) DEFERRABLE INITIALLY DEFERRED);",
        )
        .unwrap();
    connection
}

#[test]
fn a_failed_commit_rolls_back_the_transaction() {
    let mut connection = foreign_keys();
    let rollbacks = trace_statements(&mut connection, "ROLLBACK");
    let transaction = immediate(&mut connection).unwrap();
    exec(&transaction, "INSERT INTO children VALUES (1)", []).unwrap();
    assert!(matches!(
        transaction.commit(),
        Err(Error::SqliteFailure(error, _))
            if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
    ));
    assert_eq!(rollbacks.load(Ordering::Relaxed), 1);
    assert!(connection.is_autocommit());
    assert_eq!(
        scalar::<u32>(&connection, "SELECT count(*) FROM children", []).unwrap(),
        0
    );
    immediate(&mut connection).unwrap().commit().unwrap();
}

#[test]
fn an_automatic_sqlite_rollback_needs_no_drop_rollback() {
    let mut connection = table();
    exec_on_writer(
        &mut connection,
        "CREATE UNIQUE INDEX entry_id ON entries (id)",
        [],
    )
    .unwrap();
    let rollbacks = trace_statements(&mut connection, "ROLLBACK");
    let transaction = immediate(&mut connection).unwrap();
    exec(&transaction, "INSERT INTO entries VALUES (1)", []).unwrap();
    assert!(matches!(
        exec(&transaction, "INSERT OR ROLLBACK INTO entries VALUES (1)", []),
        Err(Error::SqliteFailure(error, _))
            if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    ));
    assert!(transaction.is_autocommit());
    assert!(transaction.commit().is_err());
    assert_eq!(rollbacks.load(Ordering::Relaxed), 0);
    assert_eq!(
        scalar::<u32>(&connection, "SELECT count(*) FROM entries", []).unwrap(),
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

#[test]
fn a_deferred_write_does_not_reserve_the_writer_at_begin() {
    let dir = TestDir::new();
    let path = dir.0.join("writer.sqlite3");
    let mut first = Connection::open(&path).unwrap();
    let mut second = Connection::open(&path).unwrap();
    exec_on_writer(&mut first, "CREATE TABLE entries (id INTEGER)", []).unwrap();
    second.busy_timeout(Duration::ZERO).unwrap();
    let begins = trace_statements(&mut second, "BEGIN DEFERRED");

    let held = immediate(&mut first).unwrap();
    let transaction = deferred_write(&mut second).unwrap();
    assert_eq!(begins.load(Ordering::Relaxed), 1);
    assert!(matches!(
        exec(&transaction, "INSERT INTO entries VALUES (1)", []),
        Err(Error::SqliteFailure(error, _)) if error.code == ErrorCode::DatabaseBusy
    ));
    drop(transaction);
    assert!(second.is_autocommit());
    held.commit().unwrap();
    let transaction = deferred_write(&mut second).unwrap();
    exec(&transaction, "INSERT INTO entries VALUES (1)", []).unwrap();
    transaction.commit().unwrap();
}

#[test]
fn deferred_commit_failure_and_drop_each_roll_back() {
    let mut connection = foreign_keys();
    let rollbacks = trace_statements(&mut connection, "ROLLBACK");
    let transaction = deferred_write(&mut connection).unwrap();
    exec(&transaction, "INSERT INTO children VALUES (1)", []).unwrap();
    assert!(matches!(
        transaction.commit(),
        Err(Error::SqliteFailure(error, _))
            if error.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY
    ));
    assert!(connection.is_autocommit());
    assert_eq!(rollbacks.load(Ordering::Relaxed), 1);

    let transaction = deferred_write(&mut connection).unwrap();
    exec(&transaction, "INSERT INTO parents VALUES (1)", []).unwrap();
    drop(transaction);
    assert!(connection.is_autocommit());
    assert_eq!(rollbacks.load(Ordering::Relaxed), 2);
    assert_eq!(
        scalar::<u32>(
            &connection,
            "SELECT (SELECT count(*) FROM parents) + (SELECT count(*) FROM children)",
            [],
        )
        .unwrap(),
        0
    );
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

#[tokio::test]
async fn configuring_notifications_uses_one_writer_job() {
    let dir = TestDir::new();
    let catalog = Catalog::open(&dir.0).await.unwrap();
    let (writes, reads) = catalog.job_counts();
    catalog
        .configure_notifications(&crate::notification::Runtime::default())
        .await
        .unwrap();
    assert_eq!(catalog.job_counts(), (writes + 1, reads));
    catalog.close().await.unwrap();
}
