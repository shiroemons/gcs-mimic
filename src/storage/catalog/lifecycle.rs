//! Bounded lifecycle execution on the catalog writer and injected business clock.

use super::{Catalog, CatalogError, Inner, objects, sql, unavailable};
use crate::{
    bucket::{
        BucketAccess, StorageClass,
        lifecycle::{
            LifecycleAction, LifecycleLocation, LifecycleRule,
            evaluate::{Evaluation, PlannedAction, VersionFacts, evaluate, wakeup_at},
        },
    },
    object::{ObjectError, ObjectRecord, ObjectTarget, custom_time_key, unix_nanos::UnixNanos},
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};

mod abort;
#[cfg(test)]
mod tests;

const BUCKET_PAGE: i64 = 16;
const VERSION_PAGE: usize = 128;
const ACTION_LIMIT: u64 = 1024;
const COALESCE: Duration = Duration::from_secs(1);
const HAS_RULES: &str = "SELECT EXISTS(SELECT 1 FROM buckets WHERE lifecycle != '[]')";
const VERSION_SCAN: &str = "SELECT name, generation FROM objects WHERE bucket = ?1 AND soft_delete_time IS NULL AND (name, generation) < (?2, ?3) ORDER BY name DESC, generation DESC LIMIT ?4";
const VERSION_FIRST: &str = "SELECT name, generation FROM objects WHERE bucket = ?1 AND soft_delete_time IS NULL ORDER BY name DESC, generation DESC LIMIT ?2";

// Only the catalog writer touches this marker; even rolled-back writes may harmlessly rearm it.
thread_local! { static RULES_WRITTEN: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) }; }

pub(super) fn rules_written() {
    debug_assert!(
        RULES_WRITTEN.get().is_some(),
        "lifecycle rules must be written inside with_connection"
    );
    RULES_WRITTEN.set(Some(true));
}

pub(super) struct RuleWriteScope;

impl RuleWriteScope {
    pub(super) fn enter() -> Self {
        debug_assert!(
            RULES_WRITTEN.get().is_none(),
            "catalog write scopes cannot nest"
        );
        RULES_WRITTEN.set(Some(false));
        Self
    }
    pub(super) fn written(&self) -> bool {
        RULES_WRITTEN.get() == Some(true)
    }
}

impl Drop for RuleWriteScope {
    fn drop(&mut self) {
        RULES_WRITTEN.set(None);
    }
}

