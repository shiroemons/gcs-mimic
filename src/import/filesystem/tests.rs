use super::*;
use crate::storage::{change_journal::ChangeKind, failure::unavailable};

#[tokio::test]
async fn joining_a_scanner_does_not_block_the_async_worker() {
    let (release, blocked) = std::sync::mpsc::channel();
    let worker = crate::task::spawn_thread("import-join-test", move || {
        blocked.recv_timeout(Duration::from_secs(1)).unwrap();
        Ok(Summary::default())
    })
    .unwrap();
    let join = join_scan(worker);
    tokio::pin!(join);
    tokio::select! {
        biased;
        result = &mut join => panic!("scanner completed before release: {result:?}"),
        () = tokio::time::sleep(Duration::from_millis(10)) => {},
    }
    release.send(()).unwrap();
    join.await.unwrap().unwrap();
}

#[test]
fn unknown_stored_values_make_import_failures_terminal() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let error = connection
        .query_row("SELECT 'unknown'", [], |row| row.get::<_, ChangeKind>(0))
        .unwrap_err();
    let error: AdminError = unavailable(error).into();
    assert!(matches!(error, AdminError::Corrupt(_)));
    assert!(terminal(&error.into()));
    let query_error: AdminError = unavailable(rusqlite::Error::InvalidQuery).into();
    assert!(!terminal(&query_error.into()));
}
