use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[allow(dead_code)]
pub fn downgrade_object_schema(connection: &rusqlite::Connection) {
    downgrade_timestamp_precision(connection);
    connection.execute_batch("ALTER TABLE buckets DROP COLUMN public_access_prevention_enforced; ALTER TABLE buckets DROP COLUMN cors; DROP TABLE rewrite_sessions; DROP TABLE derived_deletions; DROP TABLE derived_inputs; DROP TABLE derived_transfers;
        ALTER TABLE objects RENAME TO objects_with_versions;
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
            PRIMARY KEY(bucket, name)
        ) WITHOUT ROWID;
        ALTER TABLE objects ADD COLUMN content_encoding TEXT;
        ALTER TABLE objects ADD COLUMN content_disposition TEXT;
        ALTER TABLE objects ADD COLUMN content_language TEXT;
        ALTER TABLE objects ADD COLUMN cache_control TEXT;
        ALTER TABLE objects ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';
        INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,content_encoding,content_disposition,content_language,cache_control,metadata) SELECT bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,content_encoding,content_disposition,content_language,cache_control,metadata FROM objects_with_versions;
        DROP TABLE objects_with_versions;").unwrap();
}

/// Remove the version 31 Autoclass column; fixtures built at older versions lack it.
pub fn downgrade_autoclass_schema(connection: &rusqlite::Connection) {
    let present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('buckets') WHERE name = 'autoclass')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if present {
        connection
            .execute_batch("ALTER TABLE buckets DROP COLUMN autoclass;")
            .unwrap();
    }
}

pub fn downgrade_soft_delete_schema(connection: &rusqlite::Connection) {
    downgrade_object_retention_schema(connection);
    connection
        .execute_batch("DROP TABLE IF EXISTS bucket_notifications; DROP TABLE IF EXISTS bucket_notification_ids; DROP TABLE IF EXISTS sync_buckets;")
        .unwrap();
    connection.execute_batch("DROP INDEX objects_soft_deleted; DROP INDEX objects_soft_expiry; ALTER TABLE objects DROP COLUMN soft_delete_time; ALTER TABLE objects DROP COLUMN hard_delete_time; ALTER TABLE buckets DROP COLUMN soft_delete_policy;").unwrap();
}

/// Restores the schema 28 layout; retain_until_time's CHECK names retention_mode, so it goes first.
pub fn downgrade_object_retention_schema(connection: &rusqlite::Connection) {
    downgrade_customer_key_schema(connection);
    connection
        .execute_batch("ALTER TABLE objects DROP COLUMN retain_until_time; ALTER TABLE objects DROP COLUMN retention_mode; ALTER TABLE buckets DROP COLUMN object_retention_enabled;")
        .unwrap();
}

/// Restores the schema 29 layout, which predates customer-supplied key fingerprints.
pub fn downgrade_customer_key_schema(connection: &rusqlite::Connection) {
    downgrade_autoclass_schema(connection);
    connection
        .execute_batch("ALTER TABLE objects DROP COLUMN customer_key_sha256;")
        .unwrap();
}

pub fn downgrade_timestamp_precision(connection: &rusqlite::Connection) {
    downgrade_soft_delete_schema(connection);
    connection
        .execute_batch("ALTER TABLE buckets DROP COLUMN retention_policy; ALTER TABLE buckets DROP COLUMN lifecycle; ALTER TABLE buckets DROP COLUMN uniform_bucket_level_access_locked_time;")
        .unwrap();
    connection.execute_batch("ALTER TABLE objects DROP COLUMN temporary_hold; ALTER TABLE objects DROP COLUMN custom_time;").unwrap();
    connection
        .execute_batch(
            "DROP TABLE IF EXISTS sync_findings; DROP TABLE IF EXISTS sync_writeback_ops; DROP TABLE IF EXISTS sync_writeback_roots; DROP TABLE IF EXISTS sync_roots; DROP TABLE IF EXISTS sync_objects; DROP TABLE IF EXISTS catalog_identity; DROP TABLE IF EXISTS import_observations; DROP TABLE IF EXISTS import_journal; DROP TABLE IF EXISTS change_journal; DROP TABLE IF EXISTS change_journal_state;",
        )
        .unwrap();
    connection.execute_batch(
        "UPDATE buckets SET time_created = time_created / 1000 - (time_created % 1000 < 0), updated = updated / 1000 - (updated % 1000 < 0);
         UPDATE objects SET time_created = time_created / 1000 - (time_created % 1000 < 0), updated = updated / 1000 - (updated % 1000 < 0), time_deleted = time_deleted / 1000 - (time_deleted % 1000 < 0);"
    ).unwrap();
}

pub fn floor_to_millis(time: SystemTime) -> SystemTime {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => UNIX_EPOCH + Duration::from_millis(duration.as_millis() as u64),
        Err(error) => {
            let millis = error.duration().as_nanos().div_ceil(1_000_000);
            UNIX_EPOCH - Duration::from_millis(millis as u64)
        }
    }
}
