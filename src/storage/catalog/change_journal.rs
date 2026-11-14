//! Local mutation feed; consumers reconcile current state after any rescan indication.
#![deny(clippy::unwrap_used, clippy::expect_used)]

use super::{
    Catalog,
    admin::{AdminError, Origin},
    unavailable,
};
use crate::storage::catalog::Incarnation;
use crate::storage::catalog::sql;
use crate::{
    bucket::{BucketAccess, CatalogError},
    object::ObjectRecord,
};
use rusqlite::{
    Connection, ToSql, params,
    types::{FromSql, FromSqlResult, ToSqlOutput, ValueRef},
};

#[cfg(test)]
struct WaitHook {
    phase: &'static str,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    used: std::sync::atomic::AtomicBool,
    checks: std::sync::atomic::AtomicUsize,
}
#[cfg(test)]
tokio::task_local! { static WAIT_HOOK: std::sync::Arc<WaitHook>; }
#[cfg(test)]
async fn wait_checkpoint(phase: &str) {
    if let Ok(hook) = WAIT_HOOK.try_with(std::sync::Arc::clone)
        && hook.phase == phase
    {
        hook.checks
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if !hook.used.swap(true, std::sync::atomic::Ordering::SeqCst) {
            hook.entered.notify_one();
            hook.release.notified().await;
        }
    }
}

pub(super) fn notify_on_write<T, E>(
    connection: &mut Connection,
    notify: &tokio::sync::Notify,
    operation: impl FnOnce(&mut Connection) -> Result<T, E>,
) -> Result<T, E> {
    let before = connection.total_changes();
    let result = operation(connection);
    // Rollbacks/unrelated writes may signal, so consumers must recheck the committed fence.
    if connection.total_changes() != before {
        notify.notify_waiters();
    }
    result
}

/// How many records the journal retains; a cursor older than that needs a rescan. A literal
/// macro, so the trimming statements spell the same number with `concat!`.
macro_rules! journal_capacity {
    () => {
        65536
    };
}
#[cfg(test)]
pub(crate) const MAX_RECORDS: i64 = journal_capacity!();
pub(crate) const MAX_PAGE: usize = 256;
pub(super) const SCHEMA: &str = "CREATE TABLE change_journal_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    sequence INTEGER NOT NULL CHECK(typeof(sequence) = 'integer' AND sequence >= 0),
    floor INTEGER NOT NULL CHECK(floor >= 0 AND floor <= sequence),
    epoch INTEGER NOT NULL CHECK(typeof(epoch) = 'integer' AND epoch >= 0),
    parent_pap TEXT NOT NULL
) WITHOUT ROWID;
INSERT INTO change_journal_state VALUES (1, 0, 0, 0, 0, '[]');
CREATE TABLE change_journal (
    sequence INTEGER PRIMARY KEY CHECK(sequence > 0),
    origin TEXT NOT NULL CHECK(origin IN ('api', 'seed', 'bucket-init', 'sync-ingest', 'import')),
    kind TEXT NOT NULL CHECK(kind IN ('publish', 'metadata', 'delete', 'bucket-create', 'bucket-update', 'bucket-delete', 'iam')),
    bucket TEXT NOT NULL,
    project TEXT NOT NULL,
    incarnation TEXT NOT NULL,
    object TEXT,
    generation INTEGER,
    metageneration INTEGER
);
PRAGMA user_version = 15;";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct JournalSeq(u64);

impl JournalSeq {
    pub fn get(self) -> u64 {
        self.0
    }
}

impl ToSql for JournalSeq {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

impl FromSql for JournalSeq {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        u64::column_result(value).map(Self)
    }
}

/// Capture once, then page to this position without chasing concurrent mutations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalFence {
    pub sequence: JournalSeq,
    pub revision: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChangeKind {
    Publish,
    Metadata,
    Delete,
    BucketCreate,
    BucketUpdate,
    BucketDelete,
    Iam,
}

sql::text_enum!(pub(crate) ChangeKind, "invalid change kind", {
    Publish => "publish", Metadata => "metadata", Delete => "delete",
    BucketCreate => "bucket-create", BucketUpdate => "bucket-update",
    BucketDelete => "bucket-delete", Iam => "iam",
});

sql::text_enum!(pub(crate) Origin, "invalid journal origin", {
    Api => "api", Seed => "seed", BucketInit => "bucket-init",
    SyncIngest => "sync-ingest", Import => "import",
});

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Change {
    pub sequence: JournalSeq,
    pub origin: Origin,
    pub kind: ChangeKind,
    pub bucket: String,
    pub project: String,
    pub incarnation: Incarnation,
    pub object: Option<String>,
    pub generation: Option<i64>,
    pub metageneration: Option<i64>,
}

pub(crate) struct ChangePage {
    pub items: Vec<Change>,
    pub next: JournalFence,
    pub fence: JournalFence,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "only unit tests read this field")
    )]
    pub has_more: bool,
}

