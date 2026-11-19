CREATE TEMP TABLE sync_reason_findings AS SELECT ordinal,root_id,bucket,name,incarnation,generation,reason,owner_project FROM sync_findings;
CREATE TEMP TABLE sync_reason_owners AS SELECT ordinal,incarnation,owner_project FROM sync_finding_owners;
CREATE TEMP TABLE sync_reason_stops AS SELECT ordinal,root_id,bucket,name,owner_project,incarnation,generation,reason FROM sync_path_stops;
CREATE TEMP TABLE sync_reason_sequences AS SELECT name,seq FROM sqlite_sequence WHERE name IN ('sync_findings','sync_path_stops');
DELETE FROM sync_finding_owners;
DROP TABLE sync_findings;
DROP TABLE sync_path_stops;
DROP TRIGGER sync_observation_root_insert;
ALTER TABLE sync_observations ADD COLUMN mapping_findings_revision INTEGER NOT NULL DEFAULT 0 CHECK(typeof(mapping_findings_revision)='integer' AND mapping_findings_revision BETWEEN 0 AND 9223372036854775806);
CREATE TRIGGER sync_observation_root_insert AFTER INSERT ON sync_roots BEGIN
 INSERT INTO sync_observations(root_id,revision,paused_reason,scan_stop) VALUES(NEW.root_id,0,NULL,NULL);
END;
CREATE TABLE sync_findings (
 ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
 root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
 bucket TEXT NOT NULL COLLATE BINARY,
 name TEXT NOT NULL COLLATE BINARY,
 incarnation TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(generation > 0),
 reason TEXT NOT NULL CHECK(reason IN ('invalid-name','host-restriction','reserved','ignored','collision','unsafe-entry','export-conflict','alias')),
 owner_project TEXT CHECK(owner_project IS NULL OR length(owner_project) BETWEEN 1 AND 256),
 UNIQUE(root_id,bucket,name)
);
INSERT INTO sync_findings(ordinal,root_id,bucket,name,incarnation,generation,reason,owner_project) SELECT ordinal,root_id,bucket,name,incarnation,generation,CASE reason WHEN 'collision' THEN 'export-conflict' ELSE reason END,owner_project FROM temp.sync_reason_findings;
INSERT INTO sync_finding_owners(ordinal,incarnation,owner_project) SELECT ordinal,incarnation,owner_project FROM temp.sync_reason_owners;
CREATE INDEX sync_findings_page ON sync_findings(root_id,ordinal);
CREATE TRIGGER sync_observation_finding_insert AFTER INSERT ON sync_findings BEGIN
 UPDATE sync_observations SET revision=revision+1,mapping_findings_revision=mapping_findings_revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_finding_update AFTER UPDATE ON sync_findings BEGIN
 UPDATE sync_observations SET revision=revision+1,mapping_findings_revision=mapping_findings_revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_finding_delete AFTER DELETE ON sync_findings BEGIN
 UPDATE sync_observations SET revision=revision+1,mapping_findings_revision=mapping_findings_revision+1 WHERE root_id=OLD.root_id;
END;
CREATE TABLE sync_path_stops (
 ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
 root_id BLOB NOT NULL REFERENCES sync_roots(root_id) ON DELETE CASCADE,
 bucket TEXT NOT NULL COLLATE BINARY,
 name TEXT NOT NULL COLLATE BINARY,
 owner_project TEXT NOT NULL CHECK(length(owner_project) BETWEEN 1 AND 256),
 incarnation TEXT NOT NULL CHECK(length(incarnation)=32),
 generation INTEGER NOT NULL CHECK(generation>=0),
 reason TEXT NOT NULL CHECK(reason IN ('ProtectedDeletion','SourceChanging','IoFailure','EntryReplaced','AliasEdited')),
 UNIQUE(root_id,bucket,name,incarnation,reason)
);
INSERT INTO sync_path_stops(ordinal,root_id,bucket,name,owner_project,incarnation,generation,reason) SELECT ordinal,root_id,bucket,name,owner_project,incarnation,generation,reason FROM temp.sync_reason_stops;
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
UPDATE sqlite_sequence SET seq=max(seq,coalesce((SELECT kept.seq FROM temp.sync_reason_sequences kept WHERE kept.name=sqlite_sequence.name),0)) WHERE name IN ('sync_findings','sync_path_stops');
INSERT INTO sqlite_sequence(name,seq) SELECT kept.name,kept.seq FROM temp.sync_reason_sequences kept WHERE NOT EXISTS(SELECT 1 FROM sqlite_sequence present WHERE present.name=kept.name);
DROP TABLE temp.sync_reason_findings;
DROP TABLE temp.sync_reason_owners;
DROP TABLE temp.sync_reason_stops;
DROP TABLE temp.sync_reason_sequences;
PRAGMA user_version=58;
