//! Bounded lifecycle Delete execution on the catalog writer and injected business clock.

use super::{Catalog, CatalogError, Inner, objects, sql, unavailable};
use crate::{
    bucket::{
        BucketAccess,
        lifecycle::{
            LifecycleRule,
            evaluate::{PlannedAction, VersionFacts, evaluate},
        },
    },
    object::{ObjectError, ObjectRecord, ObjectTarget, custom_time_key},
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

#[cfg(test)]
mod tests;

const BUCKET_PAGE: i64 = 16;
const VERSION_PAGE: usize = 128;
const ACTION_LIMIT: u64 = 1024;
const COALESCE: Duration = Duration::from_secs(1);
const HAS_RULES: &str = "SELECT EXISTS(SELECT 1 FROM buckets WHERE lifecycle != '[]')";
const VERSION_SCAN: &str = "SELECT name, generation FROM objects WHERE bucket = ?1 AND soft_delete_time IS NULL AND (name, generation) < (?2, ?3) ORDER BY name DESC, generation DESC LIMIT 128";
const VERSION_FIRST: &str = "SELECT name, generation FROM objects WHERE bucket = ?1 AND soft_delete_time IS NULL ORDER BY name DESC, generation DESC LIMIT 128";

// Only the catalog writer touches this marker; even rolled-back writes may harmlessly rearm it.
thread_local! { static RULES_WRITTEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

pub(super) fn rules_written() {
    RULES_WRITTEN.set(true);
}

pub(super) fn take_rules_written() -> bool {
    RULES_WRITTEN.replace(false)
}

pub(super) struct LifecycleSchedule {
    armed: AtomicBool,
    dirty: AtomicBool,
    enabled: AtomicBool,
    state: Mutex<ScheduleState>,
    pass: tokio::sync::Semaphore,
}

#[derive(Default)]
struct ScheduleState {
    next: Option<SystemTime>,
    last_scan: Option<SystemTime>,
    #[cfg(test)]
    scans: u64,
}

impl LifecycleSchedule {
    pub(super) fn open(connection: &Connection) -> Result<Self, CatalogError> {
        let armed = sql::scalar(connection, HAS_RULES, [])?;
        Ok(Self {
            armed: AtomicBool::new(armed),
            dirty: AtomicBool::new(armed),
            enabled: AtomicBool::new(true),
            state: Mutex::new(ScheduleState::default()),
            pass: tokio::sync::Semaphore::new(1),
        })
    }

    /// Every ordinary write pays just this load when no bucket has rules.
    pub(super) fn on_write(&self, wake: &tokio::sync::Notify) {
        if self.armed.load(Ordering::Acquire) && !self.dirty.swap(true, Ordering::AcqRel) {
            wake.notify_one();
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, ScheduleState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Inner {
    /// Called on the writer after a transaction stores rules, even if a prior scan disarmed us.
    pub(super) fn lifecycle_rules_changed(&self) {
        self.lifecycle.armed.store(true, Ordering::Release);
        self.lifecycle.on_write(&self.maintenance_wake);
    }
}

#[derive(Clone, Copy, Default, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LifecycleOutcome {
    pub(crate) scanned: u64,
    pub(crate) deleted: u64,
    pub(crate) archived: u64,
    pub(crate) blocked: u64,
    #[serde(skip)]
    pub(crate) next: Option<SystemTime>,
}

impl LifecycleOutcome {
    fn record(&mut self, other: Self) {
        self.scanned += other.scanned;
        self.deleted += other.deleted;
        self.archived += other.archived;
        self.blocked += other.blocked;
        self.next = self.next.into_iter().chain(other.next).min();
    }
    fn applied(&self) -> u64 {
        self.deleted + self.archived
    }
}

#[derive(Clone, Copy)]
pub(super) enum Pass {
    Scheduled,
    Forced,
}

impl Catalog {
    /// Applied before maintenance starts; stored rules still round trip while execution is off.
    pub fn configure_lifecycle(&self, config: &crate::config::Config) {
        self.0
            .lifecycle
            .enabled
            .store(!config.disable_lifecycle, Ordering::Release);
    }

    pub(crate) async fn run_lifecycle_now(&self) -> Result<LifecycleOutcome, CatalogError> {
        let outcome = self.run_lifecycle(Pass::Forced).await;
        self.0.maintenance_wake.notify_one();
        outcome
    }

    pub(super) async fn run_lifecycle(&self, pass: Pass) -> Result<LifecycleOutcome, CatalogError> {
        let schedule = &self.0.lifecycle;
        let _pass = schedule.pass.acquire().await.map_err(unavailable)?;
        if !schedule.enabled.load(Ordering::Acquire) {
            return Ok(LifecycleOutcome::default());
        }
        let now = self.0.clock.now();
        if matches!(pass, Pass::Scheduled) {
            let state = schedule.state();
            if !schedule.dirty.load(Ordering::Acquire) && state.next.is_none_or(|due| now < due) {
                return Ok(LifecycleOutcome {
                    next: state.next,
                    ..Default::default()
                });
            }
            if schedule.dirty.load(Ordering::Acquire)
                && state.next.is_none_or(|due| now < due)
                && let Some(gate) = state.last_scan.and_then(|last| last.checked_add(COALESCE))
                && now < gate
            {
                return Ok(LifecycleOutcome {
                    next: Some(gate),
                    ..Default::default()
                });
            }
        }
        schedule.dirty.store(false, Ordering::Release);
        {
            let mut state = schedule.state();
            state.last_scan = Some(now);
            #[cfg(test)]
            {
                state.scans += 1;
            }
        }
        let mut scan = ScanGuard {
            inner: &self.0,
            complete: false,
        };
        let result = self.sweep_lifecycle(now).await;
        let inner = self.0.clone();
        let capped = result
            .as_ref()
            .is_ok_and(|outcome| outcome.applied() == ACTION_LIMIT);
        let next = result.as_ref().ok().and_then(|outcome| outcome.next);
        let failed = result.is_err();
        let next = self
            .run::<_, CatalogError>(move |connection| {
                let armed = sql::scalar(connection, HAS_RULES, [])?;
                inner.lifecycle.armed.store(armed, Ordering::Release);
                if !armed {
                    inner.lifecycle.dirty.store(false, Ordering::Release);
                }
                if failed || capped {
                    inner.lifecycle.dirty.store(true, Ordering::Release);
                }
                let next = if capped {
                    Some(now)
                } else if inner.lifecycle.dirty.load(Ordering::Acquire) {
                    next.into_iter().chain(now.checked_add(COALESCE)).min()
                } else {
                    next
                };
                let mut state = inner.lifecycle.state();
                state.next = next;
                if failed {
                    state.last_scan = None;
                }
                Ok(next)
            })
            .await?;
        scan.complete = true;
        result.map(|mut outcome| {
            outcome.next = next;
            outcome
        })
    }

    async fn sweep_lifecycle(&self, now: SystemTime) -> Result<LifecycleOutcome, CatalogError> {
        let mut outcome = LifecycleOutcome::default();
        let mut after = String::new();
        let mut failure = None;
        loop {
            let cursor = after.clone();
            let buckets = self.run::<_, CatalogError>(move |connection| {
                Ok(connection.prepare_cached("SELECT name, incarnation FROM buckets JOIN bucket_incarnations ON bucket = name WHERE lifecycle != '[]' AND name > ?1 ORDER BY name LIMIT ?2")?
                    .query_map(params![cursor, BUCKET_PAGE], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            }).await?;
            if buckets.is_empty() {
                break;
            }
            for (bucket, incarnation) in buckets {
                after.clone_from(&bucket);
                let (page, error) = self
                    .sweep_lifecycle_bucket(
                        bucket,
                        incarnation,
                        now,
                        ACTION_LIMIT - outcome.applied(),
                    )
                    .await;
                outcome.record(page);
                if let Some(error) = error {
                    crate::diagnostics::emit(
                        crate::diagnostics::LogLevel::Warn,
                        "lifecycle_bucket_failed",
                        serde_json::json!({"bucket":after,"message":error.to_string()}),
                    );
                    if failure.is_none() {
                        failure = Some(error);
                    }
                }
                if outcome.applied() == ACTION_LIMIT {
                    return failure.map_or(Ok(outcome), Err);
                }
            }
        }
        failure.map_or(Ok(outcome), Err)
    }

    async fn sweep_lifecycle_bucket(
        &self,
        bucket: String,
        incarnation: String,
        now: SystemTime,
        limit: u64,
    ) -> (LifecycleOutcome, Option<CatalogError>) {
        let mut outcome = LifecycleOutcome::default();
        let mut cursor = None;
        loop {
            let (bucket, incarnation) = (bucket.clone(), incarnation.clone());
            let remaining = limit - outcome.applied();
            let page = self
                .run::<_, CatalogError>(move |connection| {
                    sweep_page(connection, &bucket, &incarnation, cursor, now, remaining)
                        .map_err(unavailable)
                })
                .await;
            let page = match page {
                Ok(page) => page,
                Err(error) => return (outcome, Some(error)),
            };
            outcome.record(page.outcome);
            cursor = page.after;
            if let Some(error) = page.failure {
                return (outcome, Some(unavailable(error)));
            }
            if cursor.is_none() || outcome.applied() == limit {
                return (outcome, None);
            }
        }
    }
}

struct ScanGuard<'a> {
    inner: &'a Inner,
    complete: bool,
}

impl Drop for ScanGuard<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.inner.lifecycle.dirty.store(true, Ordering::Release);
            self.inner.maintenance_wake.notify_one();
        }
    }
}

type Cursor = (String, i64);

struct Page {
    outcome: LifecycleOutcome,
    after: Option<Cursor>,
    failure: Option<ObjectError>,
}

fn sweep_page(
    connection: &mut Connection,
    bucket: &str,
    incarnation: &str,
    cursor: Option<Cursor>,
    now: SystemTime,
    limit: u64,
) -> Result<Page, ObjectError> {
    let rows = if let Some((name, generation)) = cursor {
        connection
            .prepare_cached(VERSION_SCAN)?
            .query_map(params![bucket, name, generation], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        connection
            .prepare_cached(VERSION_FIRST)?
            .query_map([bucket], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let after = (rows.len() == VERSION_PAGE)
        .then(|| rows.last().cloned())
        .flatten();
    let mut outcome = LifecycleOutcome::default();
    for (name, generation) in rows {
        match apply_version(
            connection,
            &ObjectTarget::new(bucket, &name, Some(generation)),
            incarnation,
            now,
        ) {
            Ok(version) => outcome.record(version),
            Err(error) => {
                return Ok(Page {
                    outcome,
                    after,
                    failure: Some(error),
                });
            }
        }
        if outcome.applied() == limit {
            break;
        }
    }
    Ok(Page {
        outcome,
        after,
        failure: None,
    })
}

fn apply_version(
    connection: &mut Connection,
    target: &ObjectTarget,
    incarnation: &str,
    now: SystemTime,
) -> Result<LifecycleOutcome, ObjectError> {
    let transaction = sql::immediate(connection)?;
    let stored: Option<(String, String)> = sql::row(&transaction,
        "SELECT lifecycle, incarnation FROM buckets JOIN bucket_incarnations ON bucket = name WHERE name = ?1",
        [&target.bucket], |row| Ok((row.get(0)?, row.get(1)?))).optional()?;
    let Some((stored, current)) = stored else {
        return Ok(LifecycleOutcome::default());
    };
    if current != incarnation {
        return Ok(LifecycleOutcome::default());
    }
    let values = serde_json::from_str::<Vec<serde_json::Value>>(&stored)
        .map_err(|error| ObjectError::Corrupt(super::BackendCause::from_error(error)))?;
    let rules = LifecycleRule::from_stored(&values)?;
    let object = match objects::selected(
        &transaction,
        &target.bucket,
        &target.name,
        target.generation,
        Default::default(),
        objects::RequestKind::Read,
        &BucketAccess::Permissive,
    ) {
        Ok(object) => object,
        Err(ObjectError::ObjectNotFound) => return Ok(LifecycleOutcome::default()),
        Err(error) => return Err(error),
    };
    let newer_versions = sql::scalar(
        &transaction,
        "SELECT count(*) FROM objects WHERE bucket = ?1 AND name = ?2 AND generation > ?3 AND soft_delete_time IS NULL",
        params![target.bucket, target.name, target.generation],
    )?;
    let facts = version_facts(&object.record, newer_versions)?;
    let evaluation = evaluate(&rules, &facts, now);
    if object.record.has_hold() {
        return Ok(LifecycleOutcome {
            scanned: 1,
            blocked: u64::from(matches!(evaluation.action, Some(PlannedAction::Delete))),
            ..Default::default()
        });
    }
    if let Some(until) = object.record.earliest_deletion_time()
        && until > crate::object::unix_nanos::UnixNanos::from(now)
    {
        let next = nanos_time(until).and_then(|until| {
            let future = evaluate(&rules, &facts, until);
            if matches!(future.action, Some(PlannedAction::Delete)) {
                Some(until)
            } else {
                future.next_due
            }
        });
        return Ok(LifecycleOutcome {
            scanned: 1,
            blocked: u64::from(matches!(evaluation.action, Some(PlannedAction::Delete))),
            next,
            ..Default::default()
        });
    }
    let mut outcome = LifecycleOutcome {
        scanned: 1,
        next: evaluation.next_due,
        ..Default::default()
    };
    match evaluation.action {
        Some(PlannedAction::Delete) => {
            outcome.record(retire_version(transaction, &object.record, now)?);
            outcome.next = None;
        }
        Some(PlannedAction::SetStorageClass(_)) | None => {}
    }
    Ok(outcome)
}

fn retire_version(
    transaction: rusqlite::Transaction<'_>,
    record: &ObjectRecord,
    now: SystemTime,
) -> Result<LifecycleOutcome, ObjectError> {
    record.check_retention(now)?;
    let target = ObjectTarget::new(
        &record.bucket,
        &record.name,
        record.time_deleted.map(|_| record.generation),
    );
    // The maintenance pass records soft-delete deadlines in its following expiry sweep.
    let _retired = objects::delete_in_transaction(
        &transaction,
        &target,
        objects::DeleteContext {
            now,
            conditions: Default::default(),
            access: &BucketAccess::Permissive,
            sync_binding: None,
        },
    )?;
    let archived = record.time_deleted.is_none()
        && sql::scalar::<bool>(
            &transaction,
            "SELECT versioning_enabled FROM buckets WHERE name = ?1",
            [&record.bucket],
        )?;
    #[cfg(test)]
    let data_dir = super::fault::database_dir(&transaction);
    #[cfg(test)]
    super::fault::hit(&data_dir, super::fault::Step::LifecycleCommit)
        .map_err(objects::object_unavailable)?;
    transaction.commit()?;
    #[cfg(test)]
    super::fault::hit(&data_dir, super::fault::Step::LifecycleCommitted)
        .map_err(objects::object_unavailable)?;
    Ok(LifecycleOutcome {
        deleted: u64::from(!archived),
        archived: u64::from(archived),
        ..Default::default()
    })
}

fn version_facts(
    record: &ObjectRecord,
    newer_versions: u64,
) -> Result<VersionFacts<'_>, ObjectError> {
    let custom_time = record
        .custom_time
        .as_deref()
        .map(custom_time_key)
        .transpose()?
        .and_then(|(seconds, nanos)| {
            crate::clock::system_time_from_unix_seconds(seconds)?
                .checked_add(Duration::from_nanos(u64::from(nanos)))
        });
    Ok(VersionFacts {
        name: &record.name,
        size: record.size,
        storage_class: record.storage_class,
        time_created: record.time_created,
        custom_time,
        noncurrent_time: record.time_deleted,
        newer_versions,
    })
}

fn nanos_time(value: crate::object::unix_nanos::UnixNanos) -> Option<SystemTime> {
    // Round up to the platform clock's precision, so a nanosecond retention never spins early.
    let time = crate::clock::system_time_from_unix_seconds(value.secs())?
        .checked_add(Duration::from_nanos(u64::from(value.subsec_nanos())))?;
    if crate::object::unix_nanos::UnixNanos::from(time) < value {
        time.checked_add(Duration::from_nanos(100))
    } else {
        Some(time)
    }
}