pub(super) struct LifecycleSchedule {
    multipart_busy: AtomicBool,
    armed: AtomicBool,
    dirty: AtomicBool,
    enabled: AtomicBool,
    sync_disabled: AtomicBool,
    sync_notice: AtomicBool,
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
        sql::exec(
            connection,
            "CREATE TEMP TABLE lifecycle_quarantine (bucket TEXT NOT NULL, incarnation TEXT NOT NULL, name TEXT NOT NULL, generation INTEGER NOT NULL, revision TEXT NOT NULL, PRIMARY KEY (bucket, incarnation, name, generation)) WITHOUT ROWID",
            [],
        )?;
        let armed = sql::scalar(connection, HAS_RULES, [])?;
        Ok(Self {
            multipart_busy: AtomicBool::new(false),
            armed: AtomicBool::new(armed),
            dirty: AtomicBool::new(armed),
            enabled: AtomicBool::new(false),
            sync_disabled: AtomicBool::new(false),
            sync_notice: AtomicBool::new(false),
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

    fn notify_sync_disabled(&self) {
        if self.sync_disabled.load(Ordering::Acquire)
            && !self.sync_notice.swap(true, Ordering::AcqRel)
        {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Info,
                "lifecycle_sync_disabled",
                serde_json::json!({"message":"Stored lifecycle rules are not executed while directory synchronization is configured by default. Set lifecycle_execution=enabled to execute lifecycle actions; like API deletions, Delete also removes synchronized host files."}),
            );
        }
    }
}

impl Inner {
    /// Called on the writer after a transaction stores rules, even if a prior scan disarmed us.
    pub(super) fn lifecycle_rules_changed(&self) {
        self.lifecycle.armed.store(true, Ordering::Release);
        self.lifecycle.notify_sync_disabled();
        self.lifecycle.on_write(&self.maintenance_wake);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum LifecycleOutcome {
    Disabled,
    Executed(LifecycleCounts),
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct LifecycleCounts {
    pub(crate) aborted: u64,
    pub(crate) scanned: u64,
    pub(crate) deleted: u64,
    pub(crate) archived: u64,
    pub(crate) transitioned: u64,
    pub(crate) blocked: u64,
    pub(crate) skipped: u64,
    pub(crate) next: Option<SystemTime>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleResponse {
    aborted: u64,
    enabled: bool,
    scanned: u64,
    deleted: u64,
    archived: u64,
    transitioned: u64,
    blocked: u64,
    skipped: u64,
}

impl LifecycleOutcome {
    pub(super) fn next(&self) -> Option<SystemTime> {
        match self {
            Self::Disabled => None,
            Self::Executed(counts) => counts.next,
        }
    }
}

impl serde::Serialize for LifecycleOutcome {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let counts = match self {
            Self::Disabled => LifecycleCounts::default(),
            Self::Executed(counts) => *counts,
        };
        LifecycleResponse {
            aborted: counts.aborted,
            enabled: matches!(self, Self::Executed(_)),
            scanned: counts.scanned,
            deleted: counts.deleted,
            archived: counts.archived,
            transitioned: counts.transitioned,
            blocked: counts.blocked,
            skipped: counts.skipped,
        }
        .serialize(serializer)
    }
}

impl LifecycleCounts {
    fn record(&mut self, other: Self) {
        self.aborted += other.aborted;
        self.scanned += other.scanned;
        self.deleted += other.deleted;
        self.archived += other.archived;
        self.transitioned += other.transitioned;
        self.blocked += other.blocked;
        self.skipped += other.skipped;
        self.next = self.next.into_iter().chain(other.next).min();
    }
    fn applied(&self) -> u64 {
        self.deleted + self.archived + self.transitioned + self.aborted
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
        let schedule = &self.0.lifecycle;
        schedule.sync_disabled.store(
            config.sync_dir.is_some() && config.lifecycle_execution.is_none(),
            Ordering::Release,
        );
        if schedule.armed.load(Ordering::Acquire) {
            schedule.notify_sync_disabled();
        }
        schedule.enabled.store(
            config.effective_lifecycle_execution() == crate::config::LifecycleExecution::Enabled,
            Ordering::Release,
        );
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
            return Ok(LifecycleOutcome::Disabled);
        }
        let now = self.0.clock.now();
        if matches!(pass, Pass::Scheduled) {
            let state = schedule.state();
            if !schedule.dirty.load(Ordering::Acquire) && state.next.is_none_or(|due| now < due) {
                return Ok(LifecycleOutcome::Executed(LifecycleCounts {
                    next: state.next,
                    ..Default::default()
                }));
            }
            if schedule.dirty.load(Ordering::Acquire)
                && state.next.is_none_or(|due| now < due)
                && let Some(gate) = state.last_scan.and_then(|last| last.checked_add(COALESCE))
                && now < gate
            {
                return Ok(LifecycleOutcome::Executed(LifecycleCounts {
                    next: Some(gate),
                    ..Default::default()
                }));
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
            LifecycleOutcome::Executed(outcome)
        })
    }

    async fn sweep_lifecycle(&self, now: SystemTime) -> Result<LifecycleCounts, CatalogError> {
        self.run::<_, CatalogError>(|connection| {
            sql::exec(connection, "DELETE FROM lifecycle_quarantine WHERE NOT EXISTS (SELECT 1 FROM bucket_incarnations b WHERE b.bucket = lifecycle_quarantine.bucket AND b.incarnation = lifecycle_quarantine.incarnation) OR (generation != 0 AND NOT EXISTS (SELECT 1 FROM objects o WHERE o.bucket = lifecycle_quarantine.bucket AND o.name = lifecycle_quarantine.name AND o.generation = lifecycle_quarantine.generation))", [])?;
            Ok(())
        }).await?;
        let mut outcome = LifecycleCounts::default();
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
    ) -> (LifecycleCounts, Option<CatalogError>) {
        let (mut outcome, error) = self
            .sweep_lifecycle_abort(&bucket, &incarnation, now, limit)
            .await;
        if error.is_some() || outcome.applied() == limit {
            return (outcome, error);
        }
        let mut cursor = None;
        let mut rules = BucketRules::Absent;
        loop {
            let (bucket, incarnation) = (bucket.clone(), incarnation.clone());
            let remaining = limit - outcome.applied();
            let page = self
                .run::<_, CatalogError>(move |connection| {
                    sweep_page(
                        connection,
                        &bucket,
                        &incarnation,
                        cursor,
                        now,
                        remaining,
                        rules,
                    )
                    .map_err(unavailable)
                })
                .await;
            let page = match page {
                Ok(page) => page,
                Err(error) => return (outcome, Some(error)),
            };
            outcome.record(page.outcome);
            cursor = page.after;
            rules = page.rules;
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
    outcome: LifecycleCounts,
    after: Option<Cursor>,
    rules: BucketRules,
}

enum BucketRules {
    Absent,
    Quarantined,
    Ready {
        stored: String,
        location: LifecycleLocation,
        rules: Vec<LifecycleRule>,
    },
}

fn sweep_page(
    connection: &mut Connection,
    bucket: &str,
    incarnation: &str,
    cursor: Option<Cursor>,
    now: SystemTime,
    limit: u64,
    cached: BucketRules,
) -> Result<Page, ObjectError> {
    let transaction = sql::immediate(connection)?;
    let mut diagnostics = Vec::new();
    let rules = bucket_rules(&transaction, bucket, incarnation, cached, &mut diagnostics)?;
    let BucketRules::Ready { rules: parsed, .. } = &rules else {
        transaction.commit()?;
        emit_quarantines(diagnostics);
        return Ok(Page {
            outcome: LifecycleCounts::default(),
            after: None,
            rules,
        });
    };
    let rows = if let Some((name, generation)) = cursor {
        transaction
            .prepare_cached(VERSION_SCAN)?
            .query_map(params![bucket, name, generation, VERSION_PAGE], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    } else {
        transaction
            .prepare_cached(VERSION_FIRST)?
            .query_map(params![bucket, VERSION_PAGE], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let after = (rows.len() == VERSION_PAGE)
        .then(|| rows.last().cloned())
        .flatten();
    let mut outcome = LifecycleCounts::default();
    for (name, generation) in rows {
        outcome.record(evaluate_version(
            &transaction,
            &ObjectTarget::new(bucket, &name, Some(generation)),
            parsed,
            now,
            incarnation,
            &mut diagnostics,
            limit - outcome.applied(),
        )?);
        if outcome.applied() == limit {
            break;
        }
    }
    #[cfg(test)]
    let data_dir = super::fault::database_dir(&transaction);
    #[cfg(test)]
    if outcome.applied() > 0 {
        super::fault::hit(&data_dir, super::fault::Step::LifecycleCommit)
            .map_err(objects::object_unavailable)?;
    }
    transaction.commit()?;
    emit_quarantines(diagnostics);
    #[cfg(test)]
    if outcome.applied() > 0 {
        super::fault::hit(&data_dir, super::fault::Step::LifecycleCommitted)
            .map_err(objects::object_unavailable)?;
    }
    Ok(Page {
        outcome,
        after,
        rules,
    })
}

fn bucket_rules(
    transaction: &rusqlite::Transaction<'_>,
    bucket: &str,
    incarnation: &str,
    cached: BucketRules,
    diagnostics: &mut Vec<QuarantineEvent>,
) -> Result<BucketRules, ObjectError> {
    let stored: Option<(String, String, String)> = sql::row(transaction,
        "SELECT lifecycle, incarnation, location_type FROM buckets JOIN bucket_incarnations ON bucket = name WHERE name = ?1",
        [bucket], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).optional()?;
    let Some((stored, current, location)) = stored else {
        return Ok(BucketRules::Absent);
    };
    if current != incarnation {
        return Ok(BucketRules::Absent);
    }
    let location = LifecycleLocation::from(location.as_str());
    if let BucketRules::Ready {
        stored: previous,
        location: previous_location,
        ..
    } = &cached
        && *previous == stored
        && *previous_location == location
    {
        return Ok(cached);
    }
    let rules = serde_json::from_str::<Vec<serde_json::Value>>(&stored)
        .map_err(unavailable)
        .and_then(|values| LifecycleRule::from_stored(&values));
    match rules {
        Ok(mut rules) => {
            rules.retain(|rule| location.permits(rule.action));
            clear_quarantine(transaction, QuarantineKey::rules(bucket, incarnation))?;
            Ok(BucketRules::Ready {
                stored,
                location,
                rules,
            })
        }
        Err(error) => {
            if let Some(event) = quarantine(
                transaction,
                QuarantineKey::rules(bucket, incarnation),
                &stored,
                "lifecycle_rules_quarantined",
                &error,
            )? {
                diagnostics.push(event);
            }
            Ok(BucketRules::Quarantined)
        }
    }
}

fn evaluate_version(
    transaction: &rusqlite::Transaction<'_>,
    target: &ObjectTarget,
    rules: &[LifecycleRule],
    now: SystemTime,
    incarnation: &str,
    diagnostics: &mut Vec<QuarantineEvent>,
    limit: u64,
) -> Result<LifecycleCounts, ObjectError> {
    let key = QuarantineKey {
        bucket: &target.bucket,
        incarnation,
        name: &target.name,
        generation: target.generation.ok_or(ObjectError::ObjectNotFound)?,
    };
    let plan = match plan_version(transaction, target, rules, now) {
        Ok(plan) => plan,
        Err(error) if deterministic_read_error(&error) => {
            let revision: String = sql::scalar(
                transaction,
                "SELECT CAST(metageneration AS TEXT) FROM objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
                params![target.bucket, target.name, target.generation],
            )?;
            if let Some(event) = quarantine(
                transaction,
                key,
                &revision,
                "lifecycle_version_quarantined",
                &error,
            )? {
                diagnostics.push(event);
            }
            return Ok(LifecycleCounts {
                scanned: 1,
                skipped: 1,
                ..Default::default()
            });
        }
        Err(error) => return Err(error),
    };
    clear_quarantine(transaction, key)?;
    let (mut outcome, mut selected) = plan;
    while let Some((record, action)) = selected {
        match action {
            PlannedAction::Delete => {
                outcome.record(retire_version(transaction, &record, now)?);
                outcome.next = None;
                break;
            }
            PlannedAction::SetStorageClass(class) => {
                transition_version(transaction, record, class, now)?;
                outcome.transitioned += 1;
            }
        }
        if outcome.applied() == limit {
            outcome.next = Some(now);
            break;
        }
        // Re-evaluate the changed class in this transaction, up to the shared action budget.
        let (next, action) = plan_version(transaction, target, rules, now)?;
        outcome.blocked += next.blocked;
        outcome.next = next.next;
        selected = action;
    }
    Ok(outcome)
}

fn deterministic_read_error(error: &ObjectError) -> bool {
    match error {
        ObjectError::Corrupt(_)
        | ObjectError::InvalidFixedMetadata(_)
        | ObjectError::InvalidMetadata
        | ObjectError::Metadata(_) => true,
        ObjectError::Unavailable(cause) => cause.source().is_some_and(|source| {
            matches!(
                source.downcast_ref::<rusqlite::Error>(),
                Some(
                    rusqlite::Error::FromSqlConversionFailure(..)
                        | rusqlite::Error::IntegralValueOutOfRange(..)
                        | rusqlite::Error::InvalidColumnType(..)
                )
            )
        }),
        ObjectError::BucketNotFound
        | ObjectError::ObjectNotFound
        | ObjectError::PreconditionFailed
        | ObjectError::Component(_)
        | ObjectError::PublicAccessPrevention
        | ObjectError::OverwriteForbidden(_)
        | ObjectError::ActiveHold(_)
        | ObjectError::ObjectRetentionNotMet(_)
        | ObjectError::UnlockedRetentionOverrideRequired
        | ObjectError::LockedRetention
        | ObjectError::ObjectNotSoftDeleted
        | ObjectError::Denied(_)
        | ObjectError::Forbidden
        | ObjectError::NotModified
        | ObjectError::ChecksumMismatch
        | ObjectError::HashMismatch(_)
        | ObjectError::TooLarge
        | ObjectError::UniformAccessPredefinedAcl
        | ObjectError::InvalidDerivedArgument(_)
        | ObjectError::DerivedTransferNotFound
        | ObjectError::DerivedTransferExpired
        | ObjectError::DerivedTransferBusy
        | ObjectError::DerivedTransferIncomplete
        | ObjectError::UnsupportedMetadata(_)
        | ObjectError::CustomerEncryption(_)
        | ObjectError::BodyAborted(_)
        | ObjectError::Io(_) => false,
    }
}

fn plan_version(
    transaction: &rusqlite::Transaction<'_>,
    target: &ObjectTarget,
    rules: &[LifecycleRule],
    now: SystemTime,
) -> Result<(LifecycleCounts, Option<(ObjectRecord, PlannedAction)>), ObjectError> {
    let object = match objects::selected(
        transaction,
        &target.bucket,
        &target.name,
        target.generation,
        Default::default(),
        objects::RequestKind::Read,
        &BucketAccess::Permissive,
    ) {
        Ok(object) => object,
        Err(ObjectError::ObjectNotFound) => return Ok((LifecycleCounts::default(), None)),
        Err(error) => return Err(error),
    };
    let newer_versions = sql::scalar(
        transaction,
        "SELECT count(*) FROM objects WHERE bucket = ?1 AND name = ?2 AND generation > ?3 AND soft_delete_time IS NULL",
        params![target.bucket, target.name, target.generation],
    )?;
    let facts = version_facts(&object.record, newer_versions)?;
    let evaluation = evaluate(rules, &facts, now);
    let transition = matches!(
        evaluation,
        Evaluation::Act(PlannedAction::SetStorageClass(_))
    );
    let transition_due = || match evaluate(
        rules
            .iter()
            .filter(|rule| matches!(rule.action, LifecycleAction::SetStorageClass(_))),
        &facts,
        now,
    ) {
        Evaluation::Act(_) => None,
        Evaluation::Wait(next) => next.filter(|due| {
            matches!(evaluation, Evaluation::Wait(_))
                || matches!(
                    evaluate(rules, &facts, *due),
                    Evaluation::Act(PlannedAction::SetStorageClass(_))
                )
        }),
    };
    if !transition && object.record.has_hold() {
        return Ok((
            LifecycleCounts {
                scanned: 1,
                blocked: u64::from(matches!(evaluation, Evaluation::Act(PlannedAction::Delete))),
                next: transition_due(),
                ..Default::default()
            },
            None,
        ));
    }
    if !transition
        && let Some(until) = object.record.earliest_deletion_time()
        && until > crate::object::unix_nanos::UnixNanos::from(now)
    {
        let next = wakeup_at(until, now.into()).and_then(|until| {
            match evaluate(
                rules
                    .iter()
                    .filter(|rule| matches!(rule.action, LifecycleAction::Delete)),
                &facts,
                until,
            ) {
                Evaluation::Act(_) => Some(until),
                Evaluation::Wait(next) => next,
            }
        });
        let next = next.into_iter().chain(transition_due()).min();
        return Ok((
            LifecycleCounts {
                scanned: 1,
                blocked: u64::from(matches!(evaluation, Evaluation::Act(PlannedAction::Delete))),
                next,
                ..Default::default()
            },
            None,
        ));
    }
    let (action, next) = match evaluation {
        Evaluation::Act(action) => (Some((object.record, action)), None),
        Evaluation::Wait(next) => (None, next),
    };
    let outcome = LifecycleCounts {
        scanned: 1,
        next,
        ..Default::default()
    };
    Ok((outcome, action))
}

fn transition_version(
    transaction: &rusqlite::Transaction<'_>,
    mut record: ObjectRecord,
    class: StorageClass,
    now: SystemTime,
) -> Result<(), ObjectError> {
    record.metageneration = record
        .metageneration
        .checked_add(1)
        .ok_or_else(|| objects::object_unavailable("metageneration exhausted"))?;
    let micros = super::unix_micros(now.max(record.updated).max(record.time_created))?;
    record.updated = super::system_time_micros(micros)?;
    record.time_storage_class_updated = Some(record.updated);
    record.storage_class = class;
    sql::exec(
        transaction,
        "UPDATE objects SET storage_class = ?4, metageneration = ?5, updated = ?6, time_storage_class_updated = ?6 WHERE bucket = ?1 AND name = ?2 AND generation = ?3",
        params![
            record.bucket,
            record.name,
            record.generation,
            class,
            record.metageneration,
            micros
        ],
    )?;
    super::change_journal::object(
        transaction,
        super::admin::Origin::Api,
        super::change_journal::ChangeKind::Metadata,
        &record,
    )?;
    Ok(())
}

fn retire_version(
    transaction: &rusqlite::Transaction<'_>,
    record: &ObjectRecord,
    now: SystemTime,
) -> Result<LifecycleCounts, ObjectError> {
    record.check_retention(now)?;
    let target = ObjectTarget::new(
        &record.bucket,
        &record.name,
        record.time_deleted.map(|_| record.generation),
    );
    // The maintenance pass records soft-delete deadlines in its following expiry sweep.
    let deleted = objects::delete_in_transaction(
        transaction,
        &target,
        objects::DeleteContext {
            now,
            conditions: Default::default(),
            access: &BucketAccess::Permissive,
            sync_binding: None,
        },
    )?;
    let archived = deleted.retirement == objects::Retirement::Noncurrent;
    Ok(LifecycleCounts {
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
        .transpose()?;
    // Creation/noncurrent times are already SystemTime: catalog decoding rejects out-of-range values.
    // Custom-Time is accepted as RFC3339 text even before Windows' 1601 epoch; keep its integer time.
    Ok(VersionFacts {
        name: &record.name,
        size: record.size,
        storage_class: record.storage_class,
        time_created: record.time_created.into(),
        custom_time,
        noncurrent_time: record.time_deleted.map(UnixNanos::from),
        newer_versions,
    })
}

#[derive(Clone, Copy)]
struct QuarantineKey<'a> {
    bucket: &'a str,
    incarnation: &'a str,
    name: &'a str,
    generation: i64,
}

impl<'a> QuarantineKey<'a> {
    fn rules(bucket: &'a str, incarnation: &'a str) -> Self {
        Self {
            bucket,
            incarnation,
            name: "",
            generation: 0,
        }
    }
}

fn quarantine(
    transaction: &rusqlite::Transaction<'_>,
    key: QuarantineKey<'_>,
    revision: &str,
    event: &str,
    error: &dyn std::error::Error,
) -> Result<Option<QuarantineEvent>, ObjectError> {
    // History uses SQLite's bounded cache (and the memory backend's existing TEMP quota).
    let changed = transaction.prepare_cached("INSERT INTO lifecycle_quarantine (bucket, incarnation, name, generation, revision) VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (bucket, incarnation, name, generation) DO UPDATE SET revision = excluded.revision WHERE revision != excluded.revision")?
        .execute(params![key.bucket, key.incarnation, key.name, key.generation, revision])?;
    Ok((changed > 0).then(|| QuarantineEvent {
        event: event.to_owned(),
        fields: serde_json::json!({"bucket":key.bucket,"name":key.name,"generation":key.generation,"message":format!("Stored lifecycle input cannot be restored; it is skipped until repaired: {error}")}),
    }))
}

struct QuarantineEvent {
    event: String,
    fields: serde_json::Value,
}

fn emit_quarantines(events: Vec<QuarantineEvent>) {
    // A rolled-back page must not report a quarantine whose deduplication record also rolled back.
    for event in events {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Warn,
            &event.event,
            event.fields,
        );
    }
}

fn clear_quarantine(
    transaction: &rusqlite::Transaction<'_>,
    key: QuarantineKey<'_>,
) -> Result<(), ObjectError> {
    sql::exec(
        transaction,
        "DELETE FROM lifecycle_quarantine WHERE bucket = ?1 AND incarnation = ?2 AND name = ?3 AND generation = ?4",
        params![key.bucket, key.incarnation, key.name, key.generation],
    )?;
    Ok(())
}