/// One read of the change journal after a cursor.
pub(crate) enum ChangeRead {
    /// The cursor no longer continues this journal: rescan from `fence`.
    Rescan {
        fence: JournalFence,
    },
    Page(ChangePage),
}

/// Tests read a rescan as an empty page that ends at the current fence.
#[cfg(test)]
impl ChangeRead {
    pub(crate) fn rescan_required(&self) -> bool {
        matches!(self, Self::Rescan { .. })
    }

    pub(crate) fn fence(&self) -> JournalFence {
        match self {
            Self::Rescan { fence } | Self::Page(ChangePage { fence, .. }) => *fence,
        }
    }

    pub(crate) fn next(&self) -> JournalFence {
        match self {
            Self::Rescan { fence } | Self::Page(ChangePage { next: fence, .. }) => *fence,
        }
    }

    pub(crate) fn items(&self) -> &[Change] {
        match self {
            Self::Rescan { .. } => &[],
            Self::Page(page) => &page.items,
        }
    }

    pub(crate) fn has_more(&self) -> bool {
        matches!(self, Self::Page(page) if page.has_more)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChangeObservation {
    pub fence: JournalFence,
    pub rescan_required: bool,
}

fn valid_cursor(cursor: JournalFence, current: JournalFence, floor: JournalSeq) -> bool {
    cursor.revision == current.revision
        && cursor.sequence >= floor
        && cursor.sequence <= current.sequence
}

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    sql::validate(
        connection,
        concat!(
            "SELECT count(*) = 1 AND min(singleton) = 1 AND min(sequence) >= (SELECT coalesce(max(sequence), 0) FROM change_journal) AND (SELECT count(*) FROM change_journal) = min(sequence - floor) AND min(sequence - floor) <= ",
            journal_capacity!(),
            " AND NOT EXISTS(SELECT 1 FROM change_journal WHERE sequence <= (SELECT floor FROM change_journal_state)) FROM change_journal_state"
        ),
        "invalid change journal state",
    )
}

pub(super) fn origin(access: &BucketAccess) -> Origin {
    match access {
        BucketAccess::Admin(grant) => grant.origin(),
        BucketAccess::Permissive | BucketAccess::Policy { .. } => Origin::Api,
    }
}

pub(super) fn object(
    connection: &Connection,
    origin: Origin,
    kind: ChangeKind,
    record: &ObjectRecord,
) -> Result<(), CatalogError> {
    if kind == ChangeKind::Metadata && origin != Origin::Import {
        super::notification_outbox::enqueue(
            connection,
            crate::notification::Message {
                event: crate::notification::Event::MetadataUpdate,
                object: record,
                time: record.updated,
                replacement: None,
            },
        )?;
    }
    append(connection, origin, kind, &record.bucket, Some(record))
}

pub(super) fn append(
    connection: &Connection,
    origin: Origin,
    kind: ChangeKind,
    bucket: &str,
    object: Option<&ObjectRecord>,
) -> Result<(), CatalogError> {
    let enabled: bool = sql::scalar(
        connection,
        "SELECT enabled FROM change_journal_state WHERE singleton = 1",
        [],
    )?;
    if !enabled {
        return Ok(());
    }
    sql::exec(
        connection,
        "UPDATE change_journal_state SET sequence = sequence + 1 WHERE singleton = 1",
        [],
    )?;
    let inserted = sql::exec(
        connection,
        "INSERT INTO change_journal SELECT s.sequence, ?1, ?2, b.name, b.project, i.incarnation, ?4, ?5, coalesce(?6, b.metageneration) FROM change_journal_state s, buckets b JOIN bucket_incarnations i ON i.bucket = b.name WHERE b.name = ?3",
        params![
            origin,
            kind,
            bucket,
            object.map(|o| &o.name),
            object.map(|o| o.generation),
            object.map(|o| o.metageneration)
        ],
    )?;
    if inserted != 1 {
        return Err(unavailable("missing change journal bucket identity"));
    }
    // Each append evicts at most one record, even inside a larger mutation batch.
    sql::exec(
        connection,
        concat!(
            "DELETE FROM change_journal WHERE sequence <= (SELECT sequence - ",
            journal_capacity!(),
            " FROM change_journal_state) AND sequence IN (SELECT sequence FROM change_journal ORDER BY sequence LIMIT 256)"
        ),
        [],
    )?;
    sql::exec(
        connection,
        concat!(
            "UPDATE change_journal_state SET floor = max(floor, sequence - ",
            journal_capacity!(),
            ") WHERE singleton = 1"
        ),
        [],
    )?;
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "parent policy is installed once at startup"
)]
pub(super) fn parent_pap(connection: &Connection, projects: &[String]) -> Result<(), CatalogError> {
    let mut projects = projects.to_vec();
    projects.sort();
    projects.dedup();
    let value = serde_json::to_string(&projects).map_err(unavailable)?;
    let current: String = connection.query_row(
        "SELECT parent_pap FROM change_journal_state WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    if current != value {
        connection.execute("UPDATE change_journal_state SET parent_pap = ?1, epoch = epoch + 1 WHERE singleton = 1", [value])?;
    }
    Ok(())
}

