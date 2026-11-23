CREATE TABLE seed_objects (
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY,
    owned_generation INTEGER CHECK(owned_generation IS NULL OR (typeof(owned_generation) = 'integer' AND owned_generation > 0)),
    size INTEGER NOT NULL CHECK(typeof(size) = 'integer' AND size >= 0),
    md5 BLOB NOT NULL CHECK(typeof(md5) = 'blob' AND length(md5) = 16),
    crc32c INTEGER NOT NULL CHECK(typeof(crc32c) = 'integer' AND crc32c BETWEEN 0 AND 4294967295),
    mtime_ns BLOB NOT NULL CHECK(typeof(mtime_ns) = 'blob' AND length(mtime_ns) = 16),
    ctime_ns BLOB NOT NULL CHECK(typeof(ctime_ns) = 'blob' AND length(ctime_ns) = 16),
    inode BLOB NOT NULL CHECK(typeof(inode) = 'blob' AND length(inode) = 8),
    observed_ns BLOB NOT NULL CHECK(typeof(observed_ns) = 'blob' AND length(observed_ns) = 16),
    absent_since_ns BLOB CHECK(absent_since_ns IS NULL OR (typeof(absent_since_ns) = 'blob' AND length(absent_since_ns) = 16)),
    PRIMARY KEY(bucket, name)
) WITHOUT ROWID;
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
