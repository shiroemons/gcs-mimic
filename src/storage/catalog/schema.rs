use super::*;

const BUCKET_VERSIONING_PRESENCE_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN versioning_configured INTEGER NOT NULL DEFAULT 0 CHECK(versioning_configured IN (0, 1));
ALTER TABLE soft_deleted_buckets ADD COLUMN versioning_configured INTEGER NOT NULL DEFAULT 0 CHECK(versioning_configured IN (0, 1));
PRAGMA user_version = 49;";
pub(super) const SOFT_DELETE_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN soft_delete_policy TEXT;
ALTER TABLE objects ADD COLUMN soft_delete_time INTEGER;
ALTER TABLE objects ADD COLUMN hard_delete_time INTEGER;
CREATE INDEX objects_soft_deleted ON objects(bucket, name, generation) WHERE soft_delete_time IS NOT NULL;
CREATE INDEX objects_soft_expiry ON objects(bucket, hard_delete_time) WHERE soft_delete_time IS NOT NULL;
PRAGMA user_version = 26;";
pub(super) const BUCKET_RETENTION_SCHEMA: &str =
    "ALTER TABLE buckets ADD COLUMN retention_policy TEXT; PRAGMA user_version = 25;";
pub(super) const BUCKET_LIFECYCLE_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN lifecycle TEXT NOT NULL DEFAULT '[]'; PRAGMA user_version = 24;";
pub(super) const BUCKET_UNIFORM_ACCESS_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN uniform_bucket_level_access_locked_time INTEGER; PRAGMA user_version = 23;";
pub(super) const OBJECT_HOLDS_CUSTOM_TIME_SCHEMA: &str = "ALTER TABLE objects ADD COLUMN temporary_hold INTEGER NOT NULL DEFAULT 0 CHECK(temporary_hold IN (0, 1)); ALTER TABLE objects ADD COLUMN custom_time TEXT; PRAGMA user_version = 22;";
const TIMESTAMP_MICROS_SCHEMA_VERSION: i64 = 14;
const SEED_SCHEMA: &str = "CREATE TABLE startup_seed (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    complete INTEGER NOT NULL CHECK(complete IN (0, 1))
) WITHOUT ROWID;
PRAGMA user_version = 5;";
const BUCKET_PAP_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN public_access_prevention_enforced INTEGER NOT NULL DEFAULT 0 CHECK(public_access_prevention_enforced IN (0, 1));
PRAGMA user_version = 13;";
const BUCKET_CORS_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN cors TEXT NOT NULL DEFAULT '[]';
PRAGMA user_version = 12;";
pub(super) const BUCKET_SETTINGS_SCHEMA: &str = "ALTER TABLE buckets ADD COLUMN versioning_enabled INTEGER NOT NULL DEFAULT 0 CHECK(versioning_enabled IN (0, 1));
ALTER TABLE buckets ADD COLUMN default_event_based_hold INTEGER NOT NULL DEFAULT 0 CHECK(default_event_based_hold IN (0, 1));
PRAGMA user_version = 8;";
pub(super) const BUCKET_SCHEMA: &str = "CREATE TABLE buckets (
                        name TEXT PRIMARY KEY COLLATE BINARY,
                        project TEXT NOT NULL,
                        location TEXT NOT NULL,
                        location_type TEXT NOT NULL,
                        storage_class TEXT NOT NULL,
                        labels TEXT NOT NULL,
                        metageneration INTEGER NOT NULL,
                        time_created INTEGER NOT NULL,
                        updated INTEGER NOT NULL
                     ) WITHOUT ROWID;
                     CREATE INDEX buckets_project_name ON buckets(project, name);
                     PRAGMA user_version = 1;";

