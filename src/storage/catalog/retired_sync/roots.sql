CREATE TABLE sync_roots (
    root_id BLOB PRIMARY KEY CHECK(typeof(root_id) = 'blob' AND length(root_id) = 16),
    location BLOB NOT NULL UNIQUE CHECK(typeof(location) = 'blob'),
    device BLOB NOT NULL CHECK(typeof(device) = 'blob' AND length(device) = 8),
    inode BLOB NOT NULL CHECK(typeof(inode) = 'blob' AND length(inode) = 8),
    complete INTEGER NOT NULL CHECK(complete IN (0, 1)),
    UNIQUE(device, inode)
) WITHOUT ROWID;
PRAGMA user_version = 19;
