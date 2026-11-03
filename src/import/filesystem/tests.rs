use super::*;
use crate::storage::{change_journal::ChangeKind, failure::unavailable};

#[test]
fn unknown_stored_values_make_import_failures_terminal() {
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    let error = connection
        .query_row("SELECT 'unknown'", [], |row| row.get::<_, ChangeKind>(0))
        .unwrap_err();
    let error: AdminError = unavailable(error).into();
    assert!(matches!(error, AdminError::Corrupt(_)));
    assert!(terminal(&error));
    let query_error: AdminError = unavailable(rusqlite::Error::InvalidQuery).into();
    assert!(!terminal(&query_error));
}
