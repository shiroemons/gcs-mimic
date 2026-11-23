CREATE TABLE sync_objects (
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY,
    incarnation TEXT NOT NULL,
    path TEXT NOT NULL COLLATE BINARY CHECK(length(path) BETWEEN 1 AND 4096),
    generation INTEGER NOT NULL CHECK(typeof(generation) = 'integer' AND generation > 0),
    size INTEGER NOT NULL CHECK(typeof(size) = 'integer' AND size >= 0),
    mtime_ns BLOB NOT NULL CHECK(typeof(mtime_ns) = 'blob' AND length(mtime_ns) = 16),
    ctime_ns BLOB NOT NULL CHECK(typeof(ctime_ns) = 'blob' AND length(ctime_ns) = 16),
    inode BLOB NOT NULL CHECK(typeof(inode) = 'blob' AND length(inode) = 8),
    md5 BLOB NOT NULL CHECK(typeof(md5) = 'blob' AND length(md5) = 16),
    crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
    racy INTEGER NOT NULL CHECK(racy IN (0, 1)),
    PRIMARY KEY(bucket, name)
) WITHOUT ROWID;
CREATE UNIQUE INDEX sync_objects_path ON sync_objects(path);
PRAGMA user_version = 17;
