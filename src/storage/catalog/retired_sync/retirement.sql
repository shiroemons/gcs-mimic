CREATE TABLE reset_epoch (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    epoch INTEGER NOT NULL CHECK(typeof(epoch) = 'integer' AND epoch >= 0)
) WITHOUT ROWID;
INSERT INTO reset_epoch SELECT singleton, epoch FROM change_journal_state;
DROP TABLE change_journal;
DROP TABLE change_journal_state;
DROP TABLE sync_deletion_members;
DROP TABLE sync_deletion_sets;
DROP TABLE sync_deletion_history;
DROP TABLE sync_finding_owners;
DROP TABLE sync_findings;
DROP TABLE sync_path_stops;
DROP TABLE sync_observations;
DROP TABLE sync_writeback_ops;
DROP TABLE sync_writeback_roots;
DROP TABLE sync_conflict_operations;
DROP TABLE sync_conflicts;
DROP TABLE sync_conflict_roots;
DROP TABLE sync_recovery_workspaces;
DROP TABLE sync_buckets;
DROP TABLE sync_objects;
DROP TABLE sync_roots;
PRAGMA user_version = 58;
