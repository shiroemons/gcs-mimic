CREATE TABLE catalog_identity (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    id BLOB NOT NULL CHECK(typeof(id) = 'blob' AND length(id) = 16)
) WITHOUT ROWID;
