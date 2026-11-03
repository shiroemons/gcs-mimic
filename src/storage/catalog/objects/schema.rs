use super::*;

pub(in crate::storage::catalog) const SCHEMA: &str = "CREATE TABLE objects (
            bucket TEXT NOT NULL COLLATE BINARY,
            name TEXT NOT NULL COLLATE BINARY,
            generation INTEGER NOT NULL CHECK(generation > 0),
            metageneration INTEGER NOT NULL CHECK(metageneration > 0),
            content_type TEXT NOT NULL,
            size INTEGER NOT NULL CHECK(size >= 0),
            md5 BLOB NOT NULL CHECK(length(md5) = 16),
            crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
            storage_class TEXT NOT NULL,
            time_created INTEGER NOT NULL,
            updated INTEGER NOT NULL,
            blob_id TEXT NOT NULL UNIQUE,
            PRIMARY KEY(bucket, name)
         ) WITHOUT ROWID;
         CREATE TABLE pending_blob_deletions (blob_id TEXT PRIMARY KEY) WITHOUT ROWID;
         PRAGMA user_version = 2;";

pub(in crate::storage::catalog) const GENERATION_SCHEMA: &str = "CREATE TABLE object_generation (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    last_generation INTEGER NOT NULL CHECK(last_generation >= 0)
) WITHOUT ROWID;";

pub(in crate::storage::catalog) const METADATA_SCHEMA: &str =
    "ALTER TABLE objects ADD COLUMN content_encoding TEXT;
    ALTER TABLE objects ADD COLUMN content_disposition TEXT;
    ALTER TABLE objects ADD COLUMN content_language TEXT;
    ALTER TABLE objects ADD COLUMN cache_control TEXT;
    ALTER TABLE objects ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';
    PRAGMA user_version = 4;";

pub(in crate::storage::catalog) fn migrate_metadata(
    connection: &mut Connection,
) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(METADATA_SCHEMA)?;
    transaction.commit().map_err(unavailable)
}

pub(in crate::storage::catalog) const VERSIONING_SCHEMA: &str =
    "ALTER TABLE objects RENAME TO objects_before_versioning;
CREATE TABLE objects (
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY,
    generation INTEGER NOT NULL CHECK(generation > 0),
    metageneration INTEGER NOT NULL CHECK(metageneration > 0),
    content_type TEXT NOT NULL,
    size INTEGER NOT NULL CHECK(size >= 0),
    md5 BLOB NOT NULL CHECK(length(md5) = 16),
    crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
    storage_class TEXT NOT NULL,
    time_created INTEGER NOT NULL,
    updated INTEGER NOT NULL,
    blob_id TEXT NOT NULL UNIQUE,
    content_encoding TEXT,
    content_disposition TEXT,
    content_language TEXT,
    cache_control TEXT,
    metadata TEXT NOT NULL DEFAULT '{}',
    time_deleted INTEGER,
    event_based_hold INTEGER NOT NULL DEFAULT 0 CHECK(event_based_hold IN (0, 1)),
    PRIMARY KEY(bucket, name, generation)
) WITHOUT ROWID;
INSERT INTO objects (bucket, name, generation, metageneration, content_type, size,
    md5, crc32c, storage_class, time_created, updated, blob_id, content_encoding,
    content_disposition, content_language, cache_control, metadata, time_deleted,
    event_based_hold)
SELECT bucket, name, generation, metageneration, content_type, size, md5, crc32c,
    storage_class, time_created, updated, blob_id, content_encoding,
    content_disposition, content_language, cache_control, metadata, NULL, 0
FROM objects_before_versioning;
DROP TABLE objects_before_versioning;
CREATE UNIQUE INDEX objects_live ON objects(bucket, name) WHERE time_deleted IS NULL;
PRAGMA user_version = 9;";

pub(in crate::storage::catalog) fn migrate_versioning(
    connection: &mut Connection,
) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(VERSIONING_SCHEMA)?;
    transaction.commit().map_err(unavailable)
}

#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration at open"
)]
pub(in crate::storage::catalog) fn migrate(
    connection: &mut Connection,
) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(SCHEMA)?;
    transaction.execute_batch(GENERATION_SCHEMA)?;
    transaction.execute("INSERT INTO object_generation VALUES (1, 0)", [])?;
    transaction.pragma_update(None, "user_version", 3)?;
    transaction.commit().map_err(unavailable)
}
