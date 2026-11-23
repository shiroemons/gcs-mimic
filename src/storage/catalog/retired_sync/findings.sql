CREATE TABLE sync_findings (
    ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
    root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY,
    incarnation TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK(generation > 0),
    reason TEXT NOT NULL CHECK(reason IN ('invalid-name','host-restriction','reserved','ignored','collision','unsafe-entry')),
    UNIQUE(root_id,bucket,name)
);
CREATE INDEX sync_findings_page ON sync_findings(root_id,ordinal);
PRAGMA user_version = 21;
