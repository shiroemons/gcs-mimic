use super::*;

pub(super) const STORAGE_CLASS_UPDATED_SCHEMA: &str =
    "ALTER TABLE objects ADD COLUMN time_storage_class_updated INTEGER;
PRAGMA user_version = 53;";

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
type Migrate = fn(&mut Connection) -> Result<(), CatalogError>;

/// One schema version: the statements it adds to the expected catalog, the stored-state check it
/// introduced, and how an older catalog reaches it. Built only through the constructors below,
/// so a step's statements are named once.
pub(super) struct Step {
    pub(super) version: i64,
    /// `None` for a version that changes only stored data.
    schema: Option<&'static str>,
    check: Option<StateCheck>,
    /// `None` for a version an earlier step's migration writes itself.
    pub(super) migration: Option<Migration>,
}

impl Step {
    /// A version whose statements commit in one transaction.
    const fn statements(version: i64, schema: &'static str, check: Option<StateCheck>) -> Self {
        Self {
            version,
            schema: Some(schema),
            check,
            migration: Some(Migration::Statements(schema)),
        }
    }

    /// A version whose upgrade also moves data or seeds rows.
    const fn migrated(
        version: i64,
        schema: &'static str,
        check: Option<StateCheck>,
        migrate: Migrate,
    ) -> Self {
        Self {
            version,
            schema: Some(schema),
            check,
            migration: Some(Migration::Function(migrate)),
        }
    }

    /// Applied by the previous step's migration: `objects::migrate` creates versions 2 and 3
    /// together, and no release wrote version 2 alone.
    const fn applied_by_previous(version: i64, schema: &'static str) -> Self {
        Self {
            version,
            schema: Some(schema),
            check: None,
            migration: None,
        }
    }

    /// A version that changes only stored data, so the expected schema gains nothing.
    const fn data_only(version: i64, migrate: Migrate) -> Self {
        Self {
            version,
            schema: None,
            check: None,
            migration: Some(Migration::Function(migrate)),
        }
    }
}

