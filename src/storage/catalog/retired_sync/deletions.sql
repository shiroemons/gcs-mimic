CREATE TABLE sync_deletion_sets (
 root_id BLOB PRIMARY KEY REFERENCES sync_roots(root_id),
 revision TEXT NOT NULL UNIQUE CHECK(length(revision)=32),
 scan_revision TEXT NOT NULL CHECK(length(scan_revision)=32),
 sequence INTEGER NOT NULL CHECK(sequence>=0),
 epoch INTEGER NOT NULL CHECK(epoch>=0),
 execution_sequence INTEGER NOT NULL CHECK(execution_sequence>=sequence),
 baseline_count INTEGER NOT NULL CHECK(baseline_count>=0),
 candidate_count INTEGER NOT NULL CHECK(candidate_count BETWEEN 0 AND baseline_count),
 guard_enabled INTEGER NOT NULL CHECK(guard_enabled IN (0,1)),
 guarded INTEGER NOT NULL CHECK(guarded IN (0,1)),
 complete INTEGER NOT NULL CHECK(complete IN (0,1)),
 valid INTEGER NOT NULL CHECK(valid IN (0,1)),
 authorization INTEGER NOT NULL CHECK(authorization IN (0,1,2))
) WITHOUT ROWID;
CREATE TABLE sync_deletion_members (
 root_id BLOB NOT NULL REFERENCES sync_deletion_sets(root_id) ON DELETE CASCADE,
 ordinal INTEGER NOT NULL CHECK(ordinal>0),
 bucket TEXT NOT NULL COLLATE BINARY,
 name TEXT NOT NULL COLLATE BINARY,
 owner_project TEXT NOT NULL,
 incarnation TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(generation>0),
 path TEXT NOT NULL COLLATE BINARY,
 done INTEGER NOT NULL CHECK(done IN (0,1)),
 PRIMARY KEY(root_id,ordinal),UNIQUE(root_id,bucket,name)
) WITHOUT ROWID;
ALTER TABLE sync_findings ADD COLUMN owner_project TEXT CHECK(owner_project IS NULL OR length(owner_project) BETWEEN 1 AND 256);
UPDATE sync_findings SET owner_project=(SELECT b.project FROM buckets b JOIN bucket_incarnations i ON i.bucket=b.name WHERE b.name=sync_findings.bucket AND i.incarnation=sync_findings.incarnation);
CREATE TABLE sync_finding_owners (
 ordinal INTEGER NOT NULL REFERENCES sync_findings(ordinal) ON DELETE CASCADE,
 incarnation TEXT NOT NULL CHECK(length(incarnation)=32),
 owner_project TEXT CHECK(owner_project IS NULL OR length(owner_project) BETWEEN 1 AND 256),
 PRIMARY KEY(ordinal,incarnation)
) WITHOUT ROWID;
INSERT INTO sync_finding_owners SELECT ordinal,incarnation,owner_project FROM sync_findings;
PRAGMA user_version=40;
