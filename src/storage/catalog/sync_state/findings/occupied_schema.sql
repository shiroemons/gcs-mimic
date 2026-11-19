CREATE TEMP TABLE sync_occupied_findings AS SELECT ordinal,root_id,bucket,name,incarnation,generation,reason,owner_project FROM sync_findings;
CREATE TEMP TABLE sync_occupied_owners AS SELECT ordinal,incarnation,owner_project FROM sync_finding_owners;
CREATE TEMP TABLE sync_occupied_sequences AS SELECT name,seq FROM sqlite_sequence WHERE name='sync_findings';
DELETE FROM sync_finding_owners;
DROP TABLE sync_findings;
CREATE TABLE sync_findings (
 ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
 root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
 bucket TEXT NOT NULL COLLATE BINARY,
 name TEXT NOT NULL COLLATE BINARY,
 incarnation TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(generation > 0),
 reason TEXT NOT NULL CHECK(reason IN ('invalid-name','host-restriction','reserved','ignored','collision','unsafe-entry','export-conflict','unclassified-collision','alias','directory-occupied')),
 owner_project TEXT CHECK(owner_project IS NULL OR length(owner_project) BETWEEN 1 AND 256),
 UNIQUE(root_id,bucket,name)
);
INSERT INTO sync_findings(ordinal,root_id,bucket,name,incarnation,generation,reason,owner_project) SELECT ordinal,root_id,bucket,name,incarnation,generation,reason,owner_project FROM temp.sync_occupied_findings;
INSERT INTO sync_finding_owners(ordinal,incarnation,owner_project) SELECT ordinal,incarnation,owner_project FROM temp.sync_occupied_owners;
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
UPDATE sqlite_sequence SET seq=max(seq,coalesce((SELECT kept.seq FROM temp.sync_occupied_sequences kept WHERE kept.name=sqlite_sequence.name),0)) WHERE name='sync_findings';
INSERT INTO sqlite_sequence(name,seq) SELECT kept.name,kept.seq FROM temp.sync_occupied_sequences kept WHERE NOT EXISTS(SELECT 1 FROM sqlite_sequence present WHERE present.name=kept.name);
DROP TABLE temp.sync_occupied_findings;
DROP TABLE temp.sync_occupied_owners;
DROP TABLE temp.sync_occupied_sequences;
PRAGMA user_version=60;