/// Every schema version in order. Validation applies each step's statements up to a catalog's
/// version to build the schema it must have; an upgrade applies each migration above it.
pub(super) const STEPS: &[Step] = &[
    Step::migrated(2, objects::SCHEMA, None, objects::migrate),
    Step::applied_by_previous(3, objects::GENERATION_SCHEMA),
    Step::statements(4, objects::METADATA_SCHEMA, None),
    Step::migrated(5, SEED_SCHEMA, Some(validate_seed), migrate_seed),
    Step::migrated(
        6,
        policies::SCHEMA,
        Some(policies::validate),
        policies::migrate,
    ),
    Step::migrated(
        7,
        resumable::SCHEMA,
        Some(resumable::validate),
        resumable::migrate,
    ),
    Step::statements(8, BUCKET_SETTINGS_SCHEMA, None),
    Step::statements(9, objects::VERSIONING_SCHEMA, None),
    Step::statements(10, derived::SCHEMA, None),
    Step::statements(11, rewrite::SCHEMA, Some(rewrite::validate)),
    Step::statements(12, BUCKET_CORS_SCHEMA, None),
    Step::statements(13, BUCKET_PAP_SCHEMA, None),
    Step::data_only(TIMESTAMP_MICROS_SCHEMA_VERSION, migrate_timestamp_precision),
    Step::statements(15, change_journal::SCHEMA, Some(change_journal::validate)),
    Step::statements(16, import_journal::SCHEMA, None),
    Step::migrated(
        17,
        sync_state::SCHEMA,
        Some(sync_state::validate),
        sync_state::migrate,
    ),
    Step::statements(
        18,
        import_journal::FAILURE_SCHEMA,
        Some(import_journal::validate_failure),
    ),
    Step::statements(19, sync_state::root::SCHEMA, None),
    Step::statements(20, sync_state::writeback::SCHEMA, None),
    Step::statements(21, sync_state::findings::SCHEMA, None),
    Step::statements(22, OBJECT_HOLDS_CUSTOM_TIME_SCHEMA, None),
    Step::statements(23, BUCKET_UNIFORM_ACCESS_SCHEMA, None),
    Step::statements(24, BUCKET_LIFECYCLE_SCHEMA, None),
    Step::statements(25, BUCKET_RETENTION_SCHEMA, None),
    Step::statements(26, SOFT_DELETE_SCHEMA, None),
    Step::statements(27, sync_state::buckets::SCHEMA, None),
    Step::statements(28, notifications::SCHEMA, Some(notifications::validate)),
    Step::statements(29, objects::retention::SCHEMA, None),
    Step::statements(30, customer_keys::SCHEMA, None),
    Step::statements(31, bucket_autoclass::SCHEMA, None),
    Step::statements(32, hmac_keys::SCHEMA, None),
    Step::statements(33, acl::SCHEMA, None),
    Step::statements(34, appendable::SCHEMA, None),
    Step::statements(35, upload_replay::SCHEMA, Some(upload_replay::validate)),
    Step::statements(36, md5_availability::SCHEMA, None),
    Step::statements(37, xml_multipart::SCHEMA, Some(xml_multipart::validate)),
    Step::statements(
        38,
        notification_outbox::SCHEMA,
        Some(notification_outbox::validate),
    ),
    Step::statements(39, bucket_timestamps::SCHEMA, None),
    Step::statements(40, sync_state::deletions::SCHEMA, None),
    Step::statements(41, sync_state::observation::SCHEMA, None),
    Step::statements(42, sync_state::conflicts::SCHEMA, None),
    Step::statements(43, acl::POLICY_BINDING_SCHEMA, None),
    Step::statements(44, object_owner::SCHEMA, None),
    Step::statements(45, bucket_soft_delete::SCHEMA, None),
    Step::statements(46, bucket_soft_delete::GENERATION_SCHEMA, None),
    Step::statements(47, objects::retention::SUB_MICRO_SCHEMA, None),
    Step::statements(48, object_hold::SCHEMA, None),
    Step::statements(49, BUCKET_VERSIONING_PRESENCE_SCHEMA, None),
    Step::statements(50, sync_state::deletions::history::SCHEMA, None),
    Step::statements(51, policies::RAISED_BINDINGS_SCHEMA, None),
    Step::statements(52, admin::initialization::SCHEMA, None),
    Step::statements(53, STORAGE_CLASS_UPDATED_SCHEMA, None),
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
    for step in STEPS {
        if version < step.version {
            break;
        }
        if let Some(schema) = step.schema {
            expected.execute_batch(schema)?;
        }
        if let Some(check) = step.check {
            check(connection)?;
        }
        // Version 18 replaced the import journal's validation with the failure journal's.
        if step.version == 16 && version < 18 {
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
    Function(Migrate),
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

/// Upgrades a catalog at `version`: each step above it applies its migration, in order.
pub(super) fn upgrade(connection: &mut Connection, version: i64) -> Result<(), CatalogError> {
    for step in STEPS {
        if version < step.version
            && let Some(migration) = &step.migration
        {
            migration.apply(connection)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every version from the first migration to the current one has exactly one step, and only
    /// the two documented exceptions lack statements or a migration of their own.
    #[test]
    fn steps_list_every_version_once_with_two_exceptions() {
        let versions: Vec<i64> = STEPS.iter().map(|step| step.version).collect();
        assert_eq!(versions, (2..=CATALOG_SCHEMA_VERSION).collect::<Vec<_>>());
        let data_only: Vec<i64> = STEPS
            .iter()
            .filter(|step| step.schema.is_none())
            .map(|step| step.version)
            .collect();
        assert_eq!(data_only, [TIMESTAMP_MICROS_SCHEMA_VERSION]);
        let applied_by_previous: Vec<i64> = STEPS
            .iter()
            .filter(|step| step.migration.is_none())
            .map(|step| step.version)
            .collect();
        assert_eq!(applied_by_previous, [3]);
    }
}