#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration at open"
)]
pub(super) fn migrate_timestamp_precision(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    // SQLite promotes overflowing integer multiplication to REAL, losing precision.
    macro_rules! unconvertible {
        ($column:literal) => {
            concat!(
                "(",
                $column,
                " IS NOT NULL AND (typeof(",
                $column,
                ") != 'integer' OR ",
                $column,
                " NOT BETWEEN -9223372036854775 AND 9223372036854775))"
            )
        };
    }
    for (table, check) in [
        (
            "buckets",
            concat!(
                "SELECT EXISTS(SELECT 1 FROM buckets WHERE ",
                unconvertible!("time_created"),
                " OR ",
                unconvertible!("updated"),
                ")"
            ),
        ),
        (
            "objects",
            concat!(
                "SELECT EXISTS(SELECT 1 FROM objects WHERE ",
                unconvertible!("time_created"),
                " OR ",
                unconvertible!("updated"),
                " OR ",
                unconvertible!("time_deleted"),
                ")"
            ),
        ),
    ] {
        let invalid: bool = transaction.query_row(check, [], |row| row.get(0))?;
        if invalid {
            return Err(unavailable(format!(
                "{table} timestamps cannot be converted to integer Unix microseconds"
            )));
        }
    }
    transaction.execute_batch(
        "UPDATE buckets SET time_created = time_created * 1000, updated = updated * 1000;
         UPDATE objects SET time_created = time_created * 1000, updated = updated * 1000, time_deleted = time_deleted * 1000;",
    )?;
    transaction.pragma_update(None, "user_version", TIMESTAMP_MICROS_SCHEMA_VERSION)?;
    transaction.commit().map_err(unavailable)
}

pub(super) fn validate_schema_version(version: i64, supported: i64) -> Result<(), CatalogError> {
    if version > supported {
        return Err(CatalogError::NewerSchema { version, supported });
    }
    if version < 0 {
        return Err(unavailable(format!(
            "unsupported catalog schema version {version}; supported version is {supported}"
        )));
    }
    Ok(())
}

type StateCheck = fn(&Connection) -> Result<(), CatalogError>;

/// Each schema version's statements, applied in order to build the expected catalog, and the
/// stored-state check that version introduced.
const SCHEMA_STEPS: &[(i64, &str, Option<StateCheck>)] = &[
    (2, objects::SCHEMA, None),
    (3, objects::GENERATION_SCHEMA, None),
    (4, objects::METADATA_SCHEMA, None),
    (5, SEED_SCHEMA, Some(validate_seed)),
    (6, policies::SCHEMA, Some(policies::validate)),
    (7, resumable::SCHEMA, Some(resumable::validate)),
    (8, BUCKET_SETTINGS_SCHEMA, None),
    (9, objects::VERSIONING_SCHEMA, None),
    (10, derived::SCHEMA, None),
    (11, rewrite::SCHEMA, Some(rewrite::validate)),
    (12, BUCKET_CORS_SCHEMA, None),
    (13, BUCKET_PAP_SCHEMA, None),
    (15, change_journal::SCHEMA, Some(change_journal::validate)),
    (16, import_journal::SCHEMA, None),
    (17, sync_state::SCHEMA, Some(sync_state::validate)),
    (
        18,
        import_journal::FAILURE_SCHEMA,
        Some(import_journal::validate_failure),
    ),
    (19, sync_state::root::SCHEMA, None),
    (20, sync_state::writeback::SCHEMA, None),
    (21, sync_state::findings::SCHEMA, None),
    (22, OBJECT_HOLDS_CUSTOM_TIME_SCHEMA, None),
    (23, BUCKET_UNIFORM_ACCESS_SCHEMA, None),
    (24, BUCKET_LIFECYCLE_SCHEMA, None),
    (25, BUCKET_RETENTION_SCHEMA, None),
    (26, SOFT_DELETE_SCHEMA, None),
    (27, sync_state::buckets::SCHEMA, None),
    (28, notifications::SCHEMA, Some(notifications::validate)),
    (29, objects::retention::SCHEMA, None),
    (30, customer_keys::SCHEMA, None),
    (31, bucket_autoclass::SCHEMA, None),
    (32, hmac_keys::SCHEMA, None),
    (33, acl::SCHEMA, None),
    (34, appendable::SCHEMA, None),
    (35, upload_replay::SCHEMA, Some(upload_replay::validate)),
    (36, md5_availability::SCHEMA, None),
    (37, xml_multipart::SCHEMA, Some(xml_multipart::validate)),
    (
        38,
        notification_outbox::SCHEMA,
        Some(notification_outbox::validate),
    ),
    (39, bucket_timestamps::SCHEMA, None),
    (40, sync_state::deletions::SCHEMA, None),
    (41, sync_state::observation::SCHEMA, None),
    (42, sync_state::conflicts::SCHEMA, None),
    (43, acl::POLICY_BINDING_SCHEMA, None),
    (44, object_owner::SCHEMA, None),
    (45, bucket_soft_delete::SCHEMA, None),
    (46, bucket_soft_delete::GENERATION_SCHEMA, None),
    (47, objects::retention::SUB_MICRO_SCHEMA, None),
    (48, object_hold::SCHEMA, None),
    (49, BUCKET_VERSIONING_PRESENCE_SCHEMA, None),
    (50, sync_state::deletions::history::SCHEMA, None),
    (51, policies::UNBOUNDED_BINDINGS_SCHEMA, None),
];

