use rusqlite::Connection;

pub(crate) fn count(connection: &Connection, from: &str) -> i64 {
    connection
        .query_row(&format!("SELECT count(*) FROM {from}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}
