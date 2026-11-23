CREATE TABLE sync_deletion_history (
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
PRAGMA user_version=50;
