use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) fn downgrade_object_schema(connection: &rusqlite::Connection) {
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

/// Restores the schema 33 layout, which predates Rapid appendable objects.
pub(crate) fn downgrade_appendable_schema(connection: &rusqlite::Connection) {
    downgrade_upload_replay_schema(connection);
    connection
        .execute_batch("DROP TABLE IF EXISTS appendable_objects;")
        .unwrap();
}

/// Restores the schema 34 layout, which predates upload response replay.
pub(crate) fn downgrade_upload_replay_schema(connection: &rusqlite::Connection) {
    downgrade_md5_availability_schema(connection);
    connection
        .execute_batch("DROP TABLE IF EXISTS upload_replays;")
        .unwrap();
}

/// Restores schema 35, before explicit MD5 availability and XML multipart sessions.
pub(crate) fn downgrade_md5_availability_schema(connection: &rusqlite::Connection) {
    downgrade_bucket_timestamp_schema(connection);
    connection.execute_batch("DROP TABLE IF EXISTS notification_outbox; DROP TABLE IF EXISTS notification_outbox_state;").unwrap();
    connection.execute_batch("DROP TABLE IF EXISTS xml_multipart_parts; DROP TABLE IF EXISTS xml_multipart_uploads; DROP TABLE IF EXISTS xml_multipart_deletions;").unwrap();
    let present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('objects') WHERE name='md5_available')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if present {
        connection
            .execute_batch("ALTER TABLE objects DROP COLUMN md5_available;")
            .unwrap();
    }
}

pub(crate) fn downgrade_deletion_schema(connection: &rusqlite::Connection) {
    downgrade_observation_schema(connection);
    connection.execute_batch("DROP TABLE IF EXISTS sync_deletion_members; DROP TABLE IF EXISTS sync_deletion_sets; DROP TABLE IF EXISTS sync_finding_owners;").unwrap();
    let owner:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('sync_findings') WHERE name='owner_project')",[],|row|row.get(0)).unwrap();
    if owner {
        connection
            .execute_batch("ALTER TABLE sync_findings DROP COLUMN owner_project;")
            .unwrap();
    }
}

