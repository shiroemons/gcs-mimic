use super::*;
use std::num::NonZeroU64;

const INSERT_BUCKET: &str = concat!(
    "INSERT INTO buckets (",
    bucket_columns!(),
    ", acl_policy_bound) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, NULL, 0, 0, ?19, ?20, 1)
         ON CONFLICT(name) DO NOTHING"
);
const SELECT_BUCKET: &str = concat!(
    "SELECT ",
    bucket_columns!(),
    " FROM buckets WHERE name = ?1"
);

impl Catalog {
    /// Sets the soft delete retention that a bucket created without a policy receives. The
    /// server applies its configuration before any bucket is created; an embedded catalog keeps
    /// `None`, which leaves such buckets without a policy.
    pub fn set_default_soft_delete_retention(&self, retention: Option<NonZeroU64>) {
        self.0.default_soft_delete_retention.store(
            retention.map_or(0, NonZeroU64::get),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    pub(crate) fn default_soft_delete_retention(&self) -> Option<NonZeroU64> {
        NonZeroU64::new(
            self.0
                .default_soft_delete_retention
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    pub(crate) async fn insert_bucket(
        &self,
        mut record: BucketRecord,
    ) -> Result<BucketRecord, CatalogError> {
        let default_retention = self.default_soft_delete_retention();
        self.run(move |connection| {
            validate_cors(&record.cors)?;
            let transaction = sql::immediate(connection)?;
            insert_bucket_in_transaction(&transaction, &mut record, default_retention)?;
            change_journal::append(
                &transaction,
                admin::Origin::Api,
                change_journal::ChangeKind::BucketCreate,
                &record.name,
                None,
            )?;
            transaction.commit()?;
            Ok(record)
        })
        .await
    }

    /// Reads the bucket and checks `access` against the owner observed by the same read.
    pub(crate) async fn get_bucket(
        &self,
        name: &str,
        access: &BucketAccess,
    ) -> Result<Option<BucketRecord>, CatalogError> {
        let name = name.to_owned();
        let access = access.clone();
        self.run(move |connection| {
            let transaction = connection.transaction()?;
            let record = select_bucket(&transaction, &name)?;
            if let Some(record) = &record {
                check_access(&transaction, &name, &record.project, &access)?;
            }
            Ok(record)
        })
        .await
    }

    /// Whether the bucket stores any CORS rule, without decoding the bucket row. A missing bucket
    /// has none. Every XML object request asks this to decide `Vary`, so it must stay cheap.
    pub(crate) async fn bucket_has_cors(&self, name: &str) -> Result<bool, CatalogError> {
        let name = name.to_owned();
        self.run(move |connection| {
            let stored = sql::row(
                connection,
                "SELECT cors FROM buckets WHERE name = ?1",
                [name],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
            Ok(stored.is_some_and(|cors| cors != "[]"))
        })
        .await
    }

    /// Deletes the bucket only if `access` holds for the owner observed under the write lock.
    pub(crate) async fn delete_bucket(
        &self,
        name: &str,
        conditions: BucketConditions,
        access: &BucketAccess,
    ) -> Result<(), CatalogError> {
        let name = name.to_owned();
        let access = access.clone();
        let catalog = self.clone();
        let clock = self.0.clock.clone();
        self.run(move |connection| {
            let transaction = sql::immediate(connection)?;
            let (metageneration, project): (i64, String) = sql::row(
                &transaction,
                "SELECT metageneration, project FROM buckets WHERE name = ?1",
                [&name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .ok_or(CatalogError::NotFound)?;
            check_access(&transaction, &name, &project, &access)?;
            if !conditions.hold(metageneration) {
                return Err(CatalogError::PreconditionFailed);
            }
            const NONEMPTY: &str = "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND soft_delete_time IS NULL)";
            let nonempty: bool = sql::scalar(&transaction, NONEMPTY, [&name])?;
            if nonempty {
                return Err(CatalogError::NotEmpty);
            }
            // With a soft delete policy the version and its retained objects move aside first,
            // so the permanent deletions below no longer find them.
            bucket_soft_delete::retire(&transaction, &name, unix_micros(clock.now())?)?;
            sql::exec(&transaction,
                "INSERT OR IGNORE INTO pending_blob_deletions SELECT blob_id FROM objects WHERE bucket = ?1",
                [&name],
            )?;
            sql::exec(&transaction, "DELETE FROM objects WHERE bucket = ?1", [&name])?;
            change_journal::append(
                &transaction,
                change_journal::origin(&access),
                change_journal::ChangeKind::BucketDelete,
                &name,
                None,
            )?;
            sql::exec(&transaction, "DELETE FROM bucket_policies WHERE bucket = ?1", [&name])?;
            sql::exec(&transaction, "DELETE FROM bucket_incarnations WHERE bucket = ?1", [&name])?;
            sql::exec(&transaction, "DELETE FROM sync_objects WHERE bucket = ?1", [&name])?;
            sql::exec(&transaction, "DELETE FROM buckets WHERE name = ?1", [&name])?;
            let result = transaction.commit().map_err(unavailable);
            // Commit failures may be ambiguous; cleanup rechecks the current bucket incarnation.
            catalog.schedule_multipart_orphan_scan();
            result
        })
        .await?;
        self.reclaim_after_commit().await;
        Ok(())
    }
}

/// Largest creation-time seed for a bucket generation (2255-03-14T16:00:00Z, in nanoseconds). A
/// time past 2262 would saturate at `i64::MAX`, leaving no room for `last + 1` and failing every later
/// bucket with "generation exhausted"; the cap keeps about 2e17 allocations of headroom. The
/// price is that a bucket created after the cap gets `previous + 1` instead of a value tracking
/// its creation time (documented in docs/en/slices/bucket-soft-delete-restore.md).
const MAX_GENERATION_SEED: i64 = 9_000_000_000_000_000_000;

pub(super) fn insert_bucket_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    record: &mut BucketRecord,
    default_soft_delete_retention: Option<NonZeroU64>,
) -> Result<(), CatalogError> {
    // A server default gives a new bucket that names no policy that retention, with effectiveTime
    // at the creation time, as Cloud Storage does with its seven days. An explicit policy, zero
    // included, is kept as sent. Without a default the bucket stays without a policy.
    if record.soft_delete_policy.is_none()
        && let Some(retention) = default_soft_delete_retention
    {
        record.soft_delete_policy = Some(crate::bucket::SoftDeletePolicy {
            retention_duration_seconds: retention.get(),
            effective_time: record.time_created,
        });
    }
    crate::bucket::lifecycle::validate(&record.lifecycle)?;
    crate::bucket::lifecycle::validate_location(&record.lifecycle, &record.location_type)?;
    let created = unix_micros(record.time_created)?;
    // Times outside the nanosecond i64 range (before 1677) fall back to the microsecond value.
    let created_nanos = unix_timestamp(record.time_created, 1)
        .unwrap_or_else(|_| created.saturating_mul(1000))
        .min(MAX_GENERATION_SEED);
    let updated = unix_micros(record.updated)?;
    record.time_created = system_time_micros(created)?;
    record.updated = system_time_micros(updated)?;
    record.uniform_bucket_level_access_locked_time = record
        .uniform_bucket_level_access_locked_time
        .map(|time| {
            unix_micros(time).and_then(|time| system_time_micros(time).map_err(unavailable))
        })
        .transpose()?;
    let labels = serde_json::to_string(&record.labels).map_err(unavailable)?;
    // Cloud Storage bucket generations are 19-digit nanosecond values. A dedicated allocator
    // keeps them unique and increasing even under a frozen injected clock, and across a
    // recreated name's soft-deleted versions, without disturbing object generations.
    let last: i64 = sql::scalar(
        transaction,
        "SELECT last_generation FROM bucket_generation WHERE singleton = 1",
        [],
    )?;
    record.generation = created_nanos.max(
        last.checked_add(1)
            .ok_or_else(|| unavailable("generation exhausted"))?,
    );
    let changed = transaction.prepare_cached(INSERT_BUCKET)?.execute(params![
        record.name,
        record.project,
        record.location,
        record.location_type,
        record.storage_class,
        labels,
        record.metageneration,
        created,
        updated,
        record.versioning_enabled,
        record.default_event_based_hold,
        cors_value(&record.cors).to_string(),
        record.public_access_prevention_enforced,
        record
            .uniform_bucket_level_access_locked_time
            .map(unix_micros)
            .transpose()?,
        serde_json::to_string(&record.lifecycle).map_err(unavailable)?,
        record
            .retention_policy
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(unavailable)?,
        record
            .soft_delete_policy
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(unavailable)?,
        record.object_retention_enabled,
        record.generation,
        record.versioning_configured,
    ])?;
    if changed == 0 {
        return Err(CatalogError::AlreadyExists);
    }
    sql::exec(
        transaction,
        "UPDATE bucket_generation SET last_generation = ?1 WHERE singleton = 1",
        [record.generation],
    )?;
    bucket_autoclass::insert(transaction, record)?;
    insert_policy(
        transaction,
        &record.name,
        &Policy::default_for(&record.project),
    )?;
    sql::exec(
        transaction,
        "INSERT INTO bucket_incarnations VALUES (?1, lower(hex(randomblob(16))))",
        [&record.name],
    )?;
    Ok(())
}

/// Reads one bucket through the cached statement that request paths share.
pub(in crate::storage) fn select_bucket(
    connection: &Connection,
    name: &str,
) -> rusqlite::Result<Option<BucketRecord>> {
    connection
        .prepare_cached(SELECT_BUCKET)?
        .query_row([name], read_bucket)
        .optional()
}

pub(super) fn read_bucket(row: &Row<'_>) -> rusqlite::Result<BucketRecord> {
    let labels: String = row.get(5)?;
    Ok(BucketRecord {
        soft_delete_policy: row
            .get::<_, Option<String>>(16)?
            .map(|v| serde_json::from_str(&v).map_err(|e| sql::conversion_error(16, Type::Text, e)))
            .transpose()?,
        retention_policy: row
            .get::<_, Option<String>>(15)?
            .map(|v| serde_json::from_str(&v).map_err(|e| sql::conversion_error(15, Type::Text, e)))
            .transpose()?,
        name: row.get(0)?,
        project: row.get(1)?,
        location: row.get(2)?,
        location_type: row.get(3)?,
        storage_class: row.get(4)?,
        autoclass: bucket_autoclass::read(row)?,
        labels: serde_json::from_str(&labels)
            .map_err(|e| sql::conversion_error(5, Type::Text, e))?,
        lifecycle: serde_json::from_str(&row.get::<_, String>(14)?)
            .map_err(|e| sql::conversion_error(14, Type::Text, e))?,
        metageneration: row.get(6)?,
        generation: row.get("generation")?,
        time_created: bucket_timestamps::read(row, row.get(7)?, "time_created_nanos")?,
        updated: bucket_timestamps::read(row, row.get(8)?, "updated_nanos")?,
        object_retention_enabled: row.get(17)?,
        versioning_enabled: row.get(9)?,
        versioning_configured: row.get("versioning_configured")?,
        default_event_based_hold: row.get(10)?,
        public_access_prevention_enforced: row.get(12)?,
        uniform_bucket_level_access_locked_time: row
            .get::<_, Option<i64>>(13)?
            .map(system_time_micros)
            .transpose()?,
        cors: cors_from_value(
            &serde_json::from_str(&row.get::<_, String>(11)?)
                .map_err(|e| sql::conversion_error(11, Type::Text, e))?,
        )
        .map_err(|e| sql::conversion_error(11, Type::Text, e))?,
    })
}
