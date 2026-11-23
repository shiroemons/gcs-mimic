CREATE TABLE sync_writeback_roots (
    root_id BLOB PRIMARY KEY REFERENCES sync_roots(root_id),
    max_bytes INTEGER NOT NULL CHECK(max_bytes BETWEEN 1 AND 1099511627776),
    max_entries INTEGER NOT NULL CHECK(max_entries BETWEEN 1 AND 1000000),
    reserved BLOB NOT NULL CHECK(typeof(reserved) = 'blob' AND length(reserved) = 16),
    retained BLOB NOT NULL CHECK(typeof(retained) = 'blob' AND length(retained) = 16),
    entries INTEGER NOT NULL CHECK(entries BETWEEN 0 AND 1000000),
    protected INTEGER NOT NULL CHECK(protected BETWEEN 0 AND entries),
    recovery INTEGER NOT NULL CHECK(recovery IN (0,1)),
    io_failure INTEGER NOT NULL CHECK(io_failure IN (0,1)),
    workspace_device BLOB NOT NULL CHECK(typeof(workspace_device) = 'blob' AND length(workspace_device) = 8),
    workspace_inode BLOB NOT NULL CHECK(typeof(workspace_inode) = 'blob' AND length(workspace_inode) = 8),
    sequence INTEGER CHECK(sequence >= 0),
    epoch INTEGER CHECK(epoch >= 0),
    CHECK((sequence IS NULL) = (epoch IS NULL))
) WITHOUT ROWID;
CREATE TABLE sync_writeback_ops (
    ordinal INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE CHECK(length(id) = 32 AND id NOT GLOB '*[^0-9a-f]*'),
    root_id BLOB NOT NULL REFERENCES sync_writeback_roots(root_id),
    owner TEXT NOT NULL CHECK(length(owner) BETWEEN 1 AND 256),
    bucket TEXT NOT NULL CHECK(length(bucket) BETWEEN 1 AND 222),
    incarnation TEXT NOT NULL CHECK(length(incarnation) = 32),
    name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 1024),
    path TEXT NOT NULL CHECK(length(path) BETWEEN 1 AND 4096),
    generation INTEGER NOT NULL CHECK(generation > 0),
    kind INTEGER NOT NULL CHECK(kind IN (0,1)),
    phase INTEGER NOT NULL CHECK(phase BETWEEN 0 AND 4),
    revision INTEGER NOT NULL CHECK(revision > 0),
    io_failure INTEGER NOT NULL DEFAULT 0 CHECK(io_failure BETWEEN 0 AND 3),
    fence_sequence INTEGER NOT NULL CHECK(fence_sequence >= 0),
    fence_epoch INTEGER NOT NULL CHECK(fence_epoch >= 0),
    reservation BLOB NOT NULL CHECK(typeof(reservation) = 'blob' AND length(reservation) = 8),
    retained BLOB CHECK(retained IS NULL OR (typeof(retained) = 'blob' AND length(retained) = 8)),
    size INTEGER NOT NULL CHECK(size >= 0),
    md5 BLOB NOT NULL CHECK(typeof(md5) = 'blob' AND length(md5) = 16),
    crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
    snapshot BLOB CHECK(snapshot IS NULL OR length(snapshot) <= 4096)
);
CREATE UNIQUE INDEX sync_writeback_target ON sync_writeback_ops(root_id,path) WHERE phase != 4;
CREATE INDEX sync_writeback_page ON sync_writeback_ops(root_id,ordinal) WHERE phase != 4;
CREATE INDEX sync_writeback_phase ON sync_writeback_ops(root_id,phase);
PRAGMA user_version = 20;