pub(crate) fn downgrade_observation_schema(connection: &rusqlite::Connection) {
    downgrade_conflict_schema(connection);
    let names=connection.prepare("SELECT name FROM sqlite_schema WHERE type='trigger' AND name GLOB 'sync_observation_*'").unwrap().query_map([],|row|row.get::<_,String>(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    for name in names {
        connection
            .execute_batch(&format!("DROP TRIGGER \"{name}\";"))
            .unwrap()
    }
    connection
        .execute_batch(
            "DROP TABLE IF EXISTS sync_path_stops; DROP TABLE IF EXISTS sync_observations;",
        )
        .unwrap();
}

/// Restores schema 51, before buckets recorded the initialization file entry they came from.
pub(crate) fn downgrade_bucket_file_entries_schema(connection: &rusqlite::Connection) {
    downgrade_storage_class_updated_schema(connection);
    connection
        .execute_batch("DROP TABLE IF EXISTS bucket_file_entries;")
        .unwrap();
}

/// Keeps the findings, their owners and the findings' sequence row aside; drops the table schema
/// 60 rebuilt; replays the statements schema 58 ran for the table, its index and its triggers,
/// word for word, mapping an occupied directory back to the `collision` schemas 58 and 59
/// recorded it with; and puts the rows back before the triggers exist, so the observation
/// revision does not move.
const OCCUPIED_DOWNGRADE: &str = "CREATE TEMP TABLE downgrade_occupied_findings AS SELECT ordinal,root_id,bucket,name,incarnation,generation,CASE reason WHEN 'directory-occupied' THEN 'collision' ELSE reason END AS reason,owner_project FROM sync_findings;
CREATE TEMP TABLE downgrade_occupied_owners AS SELECT ordinal,incarnation,owner_project FROM sync_finding_owners;
CREATE TEMP TABLE downgrade_occupied_sequences AS SELECT name,seq FROM sqlite_sequence WHERE name='sync_findings';
DELETE FROM sync_finding_owners;
DROP TABLE sync_findings;
CREATE TABLE sync_findings (
 ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
 root_id BLOB NOT NULL REFERENCES sync_roots(root_id),
 bucket TEXT NOT NULL COLLATE BINARY,
 name TEXT NOT NULL COLLATE BINARY,
 incarnation TEXT NOT NULL,
 generation INTEGER NOT NULL CHECK(generation > 0),
 reason TEXT NOT NULL CHECK(reason IN ('invalid-name','host-restriction','reserved','ignored','collision','unsafe-entry','export-conflict','unclassified-collision','alias')),
 owner_project TEXT CHECK(owner_project IS NULL OR length(owner_project) BETWEEN 1 AND 256),
 UNIQUE(root_id,bucket,name)
);
INSERT INTO sync_findings(ordinal,root_id,bucket,name,incarnation,generation,reason,owner_project) SELECT ordinal,root_id,bucket,name,incarnation,generation,reason,owner_project FROM temp.downgrade_occupied_findings;
INSERT INTO sync_finding_owners(ordinal,incarnation,owner_project) SELECT ordinal,incarnation,owner_project FROM temp.downgrade_occupied_owners;
CREATE INDEX sync_findings_page ON sync_findings(root_id,ordinal);
CREATE TRIGGER sync_observation_finding_insert AFTER INSERT ON sync_findings BEGIN
 UPDATE sync_observations SET revision=revision+1,mapping_findings_revision=mapping_findings_revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_finding_update AFTER UPDATE ON sync_findings BEGIN
 UPDATE sync_observations SET revision=revision+1,mapping_findings_revision=mapping_findings_revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_finding_delete AFTER DELETE ON sync_findings BEGIN
 UPDATE sync_observations SET revision=revision+1,mapping_findings_revision=mapping_findings_revision+1 WHERE root_id=OLD.root_id;
END;
UPDATE sqlite_sequence SET seq=max(seq,coalesce((SELECT kept.seq FROM temp.downgrade_occupied_sequences kept WHERE kept.name=sqlite_sequence.name),0)) WHERE name='sync_findings';
INSERT INTO sqlite_sequence(name,seq) SELECT kept.name,kept.seq FROM temp.downgrade_occupied_sequences kept WHERE NOT EXISTS(SELECT 1 FROM sqlite_sequence present WHERE present.name=kept.name);
DROP TABLE temp.downgrade_occupied_findings;
DROP TABLE temp.downgrade_occupied_owners;
DROP TABLE temp.downgrade_occupied_sequences;";

/// Restores the schema before the files of a set waiting for a decision were kept apart from it.
pub(super) fn downgrade_held_schema(connection: &rusqlite::Connection) {
    connection
        .execute_batch("DROP TABLE IF EXISTS sync_deletion_held;")
        .unwrap();
}

/// Restores schema 59, before an occupied directory had a finding reason of its own, when the
/// findings table admits one. A catalog that keeps the files of a waiting set apart drops
/// them first.
fn downgrade_occupied_schema(connection: &rusqlite::Connection) {
    downgrade_held_schema(connection);
    let occupied: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sync_findings' AND sql LIKE '%directory-occupied%')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if occupied {
        connection.execute_batch(OCCUPIED_DOWNGRADE).unwrap();
    }
}

/// Keeps the rows of the two rebuilt tables, the finding owners and both sequence rows aside;
/// drops what schema 58 created; replays the statements schemas 21, 40 and 41 ran for these
/// tables and triggers, word for word; and puts the rows back before the triggers exist, so
/// the observation revision does not move.
const FINDING_REASON_DOWNGRADE: &str = "CREATE TEMP TABLE downgrade_findings AS SELECT ordinal,root_id,bucket,name,incarnation,generation,CASE WHEN reason IN ('export-conflict','unclassified-collision') THEN 'collision' ELSE reason END AS reason,owner_project FROM sync_findings;
CREATE TEMP TABLE downgrade_owners AS SELECT ordinal,incarnation,owner_project FROM sync_finding_owners;
CREATE TEMP TABLE downgrade_stops AS SELECT ordinal,root_id,bucket,name,owner_project,incarnation,generation,reason FROM sync_path_stops;
CREATE TEMP TABLE downgrade_sequences AS SELECT name,seq FROM sqlite_sequence WHERE name IN ('sync_findings','sync_path_stops');
DELETE FROM sync_finding_owners;
DROP TABLE sync_findings;
DROP TABLE sync_path_stops;
DROP TRIGGER sync_observation_root_insert;
ALTER TABLE sync_observations DROP COLUMN mapping_findings_revision;
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
ALTER TABLE sync_findings ADD COLUMN owner_project TEXT CHECK(owner_project IS NULL OR length(owner_project) BETWEEN 1 AND 256);
CREATE TABLE sync_path_stops (
 ordinal INTEGER PRIMARY KEY AUTOINCREMENT,
 root_id BLOB NOT NULL REFERENCES sync_roots(root_id) ON DELETE CASCADE,
 bucket TEXT NOT NULL COLLATE BINARY,
 name TEXT NOT NULL COLLATE BINARY,
 owner_project TEXT NOT NULL CHECK(length(owner_project) BETWEEN 1 AND 256),
 incarnation TEXT NOT NULL CHECK(length(incarnation)=32),
 generation INTEGER NOT NULL CHECK(generation>=0),
 reason TEXT NOT NULL CHECK(reason IN ('ProtectedDeletion','SourceChanging','IoFailure')),
 UNIQUE(root_id,bucket,name,incarnation,reason)
);
CREATE INDEX sync_path_stops_page ON sync_path_stops(root_id,ordinal);
INSERT INTO sync_findings(ordinal,root_id,bucket,name,incarnation,generation,reason,owner_project) SELECT ordinal,root_id,bucket,name,incarnation,generation,reason,owner_project FROM temp.downgrade_findings;
INSERT INTO sync_finding_owners(ordinal,incarnation,owner_project) SELECT ordinal,incarnation,owner_project FROM temp.downgrade_owners;
INSERT INTO sync_path_stops(ordinal,root_id,bucket,name,owner_project,incarnation,generation,reason) SELECT ordinal,root_id,bucket,name,owner_project,incarnation,generation,reason FROM temp.downgrade_stops;
UPDATE sqlite_sequence SET seq=max(seq,coalesce((SELECT kept.seq FROM temp.downgrade_sequences kept WHERE kept.name=sqlite_sequence.name),0)) WHERE name IN ('sync_findings','sync_path_stops');
INSERT INTO sqlite_sequence(name,seq) SELECT kept.name,kept.seq FROM temp.downgrade_sequences kept WHERE NOT EXISTS(SELECT 1 FROM sqlite_sequence present WHERE present.name=kept.name);
CREATE TRIGGER sync_observation_stop_insert AFTER INSERT ON sync_path_stops BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_stop_update AFTER UPDATE ON sync_path_stops BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_stop_delete AFTER DELETE ON sync_path_stops BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=OLD.root_id;
END;
CREATE TRIGGER sync_observation_root_insert AFTER INSERT ON sync_roots BEGIN
 INSERT INTO sync_observations VALUES(NEW.root_id,0,NULL,NULL);
END;
CREATE TRIGGER sync_observation_finding_insert AFTER INSERT ON sync_findings BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_finding_update AFTER UPDATE ON sync_findings BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_finding_delete AFTER DELETE ON sync_findings BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=OLD.root_id;
END;
DROP TABLE temp.downgrade_findings;
DROP TABLE temp.downgrade_owners;
DROP TABLE temp.downgrade_stops;
DROP TABLE temp.downgrade_sequences;";

/// Replays the triggers schema 41 created and schema 59 dropped, word for word.
const OBSERVATION_REVISION_DOWNGRADE: &str =
    "CREATE TRIGGER sync_observation_root_update AFTER UPDATE ON sync_roots BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_deletion_insert AFTER INSERT ON sync_deletion_sets BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_deletion_update AFTER UPDATE ON sync_deletion_sets BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_deletion_delete AFTER DELETE ON sync_deletion_sets BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=OLD.root_id;
END;
CREATE TRIGGER sync_observation_member_update AFTER UPDATE ON sync_deletion_members BEGIN
 UPDATE sync_observations SET revision=revision+1 WHERE root_id=NEW.root_id;
END;
CREATE TRIGGER sync_observation_object_insert AFTER INSERT ON sync_objects BEGIN
 UPDATE sync_observations SET revision=revision+1;
END;
CREATE TRIGGER sync_observation_object_update AFTER UPDATE ON sync_objects BEGIN
 UPDATE sync_observations SET revision=revision+1;
END;
CREATE TRIGGER sync_observation_object_delete AFTER DELETE ON sync_objects BEGIN
 UPDATE sync_observations SET revision=revision+1;
END;";

/// Restores schema 58, whose observation revision also followed the baseline flag, the deletion
/// sets and the tracked files. Fixtures built before schema 41 have no observations, and those
/// built from 41 through 58 still hold the triggers. A schema 60 catalog goes back to 59 first.
pub(crate) fn downgrade_observation_revision_schema(connection: &rusqlite::Connection) {
    downgrade_occupied_schema(connection);
    let dropped: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sync_observations') AND NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'trigger' AND name = 'sync_observation_root_update')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if dropped {
        connection
            .execute_batch(OBSERVATION_REVISION_DOWNGRADE)
            .unwrap();
    }
}

