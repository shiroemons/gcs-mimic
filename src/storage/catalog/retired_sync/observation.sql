CREATE TABLE sync_observations (
 root_id BLOB PRIMARY KEY REFERENCES sync_roots(root_id) ON DELETE CASCADE,
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision BETWEEN 0 AND 9223372036854775806),
 paused_reason TEXT CHECK(paused_reason IS NULL OR length(paused_reason) BETWEEN 1 AND 4096),
 scan_stop TEXT CHECK(scan_stop IS NULL OR scan_stop IN ('SourceChanging','SourceUnrepresentable'))
) WITHOUT ROWID;
INSERT INTO sync_observations SELECT root_id,0,NULL,NULL FROM sync_roots;
CREATE TABLE sync_path_stops (
 ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
 root_id BLOB NOT NULL REFERENCES sync_roots(root_id) ON DELETE CASCADE,
 bucket TEXT NOT NULL COLLATE BINARY,
 name TEXT NOT NULL COLLATE BINARY,
 owner_project TEXT NOT NULL CHECK(length(owner_project) BETWEEN 1 AND 256),
 incarnation TEXT NOT NULL CHECK(length(incarnation)=32),
 generation INTEGER NOT NULL CHECK(generation>=0),
 reason TEXT NOT NULL CHECK(reason IN ('ProtectedDeletion','SourceChanging','IoFailure')),
 UNIQUE(root_id,bucket,name,incarnation,reason)
);
CREATE INDEX sync_path_stops_page ON sync_path_stops(root_id,ordinal);
CREATE TRIGGER sync_observation_stop_insert AFTER INSERT ON sync_path_stops BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_stop_update AFTER UPDATE ON sync_path_stops BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_stop_delete AFTER DELETE ON sync_path_stops BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=OLD.root_id;
END;
CREATE TRIGGER sync_observation_root_insert AFTER INSERT ON sync_roots BEGIN
 INSERT INTO sync_observations VALUES(NEW.root_id,0,NULL,NULL);
END;
CREATE TRIGGER sync_observation_root_update AFTER UPDATE ON sync_roots BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_finding_insert AFTER INSERT ON sync_findings BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_finding_update AFTER UPDATE ON sync_findings BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_finding_delete AFTER DELETE ON sync_findings BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=OLD.root_id;
END;
CREATE TRIGGER sync_observation_operation_insert AFTER INSERT ON sync_writeback_ops BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_operation_update AFTER UPDATE ON sync_writeback_ops BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_operation_delete AFTER DELETE ON sync_writeback_ops BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=OLD.root_id;
END;
CREATE TRIGGER sync_observation_writeback_insert AFTER INSERT ON sync_writeback_roots BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_writeback_update AFTER UPDATE ON sync_writeback_roots BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_writeback_delete AFTER DELETE ON sync_writeback_roots BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=OLD.root_id;
END;
CREATE TRIGGER sync_observation_deletion_insert AFTER INSERT ON sync_deletion_sets BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_deletion_update AFTER UPDATE ON sync_deletion_sets BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_deletion_delete AFTER DELETE ON sync_deletion_sets BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=OLD.root_id;
END;
CREATE TRIGGER sync_observation_member_update AFTER UPDATE ON sync_deletion_members BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_object_insert AFTER INSERT ON sync_objects BEGIN
 UPDATE sync_observations SET revision=revision+1;
END;
CREATE TRIGGER sync_observation_object_update AFTER UPDATE ON sync_objects BEGIN
 UPDATE sync_observations SET revision=revision+1;
END;
CREATE TRIGGER sync_observation_object_delete AFTER DELETE ON sync_objects BEGIN
 UPDATE sync_observations SET revision=revision+1;
END;
PRAGMA user_version=41;