/// The journal's position and whether it records changes.
struct JournalState {
    current: JournalFence,
    /// The oldest sequence a cursor can still continue from.
    floor: JournalSeq,
    enabled: bool,
}

fn state(connection: &Connection) -> Result<JournalState, CatalogError> {
    sql::row(
        connection,
        "SELECT sequence, epoch, floor, enabled FROM change_journal_state WHERE singleton = 1",
        [],
        |r| {
            Ok(JournalState {
                current: JournalFence {
                    sequence: r.get(0)?,
                    revision: r.get(1)?,
                },
                floor: r.get(2)?,
                enabled: r.get(3)?,
            })
        },
    )
    .map_err(unavailable)
}

impl Catalog {
    /// Effect-free and cancellable; no catalog permits are retained while asleep.
    pub(crate) async fn wait_for_changes(
        &self,
        after: JournalFence,
    ) -> Result<ChangeObservation, AdminError> {
        loop {
            #[cfg(test)]
            wait_checkpoint("before-register").await;
            let changed = self.0.change_wake.notified();
            let shutdown = self.0.admin_shutdown.notified();
            tokio::pin!(changed, shutdown);
            changed.as_mut().enable();
            shutdown.as_mut().enable();
            #[cfg(test)]
            wait_checkpoint("registered").await;
            let observation = self
                .run_admin(move |connection| {
                    let JournalState {
                        current,
                        floor,
                        enabled,
                    } = state(connection)?;
                    Ok(ChangeObservation {
                        fence: current,
                        rescan_required: !enabled || !valid_cursor(after, current, floor),
                    })
                })
                .await?;
            #[cfg(test)]
            wait_checkpoint("checked").await;
            if !self.is_healthy() {
                return Err(AdminError::Closed);
            }
            if observation.rescan_required || observation.fence != after {
                return Ok(observation);
            }
            tokio::select! {
                biased;
                _ = shutdown => return Err(AdminError::Closed),
                _ = changed => {},
            }
        }
    }

    /// Startup configuration belongs to consumers; activation always requires a fresh scan.
    /// Starts recording changes; returns the fence a consumer starts reading after.
    pub async fn enable_change_journal(&self) -> Result<JournalFence, AdminError> {
        self.set_change_journal(true).await
    }

    /// Stops recording changes; a later enable starts a new epoch.
    pub async fn disable_change_journal(&self) -> Result<JournalFence, AdminError> {
        self.set_change_journal(false).await
    }

    /// Stores `enabled` as the journal's state; the public API names each direction.
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "forwards the stored enabled column; enable_change_journal and disable_change_journal name each direction"
    )]
    async fn set_change_journal(&self, enabled: bool) -> Result<JournalFence, AdminError> {
        self.run_admin(move |connection| {
            sql::exec(connection, "UPDATE change_journal_state SET enabled = ?1, epoch = epoch + (enabled != ?1) WHERE singleton = 1", [enabled])?;
            Ok(state(connection)?.current)
        }).await
    }

    /// The journal's current fence, the baseline a reader scans catalog state at and then
    /// resumes from.
    pub(crate) async fn current_fence(&self) -> Result<JournalFence, AdminError> {
        self.run_admin(|connection| Ok(state(connection)?.current))
            .await
    }

    /// One read after `after`, up to `until` (the current fence when `None`).
    pub(crate) async fn read_changes(
        &self,
        after: JournalFence,
        until: Option<JournalFence>,
        limit: usize,
    ) -> Result<ChangeRead, AdminError> {
        let limit = super::PageLimit::<MAX_PAGE>::try_from(limit)
            .map_err(|_| AdminError::Invalid("change page limit must be 1..=256".into()))?;
        self.run_admin(move |connection| {
            let JournalState { current, floor, enabled } = state(connection)?;
            let target = until.unwrap_or(current);
            let valid = |f: JournalFence| valid_cursor(f, current, floor);
            let rescan = !enabled || !valid(after) || !valid(target) || after.sequence > target.sequence;
            if rescan { return Ok(ChangeRead::Rescan { fence: current }); }
            let mut statement = connection.prepare_cached("SELECT sequence, origin, kind, bucket, project, incarnation, object, generation, metageneration FROM change_journal WHERE sequence > ?1 AND sequence <= ?2 ORDER BY sequence LIMIT ?3")?;
            let items = statement.query_map(params![after.sequence, target.sequence, limit], |r| {
                Ok(Change { sequence: r.get(0)?, origin: r.get(1)?, kind: r.get(2)?, bucket: r.get(3)?, project: r.get(4)?, incarnation: r.get(5)?, object: r.get(6)?, generation: r.get(7)?, metageneration: r.get(8)? })
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
            let next = items.last().map_or(target, |c| JournalFence {sequence: c.sequence, revision: target.revision});
            Ok(ChangeRead::Page(ChangePage {has_more: next.sequence < target.sequence, items, next, fence: target}))
        }).await
    }
}

#[cfg(test)]
mod tests;