/// Restores schema 57, before an export conflict had a finding reason apart from a name
/// collision and findings had a revision of their own. An export conflict and a collision
/// carried over from schema 57 go back to `collision`, the reason schema 57 recorded both with.
/// Fixtures hold no `alias` finding and no `EntryReplaced` or `AliasEdited` stop, which schema
/// 57 cannot store.
pub(crate) fn downgrade_finding_reason_schema(connection: &rusqlite::Connection) {
    downgrade_observation_revision_schema(connection);
    let present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('sync_observations') WHERE name = 'mapping_findings_revision')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if present {
        connection.execute_batch(FINDING_REASON_DOWNGRADE).unwrap();
    }
}

/// Restores schema 56, before objects kept the time their event-based hold was released.
pub(crate) fn downgrade_event_hold_release_schema(connection: &rusqlite::Connection) {
    downgrade_finding_reason_schema(connection);
    let present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('objects') WHERE name = 'event_based_hold_release_time')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if present {
        connection
            .execute_batch("ALTER TABLE objects DROP COLUMN event_based_hold_release_time;")
            .unwrap();
    }
}

/// Restores schema 55, before sessions kept a running digest. Fixtures built before schema 7
/// have no session table, so the column is not there to drop.
pub(crate) fn downgrade_running_hash_schema(connection: &rusqlite::Connection) {
    downgrade_event_hold_release_schema(connection);
    let present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('resumable_sessions') WHERE name = 'running_hash')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if present {
        connection
            .execute_batch("ALTER TABLE resumable_sessions DROP COLUMN running_hash;")
            .unwrap();
    }
}