#[expect(
    clippy::disallowed_methods,
    reason = "schema validation runs once at open"
)]
fn validate_seed(connection: &Connection) -> Result<(), CatalogError> {
    let valid: bool = connection.query_row(
        "SELECT count(*) = 1 AND min(singleton) = 1 FROM startup_seed",
        [],
        |r| r.get(0),
    )?;
    if valid {
        Ok(())
    } else {
        Err(unavailable("invalid startup seed state"))
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "schema validation runs once at open"
)]
pub(super) fn validate_schema(connection: &Connection, version: i64) -> Result<(), CatalogError> {
    let expected = Connection::open_in_memory()?;
    expected.execute_batch(BUCKET_SCHEMA)?;
    for &(introduced, schema, check) in SCHEMA_STEPS {
        if version < introduced {
            break;
        }
        expected.execute_batch(schema)?;
        if let Some(check) = check {
            check(connection)?;
        }
        // Version 18 replaced the import journal's validation with the failure journal's.
        if introduced == 16 && version < 18 {
            import_journal::validate(connection)?;
        }
    }
    let inventory =
        |db: &Connection| -> Result<Vec<(String, String, String, String)>, CatalogError> {
            db.prepare_cached("SELECT type, name, tbl_name, substr(sql, 1, 16385) FROM sqlite_schema WHERE name NOT GLOB 'sqlite_*' ORDER BY name LIMIT 257")?.query_map([], |row| {
                let sql: String = row.get(3)?;
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, sql.split_whitespace().collect::<Vec<_>>().join(" ")))
            })?.collect::<rusqlite::Result<_>>().map_err(unavailable)
        };
    // Versioned catalogs must retain our constraints, collations, indexes and table inventory.
    let actual = inventory(connection)?;
    if actual.len() > 256 || actual != inventory(&expected)? {
        return Err(unavailable("catalog schema does not match its version"));
    }
    if version >= 3 {
        let valid: bool = connection.query_row("SELECT count(*) = 1 AND min(singleton) = 1 AND min(last_generation) >= (SELECT coalesce(max(generation), 0) FROM objects) FROM object_generation", [], |row| row.get(0))?;
        if !valid {
            return Err(unavailable("invalid generation allocator state"));
        }
    }
    Ok(())
}

/// Creates a new catalog at schema version 5, the base the migrations continue from.
#[expect(
    clippy::disallowed_methods,
    reason = "creates a new catalog once at open"
)]
pub(super) fn initialize_catalog(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(BUCKET_SCHEMA)?;
    // Freshness and the retryable seed state must survive the same commit.
    for schema in [
        objects::SCHEMA,
        objects::GENERATION_SCHEMA,
        objects::METADATA_SCHEMA,
        SEED_SCHEMA,
    ] {
        transaction.execute_batch(schema)?;
    }
    transaction.execute("INSERT INTO object_generation VALUES (1, 0)", [])?;
    transaction.execute("INSERT INTO startup_seed VALUES (1, 0)", [])?;
    transaction.commit().map_err(unavailable)
}

#[expect(
    clippy::disallowed_methods,
    reason = "a one-time schema migration at open"
)]
fn migrate_seed(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction()?;
    transaction.execute_batch(SEED_SCHEMA)?;
    // Older catalogs have already served user mutations, even if now empty.
    transaction.execute("INSERT INTO startup_seed VALUES (1, 1)", [])?;
    transaction.commit().map_err(unavailable)
}

