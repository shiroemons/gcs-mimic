//! Automatic deletion history survives rescans and restart until an explicit safe boundary.
use super::*;

pub(in crate::storage) const SCHEMA: &str = "CREATE TABLE sync_deletion_history (
 root_id BLOB PRIMARY KEY REFERENCES sync_roots(root_id) ON DELETE CASCADE,
 automatic_count INTEGER NOT NULL DEFAULT 0 CHECK(typeof(automatic_count)='integer' AND automatic_count>=0)
) WITHOUT ROWID;
INSERT INTO sync_deletion_history(root_id) SELECT root_id FROM sync_roots;
CREATE TRIGGER sync_deletion_history_root_insert AFTER INSERT ON sync_roots BEGIN
 INSERT INTO sync_deletion_history(root_id) VALUES(NEW.root_id);
END;
CREATE TRIGGER sync_deletion_history_baseline AFTER UPDATE OF complete ON sync_roots
WHEN OLD.complete=0 AND NEW.complete=1 BEGIN
 UPDATE sync_deletion_history SET automatic_count=0 WHERE root_id=NEW.root_id;
END;
PRAGMA user_version=50;";

pub(super) fn automatic_count(
    connection: &Connection,
    root: [u8; 16],
) -> Result<u64, CatalogError> {
    Ok(sql::scalar(
        connection,
        "SELECT automatic_count FROM sync_deletion_history WHERE root_id=?1",
        [root],
    )?)
}

impl RootLease {
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) async fn automatic_deletion_count(&self) -> Result<u64, RootError> {
        let root = self.status().root_id.as_bytes();
        Ok(self
            .catalog()
            .run_admin(move |connection| automatic_count(connection, root))
            .await?)
    }
    pub(crate) async fn reset_empty_deletion_history(&self) -> Result<(), RootError> {
        let root = self.status().root_id.as_bytes();
        let scope = self.control_scope();
        self.catalog()
            .run_admin(move |connection| {
                let tx = connection.transaction()?;
                scope.authorize(&tx)?;
                if read_set(&tx, root)?.is_some_and(|set| set.candidate_count == 0) {
                    sql::exec(
                        &tx,
                        "UPDATE sync_deletion_history SET automatic_count=0 WHERE root_id=?1",
                        [root],
                    )?;
                }
                tx.commit().map_err(unavailable)
            })
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
