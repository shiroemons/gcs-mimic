CREATE TABLE sync_conflict_roots (
 root_id BLOB PRIMARY KEY REFERENCES sync_roots(root_id),
 revision TEXT NOT NULL CHECK(length(revision)=32)
) WITHOUT ROWID;
CREATE TABLE sync_conflicts (
 ordinal INTEGER PRIMARY KEY,
 id TEXT NOT NULL UNIQUE CHECK(length(id)=32 AND id NOT GLOB '*[^0-9a-f]*'),
 root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
 revision TEXT NOT NULL CHECK(length(revision)=32),
 owner TEXT NOT NULL CHECK(length(owner) BETWEEN 1 AND 256),
 bucket TEXT NOT NULL CHECK(length(bucket) BETWEEN 1 AND 222),
 incarnation TEXT NOT NULL CHECK(length(incarnation)=32),
 name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 1024),
 generation INTEGER CHECK(generation>0),
 state INTEGER NOT NULL CHECK(state BETWEEN 0 AND 2),
 size INTEGER NOT NULL CHECK(size>=0),
 source_op TEXT,
 source BLOB NOT NULL CHECK(length(source)<=2048),
 retained BLOB CHECK(retained IS NULL OR length(retained)<=2048)
);
CREATE INDEX sync_conflicts_page ON sync_conflicts(root_id,ordinal) WHERE state!=2;
CREATE UNIQUE INDEX sync_conflicts_workspace ON sync_conflicts(root_id,source_op) WHERE source_op IS NOT NULL AND state!=2;
CREATE TABLE sync_conflict_operations (
 operation_id TEXT PRIMARY KEY CHECK(length(operation_id) BETWEEN 1 AND 128),
 root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
 target_id TEXT NOT NULL CHECK(length(target_id)=32),
 target_kind INTEGER NOT NULL CHECK(target_kind IN(0,1)),
 owner TEXT NOT NULL CHECK(length(owner) BETWEEN 1 AND 256),
 payload BLOB NOT NULL CHECK(length(payload)<=4096),
 action INTEGER NOT NULL CHECK(action IN(0,1)),
 done INTEGER NOT NULL CHECK(done IN(0,1)),
 result_generation INTEGER CHECK(result_generation>0),
 error_code TEXT CHECK(error_code IS NULL OR error_code IN('target_resolved','target_changed')),
 reclaimed INTEGER NOT NULL DEFAULT 0 CHECK(reclaimed IN(0,1))
) WITHOUT ROWID;
CREATE INDEX sync_conflict_resolution_active ON sync_conflict_operations(root_id,target_kind,target_id) WHERE done=0;
CREATE TABLE sync_recovery_workspaces (
 id TEXT PRIMARY KEY CHECK(length(id)=32),
 root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
 revision TEXT NOT NULL CHECK(length(revision)=32),
 target_identity TEXT NOT NULL CHECK(length(target_identity)=32),
 observation BLOB NOT NULL CHECK(length(observation)<=2048),
 phase INTEGER NOT NULL CHECK(phase BETWEEN 0 AND 2)
) WITHOUT ROWID;
CREATE TRIGGER sync_observation_conflict_insert AFTER INSERT ON sync_conflicts BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
 UPDATE sync_conflict_roots SET revision=lower(hex(randomblob(16))) WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_conflict_update AFTER UPDATE ON sync_conflicts BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
 UPDATE sync_conflict_roots SET revision=lower(hex(randomblob(16))) WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_conflict_delete AFTER DELETE ON sync_conflicts BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=OLD.root_id;
 UPDATE sync_conflict_roots SET revision=lower(hex(randomblob(16))) WHERE root_id=OLD.root_id;
END;
CREATE TRIGGER sync_observation_resolution_insert AFTER INSERT ON sync_conflict_operations BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_resolution_update AFTER UPDATE ON sync_conflict_operations BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_resolution_delete AFTER DELETE ON sync_conflict_operations BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=OLD.root_id;
END;
CREATE TRIGGER sync_observation_recovery_insert AFTER INSERT ON sync_recovery_workspaces BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_recovery_update AFTER UPDATE ON sync_recovery_workspaces BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_recovery_delete AFTER DELETE ON sync_recovery_workspaces BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=OLD.root_id;
END;
PRAGMA user_version=42;