/// One schema upgrade: statements committed in one transaction, or a migration function.
pub(super) enum Migration {
    Statements(&'static str),
    Function(fn(&mut Connection) -> Result<(), CatalogError>),
}

impl Migration {
    pub(super) fn apply(&self, connection: &mut Connection) -> Result<(), CatalogError> {
        match self {
            Self::Statements(schema) => {
                let transaction = connection.transaction()?;
                transaction.execute_batch(schema)?;
                transaction.commit().map_err(unavailable)
            }
            Self::Function(migrate) => migrate(connection),
        }
    }
}

/// Upgrades in order; each applies when the catalog's version is below the paired version.
pub(super) const MIGRATIONS: &[(i64, Migration)] = &[
    (2, Migration::Function(objects::migrate)),
    (4, Migration::Function(objects::migrate_metadata)),
    (5, Migration::Function(migrate_seed)),
    (6, Migration::Function(policies::migrate)),
    (7, Migration::Function(resumable::migrate)),
    (8, Migration::Statements(BUCKET_SETTINGS_SCHEMA)),
    (9, Migration::Function(objects::migrate_versioning)),
    (10, Migration::Statements(derived::SCHEMA)),
    (11, Migration::Statements(rewrite::SCHEMA)),
    (12, Migration::Statements(BUCKET_CORS_SCHEMA)),
    (13, Migration::Statements(BUCKET_PAP_SCHEMA)),
    (
        TIMESTAMP_MICROS_SCHEMA_VERSION,
        Migration::Function(migrate_timestamp_precision),
    ),
    (15, Migration::Statements(change_journal::SCHEMA)),
    (16, Migration::Statements(import_journal::SCHEMA)),
    (17, Migration::Function(sync_state::migrate)),
    (18, Migration::Statements(import_journal::FAILURE_SCHEMA)),
    (19, Migration::Statements(sync_state::root::SCHEMA)),
    (20, Migration::Statements(sync_state::writeback::SCHEMA)),
    (21, Migration::Statements(sync_state::findings::SCHEMA)),
    (22, Migration::Statements(OBJECT_HOLDS_CUSTOM_TIME_SCHEMA)),
    (23, Migration::Statements(BUCKET_UNIFORM_ACCESS_SCHEMA)),
    (24, Migration::Statements(BUCKET_LIFECYCLE_SCHEMA)),
    (25, Migration::Statements(BUCKET_RETENTION_SCHEMA)),
    (26, Migration::Statements(SOFT_DELETE_SCHEMA)),
    (27, Migration::Statements(sync_state::buckets::SCHEMA)),
    (28, Migration::Statements(notifications::SCHEMA)),
    (29, Migration::Function(objects::retention::migrate)),
    (30, Migration::Function(customer_keys::migrate)),
    (31, Migration::Statements(bucket_autoclass::SCHEMA)),
    (32, Migration::Statements(hmac_keys::SCHEMA)),
    (33, Migration::Statements(acl::SCHEMA)),
    (34, Migration::Statements(appendable::SCHEMA)),
    (35, Migration::Statements(upload_replay::SCHEMA)),
    (36, Migration::Statements(md5_availability::SCHEMA)),
    (37, Migration::Statements(xml_multipart::SCHEMA)),
    (38, Migration::Statements(notification_outbox::SCHEMA)),
    (39, Migration::Statements(bucket_timestamps::SCHEMA)),
    (40, Migration::Statements(sync_state::deletions::SCHEMA)),
    (41, Migration::Statements(sync_state::observation::SCHEMA)),
    (42, Migration::Statements(sync_state::conflicts::SCHEMA)),
    (43, Migration::Statements(acl::POLICY_BINDING_SCHEMA)),
    (44, Migration::Statements(object_owner::SCHEMA)),
    (45, Migration::Statements(bucket_soft_delete::SCHEMA)),
    (
        46,
        Migration::Statements(bucket_soft_delete::GENERATION_SCHEMA),
    ),
    (
        47,
        Migration::Statements(objects::retention::SUB_MICRO_SCHEMA),
    ),
    (48, Migration::Statements(object_hold::SCHEMA)),
    (49, Migration::Statements(BUCKET_VERSIONING_PRESENCE_SCHEMA)),
    (
        50,
        Migration::Statements(sync_state::deletions::history::SCHEMA),
    ),
    (
        51,
        Migration::Statements(policies::UNBOUNDED_BINDINGS_SCHEMA),
    ),
];
