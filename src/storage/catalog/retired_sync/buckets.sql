CREATE TABLE sync_buckets (
    root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
    bucket TEXT NOT NULL COLLATE BINARY,
    incarnation TEXT NOT NULL CHECK(length(incarnation) = 32),
    PRIMARY KEY(root_id,bucket)
) WITHOUT ROWID;
PRAGMA user_version = 27;