/// Restores schema 53, before multipart upload and part times kept microseconds. Fixtures built
/// before schema 37 have no multipart tables, so neither column is there to drop.
pub(crate) fn downgrade_multipart_micros_schema(connection: &rusqlite::Connection) {
    downgrade_running_hash_schema(connection);
    for (table, column) in [
        ("xml_multipart_uploads", "created_sub_milli_micros"),
        ("xml_multipart_parts", "updated_sub_milli_micros"),
    ] {
        let present: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2)",
                [table, column],
                |row| row.get(0),
            )
            .unwrap();
        if present {
            connection
                .execute_batch(&format!("ALTER TABLE {table} DROP COLUMN {column};"))
                .unwrap();
        }
    }
}

/// Restores schema 52; older fixtures may already lack the timestamp column.
pub(crate) fn downgrade_storage_class_updated_schema(connection: &rusqlite::Connection) {
    downgrade_multipart_micros_schema(connection);
    let present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('objects') WHERE name = 'time_storage_class_updated')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if present {
        connection
            .execute_batch("ALTER TABLE objects DROP COLUMN time_storage_class_updated;")
            .unwrap();
    }
}

/// Restores the schema 50 policy table, whose bindings are checked at 64 KiB. Fixtures built at
/// older versions may have no such table, or already have this one.
pub(crate) fn downgrade_policy_bindings_schema(connection: &rusqlite::Connection) {
    downgrade_bucket_file_entries_schema(connection);
    let rebuilt: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'bucket_policies' AND sql NOT LIKE '%65536%')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if rebuilt {
        connection
            .execute_batch(
                "ALTER TABLE bucket_policies RENAME TO bucket_policies_unchecked;
                CREATE TABLE bucket_policies (
                    bucket TEXT PRIMARY KEY COLLATE BINARY,
                    bindings TEXT NOT NULL CHECK(length(CAST(bindings AS BLOB)) <= 65536),
                    revision INTEGER NOT NULL CHECK(revision > 0)
                ) WITHOUT ROWID;
                INSERT INTO bucket_policies SELECT * FROM bucket_policies_unchecked;
                DROP TABLE bucket_policies_unchecked;",
            )
            .unwrap();
    }
}

