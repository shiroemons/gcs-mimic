use super::*;
use crate::storage::catalog;

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl RootLease {
    pub(crate) async fn reject_deletion_history_reset(&self) -> Result<(), RootError> {
        Ok(self.catalog().run_admin(|connection|{
            connection.execute_batch("CREATE TRIGGER refuse_history_reset BEFORE UPDATE ON sync_deletion_history BEGIN SELECT RAISE(ABORT,'history reset blocked'); END;")?;
            Ok(())
        }).await?)
    }
}

#[test]
fn automatic_deletion_history_migrates_existing_roots_and_rejects_older_readers() {
    let mut connection = Connection::open_in_memory().unwrap();
    catalog::initialize_catalog(&mut connection).unwrap();
    for (version, migration) in catalog::MIGRATIONS
        .iter()
        .filter(|(version, _)| *version <= 49)
    {
        let current: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        if current < *version {
            migration.apply(&mut connection).unwrap();
        }
    }
    connection
        .execute(
            "INSERT INTO sync_roots VALUES(?1,?2,?3,?4,1)",
            params![[1u8; 16], b"root", [0u8; 8], [1u8; 8]],
        )
        .unwrap();
    catalog::validate_schema(&connection, 49).unwrap();
    connection
        .execute_batch("CREATE TABLE sync_deletion_history(collision)")
        .unwrap();
    assert!(
        catalog::Migration::Statements(SCHEMA)
            .apply(&mut connection)
            .is_err()
    );
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .unwrap(),
        49
    );
    connection
        .execute_batch("DROP TABLE sync_deletion_history")
        .unwrap();
    catalog::Migration::Statements(SCHEMA)
        .apply(&mut connection)
        .unwrap();
    catalog::validate_schema(&connection, 50).unwrap();
    assert_eq!(automatic_count(&connection, [1u8; 16]).unwrap(), 0);
    connection
        .execute("UPDATE sync_deletion_history SET automatic_count=12", [])
        .unwrap();
    connection
        .execute("UPDATE sync_roots SET complete=1", [])
        .unwrap();
    assert_eq!(automatic_count(&connection, [1u8; 16]).unwrap(), 12);
    connection
        .execute("UPDATE sync_roots SET complete=0", [])
        .unwrap();
    connection
        .execute("UPDATE sync_roots SET complete=1", [])
        .unwrap();
    assert_eq!(automatic_count(&connection, [1u8; 16]).unwrap(), 0);
    assert!(catalog::validate_schema_version(50, 49).is_err());
}
