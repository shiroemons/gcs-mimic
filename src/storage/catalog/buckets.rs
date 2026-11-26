use super::*;
use crate::bucket::{
    ClassLocationConflict, Location, LocationKind, Versioning,
    lifecycle::{BucketLifecycle, LifecycleRules},
};
use std::num::NonZeroU64;

const INSERT_BUCKET: &str = concat!(
    "INSERT INTO buckets (",
    bucket_columns!(),
    ", acl_policy_bound) VALUES (",
    bucket_parameters!(),
    ", 1) ON CONFLICT(name) DO NOTHING"
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
        self.0.policy.default_soft_delete_retention.store(
            retention.map_or(0, NonZeroU64::get),
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    pub(crate) fn default_soft_delete_retention(&self) -> Option<NonZeroU64> {
        NonZeroU64::new(
            self.0
                .policy
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
            let creation = BucketCreation::default_policy(default_retention);
            create_bucket_in_transaction(&transaction, &mut record, creation)?;
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
        self.run_read(move |connection| {
            let snapshot = sql::read_snapshot(connection)?;
            let record = select_bucket(&snapshot, &name)?;
            if let Some(record) = &record {
                check_access(&snapshot, &name, &record.project, &access)?;
            }
            Ok(record)
        })
        .await
    }

    /// Whether the bucket stores any CORS rule, without decoding the bucket row. A missing bucket
    /// has none. Every XML object request asks this to decide `Vary`, so it must stay cheap.
    pub(crate) async fn bucket_has_cors(&self, name: &str) -> Result<bool, CatalogError> {
        if let Some(has_cors) = self.cached_bucket_cors(name).await {
            return Ok(has_cors);
        }
        let revision = self.0.reads.cache.revision();
        let inner = self.0.clone();
        let name = name.to_owned();
        self.run_read_on(move |connection, path| {
            let stored = sql::row(
                connection,
                "SELECT cors FROM buckets WHERE name = ?1",
                [&name],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
            let has_cors = stored.is_some_and(|cors| cors != "[]");
            if path.stores_answers() {
                inner
                    .reads
                    .cache
                    .store_bucket_cors(revision, &name, has_cors);
            }
            Ok(has_cors)
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
        let clock = self.0.policy.clock.clone();
        let journaled = self.run(move |connection| {
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
            conditions.check(metageneration)?;
            const NONEMPTY: &str = "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND soft_delete_time IS NULL)";
            let nonempty: bool = sql::scalar(&transaction, NONEMPTY, [&name])?;
            if nonempty {
                return Err(CatalogError::NotEmpty);
            }
            // With a soft delete policy the version and its retained objects move aside first,
            // so the permanent deletions below no longer find them.
            bucket_soft_delete::retire(&transaction, &name, unix_micros(clock.now())?)?;
            let removed = remove_live_bucket(&transaction, &name)?;
            let result = transaction.commit().map_err(unavailable);
            // Commit failures may be ambiguous; cleanup rechecks the current bucket incarnation.
            catalog.schedule_multipart_orphan_scan();
            result?;
            Ok::<_, CatalogError>(objects::Journaled::from(removed != 0))
        })
        .await?;
        self.reclaim_if(journaled).await;
        Ok(())
    }
}

/// Object rows one step of [`remove_objects`] deletes before journaling their blobs.
const REMOVAL_PAGE: i64 = 128;

/// Deletes every object row stored under `key`, a bucket name or a soft-deleted version's
/// retained key, journals each blob for reclamation and returns how many rows it deleted. Each
/// page's rows go before their journal rows, so the journal takes the pages the rows free: a RAM
/// catalog at its page budget can still be emptied, which journaling first would refuse.
pub(super) fn remove_objects(connection: &Connection, key: &str) -> Result<u64, CatalogError> {
    let mut removed = 0;
    loop {
        let page = connection
            .prepare_cached("SELECT blob_id FROM objects WHERE bucket = ?1 LIMIT ?2")?
            .query_map(rusqlite::params![key, REMOVAL_PAGE], |row| {
                row.get::<_, crate::storage::BlobId>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if page.is_empty() {
            return Ok(removed);
        }
        for id in &page {
            sql::exec(connection, "DELETE FROM objects WHERE blob_id = ?1", [id])?;
        }
        for id in &page {
            objects::journal_deletion(connection, id)?;
        }
        removed += u64::try_from(page.len()).map_err(unavailable)?;
    }
}

/// Removes a live bucket with the objects its name still holds, inside a deletion or purge
/// transaction the caller commits: drops its policy, incarnation, seed records and row, so no
/// seed record claims an object the removal deleted. ACL, notification and initialization rows
/// cascade. Returns how many objects it removed.
pub(super) fn remove_live_bucket(
    transaction: &Connection,
    name: &str,
) -> Result<u64, CatalogError> {
    let removed = remove_objects(transaction, name)?;
    for query in [
        "DELETE FROM seed_objects WHERE bucket = ?1",
        "DELETE FROM bucket_policies WHERE bucket = ?1",
        "DELETE FROM bucket_incarnations WHERE bucket = ?1",
        "DELETE FROM buckets WHERE name = ?1",
    ] {
        sql::exec(transaction, query, [name])?;
    }
    Ok(removed)
}

/// Largest creation-time seed for a bucket generation (2255-03-14T16:00:00Z, in nanoseconds). A
/// time past 2262 would saturate at `i64::MAX`, leaving no room for `last + 1` and failing every later
/// bucket with "generation exhausted"; the cap keeps about 2e17 allocations of headroom. The
/// price is that a bucket created after the cap gets `previous + 1` instead of a value tracking
/// its creation time (documented in docs/en/slices/bucket-soft-delete-restore.md).
const MAX_GENERATION_SEED: i64 = 9_000_000_000_000_000_000;

/// How a creation stores a new bucket.
#[derive(Clone, Copy)]
pub(super) struct BucketCreation<'a> {
    pub(super) default_retention: Option<NonZeroU64>,
    /// Replaces the owning project's default policy, as a policy write would; `None` keeps the
    /// default.
    pub(super) policy: Option<&'a Policy>,
    pub(super) principals: Option<&'a principals::Profile>,
}

impl BucketCreation<'_> {
    pub(super) fn default_policy(default_retention: Option<NonZeroU64>) -> Self {
        Self {
            default_retention,
            policy: None,
            principals: None,
        }
    }
}

/// Inserts a missing bucket and derives its ACL from a policy other than the default one; the
/// caller commits.
pub(super) fn create_bucket_in_transaction(
    transaction: &sql::WriteTx<'_>,
    record: &mut BucketRecord,
    creation: BucketCreation<'_>,
) -> Result<(), CatalogError> {
    let BucketCreation {
        default_retention,
        policy,
        principals,
    } = creation;
    let Some(policy) = policy else {
        return insert_bucket_in_transaction(transaction, record, default_retention);
    };
    insert_bucket_with_policy(transaction, record, default_retention, policy)?;
    if policy.names_public_principal()
        && policies::policy_public_access(transaction, &record.name)?
            == crate::auth::PublicAccess::Prevented
    {
        return Err(CatalogError::PublicAccessPrevention);
    }
    // As a policy write does, so the bucket ACL shows the grants the policy makes.
    acl::replace_from_policy(transaction, &record.name, policy, principals)?;
    Ok(())
}

/// Inserts a bucket with the default policy of its owning project, as an API insert does.
pub(super) fn insert_bucket_in_transaction(
    transaction: &sql::WriteTx<'_>,
    record: &mut BucketRecord,
    default_soft_delete_retention: Option<NonZeroU64>,
) -> Result<(), CatalogError> {
    let policy = Policy::default_for(&record.project);
    insert_bucket_with_policy(transaction, record, default_soft_delete_retention, &policy)
}

/// Inserts a bucket that starts with `policy` instead of the default one.
fn insert_bucket_with_policy(
    transaction: &sql::WriteTx<'_>,
    record: &mut BucketRecord,
    default_soft_delete_retention: Option<NonZeroU64>,
    policy: &Policy,
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
    // A library caller can pass back unreadable stored rules; they fail as input, as any
    // request's rules would.
    let lifecycle = match &record.lifecycle {
        BucketLifecycle::Rules(rules) => rules.clone(),
        BucketLifecycle::Unreadable { values, .. } => LifecycleRules::from_values(values)?,
    };
    ClassLocationConflict::check(record.storage_class, record.location, &record.project)?;
    crate::bucket::lifecycle::validate_location(&lifecycle, record.location, &record.project)?;
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
        .map(|time| system_time_micros(unix_micros(time)?).map_err(unavailable))
        .transpose()?;
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
    let changed = insert_row(transaction, record, created, updated)?;
    if changed == 0 {
        return Err(CatalogError::AlreadyExists);
    }
    if !record.lifecycle.is_empty() {
        lifecycle::rules_written();
    }
    sql::exec(
        transaction,
        "UPDATE bucket_generation SET last_generation = ?1 WHERE singleton = 1",
        [record.generation],
    )?;
    bucket_autoclass::insert(transaction, record, &lifecycle)?;
    insert_policy(transaction, &record.name, policy)?;
    sql::exec(
        transaction,
        "INSERT INTO bucket_incarnations VALUES (?1, lower(hex(randomblob(16))))",
        [&record.name],
    )?;
    Ok(())
}

/// Inserts the row of a new bucket unless the name exists, returning how many rows it inserted.
/// Autoclass starts as NULL: `bucket_autoclass::insert` owns that column's encoding.
fn insert_row(
    transaction: &sql::WriteTx<'_>,
    record: &BucketRecord,
    created: i64,
    updated: i64,
) -> Result<usize, CatalogError> {
    let locked_time = record
        .uniform_bucket_level_access_locked_time
        .map(unix_micros)
        .transpose()?;
    let (versioning_enabled, versioning_configured) = record.versioning.columns();
    let changed = transaction
        .prepare_cached(INSERT_BUCKET)?
        .execute(rusqlite::named_params! {
            ":name": record.name,
            ":project": record.project,
            ":location": record.location,
            ":location_type": record.location.kind(),
            ":storage_class": record.storage_class,
            ":labels": sql::Json(&record.labels),
            ":metageneration": record.metageneration,
            ":time_created": created,
            ":updated": updated,
            ":versioning_enabled": versioning_enabled,
            ":default_event_based_hold": record.default_event_based_hold,
            ":cors": Cors(&record.cors),
            ":public_access_prevention_enforced": record.public_access_prevention,
            ":uniform_bucket_level_access_locked_time": locked_time,
            ":lifecycle": sql::Json(&record.lifecycle),
            ":retention_policy": record.retention_policy.as_ref().map(sql::Json),
            ":soft_delete_policy": record.soft_delete_policy.as_ref().map(sql::Json),
            ":object_retention_enabled": record.object_retention_enabled,
            ":autoclass": None::<sql::Json<crate::bucket::autoclass::Autoclass>>,
            ":time_created_nanos": 0,
            ":updated_nanos": 0,
            ":generation": record.generation,
            ":versioning_configured": versioning_configured,
        })?;
    Ok(changed)
}

/// The `cors` column holds the JSON resource form. `Catalog::bucket_has_cors` compares the
/// stored text with `[]`, the only spelling of no rules this writes.
pub(super) struct Cors<'a>(pub(super) &'a [crate::bucket::CorsRule]);

impl rusqlite::ToSql for Cors<'_> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::from(
            cors_value(self.0).to_string(),
        ))
    }
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

/// Reads every column by name, so a query may select the bucket columns in any order and add
/// its own.
pub(super) fn read_bucket(row: &Row<'_>) -> rusqlite::Result<BucketRecord> {
    Ok(BucketRecord {
        soft_delete_policy: row
            .get::<_, Option<sql::Json<_>>>("soft_delete_policy")?
            .map(|json| json.0),
        retention_policy: row
            .get::<_, Option<sql::Json<_>>>("retention_policy")?
            .map(|json| json.0),
        name: row.get("name")?,
        project: row.get("project")?,
        location: read_location(row)?,
        storage_class: row.get("storage_class")?,
        autoclass: row
            .get::<_, Option<sql::Json<_>>>("autoclass")?
            .map(|json| json.0),
        labels: row.get::<_, sql::Json<_>>("labels")?.0,
        lifecycle: BucketLifecycle::from_stored(row.get::<_, sql::Json<_>>("lifecycle")?.0),
        metageneration: row.get("metageneration")?,
        generation: row.get("generation")?,
        time_created: bucket_timestamps::read(row, "time_created", "time_created_nanos")?,
        updated: bucket_timestamps::read(row, "updated", "updated_nanos")?,
        object_retention_enabled: row.get("object_retention_enabled")?,
        versioning: Versioning::from_columns((
            row.get("versioning_enabled")?,
            row.get("versioning_configured")?,
        )),
        default_event_based_hold: row.get("default_event_based_hold")?,
        public_access_prevention: row.get("public_access_prevention_enforced")?,
        uniform_bucket_level_access_locked_time: row
            .get::<_, Option<i64>>("uniform_bucket_level_access_locked_time")?
            .map(system_time_micros)
            .transpose()?,
        cors: read_cors(row)?,
    })
}

/// The stored location. Every release wrote a canonical name from the official table next to
/// the type that table gives it, so a type that disagrees is corrupt; the column stays for older
/// binaries of the same schema.
fn read_location(row: &Row<'_>) -> rusqlite::Result<Location> {
    let location: Location = row.get("location")?;
    let index = row.as_ref().column_index("location_type")?;
    let kind: LocationKind = row.get(index)?;
    if kind != location.kind() {
        return Err(sql::conversion_error(
            index,
            Type::Text,
            "bucket location type disagrees with its location",
        ));
    }
    Ok(location)
}

/// The `cors` column holds the JSON resource form; rules it does not accept are corrupt.
fn read_cors(row: &Row<'_>) -> rusqlite::Result<Vec<crate::bucket::CorsRule>> {
    let index = row.as_ref().column_index("cors")?;
    let value = row.get::<_, sql::Json<serde_json::Value>>(index)?.0;
    cors_from_value(&value).map_err(|error| sql::conversion_error(index, Type::Text, error))
}