pub(crate) fn downgrade_deletion_history_schema(connection: &rusqlite::Connection) {
    downgrade_policy_bindings_schema(connection);
    connection.execute_batch("DROP TRIGGER IF EXISTS sync_deletion_history_root_insert; DROP TRIGGER IF EXISTS sync_deletion_history_baseline; DROP TABLE IF EXISTS sync_deletion_history;").unwrap();
}

/// Drops the schema 48 hold-state columns when present; their CHECKs name the hold columns.
pub(crate) fn drop_hold_state_columns(connection: &rusqlite::Connection) {
    downgrade_deletion_history_schema(connection);
    for table in ["buckets", "soft_deleted_buckets"] {
        let present: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name = 'versioning_configured')",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        if present {
            connection
                .execute_batch(&format!(
                    "ALTER TABLE {table} DROP COLUMN versioning_configured;"
                ))
                .unwrap();
        }
    }
    for column in ["temporary_hold_released", "event_based_hold_released"] {
        let present: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('objects') WHERE name = ?1)",
                [column],
                |row| row.get(0),
            )
            .unwrap();
        if present {
            connection
                .execute_batch(&format!("ALTER TABLE objects DROP COLUMN {column};"))
                .unwrap();
        }
    }
}

/// Drops the schema 47 sub-microsecond column when present; it must go before the retention
/// columns that its `CHECK` names.
pub(crate) fn drop_sub_micro_column(connection: &rusqlite::Connection) {
    let present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('objects') WHERE name='retain_until_sub_micro_nanos')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if present {
        connection
            .execute_batch("ALTER TABLE objects DROP COLUMN retain_until_sub_micro_nanos;")
            .unwrap();
    }
}

/// Restores the schema 44 layout, which predates bucket generations and soft-deleted buckets.
/// It also drops the schema 47 column, so every older layout can be rebuilt from the latest one.
pub(crate) fn downgrade_bucket_generation_schema(connection: &rusqlite::Connection) {
    drop_hold_state_columns(connection);
    drop_sub_micro_column(connection);
    connection
        .execute_batch(
            "DROP TABLE IF EXISTS soft_deleted_buckets; DROP TABLE IF EXISTS bucket_generation;",
        )
        .unwrap();
    let present: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('buckets') WHERE name='generation')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if present {
        connection
            .execute_batch("ALTER TABLE buckets DROP COLUMN generation;")
            .unwrap();
    }
}

pub(crate) fn downgrade_conflict_schema(connection: &rusqlite::Connection) {
    downgrade_bucket_generation_schema(connection);
    let owner: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('objects') WHERE name='owner')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if owner {
        connection
            .execute_batch("ALTER TABLE objects DROP COLUMN owner;")
            .unwrap();
    }
    let bound: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('buckets') WHERE name='acl_policy_bound')", [], |row| row.get(0)).unwrap();
    if bound {
        connection
            .execute_batch("ALTER TABLE buckets DROP COLUMN acl_policy_bound;")
            .unwrap();
    }
    let names=connection.prepare("SELECT name FROM sqlite_schema WHERE type='trigger' AND tbl_name IN ('sync_conflicts','sync_conflict_operations','sync_recovery_workspaces','sync_conflict_roots')").unwrap().query_map([],|row|row.get::<_,String>(0)).unwrap().collect::<rusqlite::Result<Vec<_>>>().unwrap();
    for name in names {
        connection
            .execute_batch(&format!("DROP TRIGGER \"{name}\";"))
            .unwrap()
    }
    connection.execute_batch("DROP TABLE IF EXISTS sync_conflict_operations; DROP TABLE IF EXISTS sync_recovery_workspaces; DROP TABLE IF EXISTS sync_conflicts; DROP TABLE IF EXISTS sync_conflict_roots;").unwrap();
}

pub(crate) fn downgrade_bucket_timestamp_schema(connection: &rusqlite::Connection) {
    downgrade_deletion_schema(connection);
    let present:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('buckets') WHERE name='time_created_nanos')",[],|row|row.get(0)).unwrap();
    if present {
        connection.execute_batch("ALTER TABLE buckets DROP COLUMN updated_nanos; ALTER TABLE buckets DROP COLUMN time_created_nanos;").unwrap();
    }
}

/// Restores the schema 32 layout, which predates stored access control lists.
pub(crate) fn downgrade_acl_schema(connection: &rusqlite::Connection) {
    downgrade_appendable_schema(connection);
    connection
        .execute_batch(
            "DROP TABLE IF EXISTS acl_object_entries; DROP TABLE IF EXISTS acl_bucket_entries;",
        )
        .unwrap();
}

/// Restores the schema 31 layout, which predates project HMAC keys.
pub(crate) fn downgrade_hmac_key_schema(connection: &rusqlite::Connection) {
    downgrade_acl_schema(connection);
    connection
        .execute_batch("DROP TABLE IF EXISTS hmac_keys;")
        .unwrap();
}

/// Remove the version 31 Autoclass column; fixtures built at older versions lack it.
pub(crate) fn downgrade_autoclass_schema(connection: &rusqlite::Connection) {
    downgrade_hmac_key_schema(connection);
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

pub(crate) fn downgrade_soft_delete_schema(connection: &rusqlite::Connection) {
    downgrade_object_retention_schema(connection);
    connection
        .execute_batch("DROP TABLE IF EXISTS bucket_notifications; DROP TABLE IF EXISTS bucket_notification_ids; DROP TABLE IF EXISTS sync_buckets;")
        .unwrap();
    connection.execute_batch("DROP INDEX objects_soft_deleted; DROP INDEX objects_soft_expiry; ALTER TABLE objects DROP COLUMN soft_delete_time; ALTER TABLE objects DROP COLUMN hard_delete_time; ALTER TABLE buckets DROP COLUMN soft_delete_policy;").unwrap();
}

/// Restores the schema 28 layout; each column dropped here is named by the CHECK of the next, so
/// the sub-microsecond column goes first, then retain_until_time, then retention_mode.
pub(crate) fn downgrade_object_retention_schema(connection: &rusqlite::Connection) {
    drop_hold_state_columns(connection);
    drop_sub_micro_column(connection);
    downgrade_customer_key_schema(connection);
    connection
        .execute_batch("ALTER TABLE objects DROP COLUMN retain_until_time; ALTER TABLE objects DROP COLUMN retention_mode; ALTER TABLE buckets DROP COLUMN object_retention_enabled;")
        .unwrap();
}

/// Restores the schema 29 layout, which predates customer-supplied key fingerprints.
pub(crate) fn downgrade_customer_key_schema(connection: &rusqlite::Connection) {
    downgrade_autoclass_schema(connection);
    connection
        .execute_batch("ALTER TABLE objects DROP COLUMN customer_key_sha256;")
        .unwrap();
}

pub(crate) fn downgrade_timestamp_precision(connection: &rusqlite::Connection) {
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

pub(crate) fn floor_to_millis(time: SystemTime) -> SystemTime {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => UNIX_EPOCH + Duration::from_millis(duration.as_millis() as u64),
        Err(error) => {
            let millis = error.duration().as_nanos().div_ceil(1_000_000);
            UNIX_EPOCH - Duration::from_millis(millis as u64)
        }
    }
}
