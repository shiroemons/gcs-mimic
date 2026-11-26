//! Durable local import progress; adapters own source verification, never SQL.
#![deny(clippy::unwrap_used, clippy::expect_used)]
#![expect(
    clippy::disallowed_methods,
    reason = "request-free: only migration, open validation, the startup import check and the offline import command call this module"
)]

use super::{Catalog, admin::AdminError, sql, unavailable};
use crate::{
    bucket::CatalogError,
    cause::{Link, TypedSource},
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{error::Error, fmt};

pub(crate) const MAX_PAGE: usize = 256;
pub(super) const SCHEMA: &str = "CREATE TABLE import_journal (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    source BLOB NOT NULL CHECK(typeof(source) = 'blob' AND length(source) = 32),
    fingerprint BLOB NOT NULL CHECK(typeof(fingerprint) = 'blob' AND length(fingerprint) = 32),
    expected INTEGER NOT NULL CHECK(typeof(expected) = 'integer' AND expected >= 0),
    cursor INTEGER NOT NULL CHECK(typeof(cursor) = 'integer' AND cursor BETWEEN 0 AND expected),
    active INTEGER NOT NULL CHECK(active IN (0, 1) AND (active = 1 OR cursor = expected))
) WITHOUT ROWID;
CREATE TABLE import_observations (
    ordinal INTEGER PRIMARY KEY CHECK(ordinal > 0),
    fingerprint BLOB NOT NULL CHECK(typeof(fingerprint) = 'blob' AND length(fingerprint) = 32),
    inode BLOB CHECK(inode IS NULL OR (typeof(inode) = 'blob' AND length(inode) = 8)),
    ctime_ns BLOB CHECK(ctime_ns IS NULL OR (typeof(ctime_ns) = 'blob' AND length(ctime_ns) = 16))
);
PRAGMA user_version = 16;";

pub(super) const FAILURE_SCHEMA: &str = "ALTER TABLE import_journal ADD COLUMN failure TEXT CHECK(failure IS NULL OR length(CAST(failure AS BLOB)) BETWEEN 1 AND 1024);
PRAGMA user_version = 18;";

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportBinding {
    pub source: [u8; 32],
    pub fingerprint: [u8; 32],
    pub expected_elements: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImportElement {
    pub ordinal: i64,
    pub fingerprint: [u8; 32],
    pub inode: Option<u64>,
    pub ctime_ns: Option<i128>,
}

impl ImportElement {
    /// The optional stat observation in its column encoding: big-endian bytes, NULL when
    /// unobserved.
    fn stat_columns(&self) -> (Option<[u8; 8]>, Option<[u8; 16]>) {
        (
            self.inode.map(u64::to_be_bytes),
            self.ctime_ns.map(i128::to_be_bytes),
        )
    }

    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            ordinal: row.get(0)?,
            fingerprint: row.get(1)?,
            inode: row.get::<_, Option<[u8; 8]>>(2)?.map(u64::from_be_bytes),
            ctime_ns: row.get::<_, Option<[u8; 16]>>(3)?.map(i128::from_be_bytes),
        })
    }
}

/// How one attempted element's publication ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ElementOutcome {
    Published,
    Rejected,
}

impl ElementOutcome {
    pub(super) fn of<T, E>(result: &Result<T, E>) -> Self {
        match result {
            Ok(_) => Self::Published,
            Err(_) => Self::Rejected,
        }
    }
}

/// One attempted element and how its publication ended.
pub(super) struct ElementResult<'a> {
    pub(super) element: &'a ImportElement,
    pub(super) outcome: ElementOutcome,
}

/// What the singleton journal row says about the import that owns the destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportState {
    /// Elements are being published.
    Active,
    /// A terminal failure stopped publication; only a fresh destination can import again.
    Failed(JournalFailure),
    /// `finish` committed every element and completed the startup seed.
    Finished,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportProgress {
    pub binding: ImportBinding,
    pub cursor: i64,
    pub state: ImportState,
}

impl ImportProgress {
    /// The journal still owns the destination: publishing, or stopped by a failure.
    pub fn is_open(&self) -> bool {
        matches!(self.state, ImportState::Active | ImportState::Failed(_))
    }

    pub fn failure(&self) -> Option<&JournalFailure> {
        match &self.state {
            ImportState::Failed(failure) => Some(failure),
            ImportState::Active | ImportState::Finished => None,
        }
    }

    /// Publication continues only for the same source while nothing has failed.
    fn accepts(&self, binding: &ImportBinding) -> bool {
        self.state == ImportState::Active && &self.binding == binding
    }

    fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        let state = match (row.get::<_, bool>(4)?, row.get(5)?) {
            (true, None) => ImportState::Active,
            (true, Some(failure)) => ImportState::Failed(failure),
            (false, None) => ImportState::Finished,
            // Opening the catalog already refuses this row, so only a later corruption reads it.
            (false, Some(_)) => {
                return Err(super::sql::conversion_error(
                    5,
                    rusqlite::types::Type::Text,
                    "failure recorded on a finished import",
                ));
            }
        };
        Ok(Self {
            binding: ImportBinding {
                source: row.get(0)?,
                fingerprint: row.get(1)?,
                expected_elements: row.get(2)?,
            },
            cursor: row.get(3)?,
            state,
        })
    }
}

/// Why an import stopped for good, as stored in `import_journal.failure`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JournalFailure {
    /// A publication batch rejected an element (`element_rejected`).
    ElementRejected,
    /// The import adapter stopped with this diagnostic.
    Adapter(String),
}

impl JournalFailure {
    const ELEMENT_REJECTED: &str = "element_rejected";

    /// At most 256 characters, so the 1024-byte column check holds; empty becomes
    /// `adapter_failure`.
    fn adapter(reason: impl Into<String>) -> Self {
        let reason: String = reason.into().chars().take(256).collect();
        Self::Adapter(if reason.is_empty() {
            "adapter_failure".into()
        } else {
            reason
        })
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::ElementRejected => Self::ELEMENT_REJECTED,
            Self::Adapter(reason) => reason,
        }
    }
}

impl fmt::Display for JournalFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl rusqlite::ToSql for JournalFailure {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::from(self.as_str()))
    }
}

impl rusqlite::types::FromSql for JournalFailure {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let text = String::column_result(value)?;
        Ok(if text == Self::ELEMENT_REJECTED {
            Self::ElementRejected
        } else {
            Self::Adapter(text)
        })
    }
}

#[derive(Clone)]
pub struct ImportJournal(Catalog);

#[derive(Debug)]
pub enum ResumeError {
    Catalog(CatalogError),
    PermanentlyFailed {
        reason: JournalFailure,
    },
    #[cfg(not(unix))]
    MissingLock,
}

impl fmt::Display for ResumeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog(error) => error.fmt(f),
            Self::PermanentlyFailed { reason } => write!(
                f,
                "import permanently failed: {reason}; preserve the source snapshot and restart into a new empty destination"
            ),
            #[cfg(not(unix))]
            Self::MissingLock => f.write_str("catalog.lock is missing; stop any server using the destination, restore catalog.lock and retry the same unfinished import"),
        }
    }
}

impl Error for ResumeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.typed_source().map(Link::as_error)
    }
}

impl TypedSource for ResumeError {
    fn typed_source(&self) -> Option<Link<'_>> {
        match self {
            Self::Catalog(error) => error.typed_source(),
            Self::PermanentlyFailed { .. } => None,
            #[cfg(not(unix))]
            Self::MissingLock => None,
        }
    }
}

impl From<CatalogError> for ResumeError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

impl From<ResumeError> for CatalogError {
    fn from(error: ResumeError) -> Self {
        match error {
            ResumeError::Catalog(error) => error,
            rejection @ ResumeError::PermanentlyFailed { .. } => {
                CatalogError::Unavailable(crate::cause::BackendCause::from_error(rejection))
            }
            #[cfg(not(unix))]
            rejection @ ResumeError::MissingLock => {
                CatalogError::Unavailable(crate::cause::BackendCause::from_error(rejection))
            }
        }
    }
}

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    super::sql::validate(connection,
        "SELECT (SELECT count(*) FROM import_journal) <= 1
         AND (SELECT count(*) FROM import_observations) = coalesce((SELECT cursor FROM import_journal), 0)
         AND NOT EXISTS(SELECT 1 FROM import_observations WHERE ordinal > (SELECT cursor FROM import_journal))
         AND NOT EXISTS(SELECT 1 FROM import_journal WHERE active = 0 AND (SELECT complete FROM startup_seed) != 1)",
        "invalid import journal state")
}

pub(super) fn in_progress(connection: &Connection) -> Result<bool, CatalogError> {
    Ok(progress(connection)?.is_some_and(|state| state.is_open()))
}

fn progress(connection: &Connection) -> Result<Option<ImportProgress>, CatalogError> {
    connection.query_row(
        "SELECT source, fingerprint, expected, cursor, active, failure FROM import_journal WHERE singleton = 1",
        [], ImportProgress::read,
    ).optional().map_err(unavailable)
}

pub(super) fn verify_resume(
    connection: &Connection,
    version: i64,
    source: [u8; 32],
) -> Result<(), ResumeError> {
    if version < 16 || !connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM import_journal WHERE singleton=1 AND active=1 AND source=?1)",
        [source], |row| row.get::<_, bool>(0),
    ).map_err(unavailable)? {
        return Err(CatalogError::PreconditionFailed.into());
    }
    if version >= 18
        && let Some(failure) = progress(connection)?.and_then(|state| state.failure().cloned())
    {
        return Err(ResumeError::PermanentlyFailed { reason: failure });
    }
    Ok(())
}

pub(super) fn matching(
    connection: &Connection,
    binding: &ImportBinding,
) -> Result<ImportProgress, CatalogError> {
    progress(connection)?
        .filter(|state| state.accepts(binding))
        .ok_or(CatalogError::PreconditionFailed)
}

impl Catalog {
    /// Trusted operator interface, independent of API authentication profiles.
    pub fn import_journal(&self) -> ImportJournal {
        ImportJournal(self.clone())
    }
}

pub(super) fn verify_destination(
    connection: &Connection,
    state: &ImportProgress,
) -> Result<(), CatalogError> {
    if state.state == ImportState::Active {
        let count: i64 = connection.query_row(
            "SELECT (SELECT count(*) FROM buckets) + (SELECT count(*) FROM objects)",
            [],
            |row| row.get(0),
        )?;
        if count != state.cursor {
            return Err(CatalogError::PreconditionFailed);
        }
    }
    Ok(())
}

pub(super) fn check_publication(
    connection: &Connection,
    binding: &ImportBinding,
    cursor: i64,
) -> Result<(), CatalogError> {
    let state = matching(connection, binding)?;
    verify_destination(connection, &state)?;
    if state.cursor != cursor {
        return Err(CatalogError::PreconditionFailed);
    }
    Ok(())
}

/// Records an attempted prefix inside the transaction that commits the same elements:
/// observations for every published element, the cursor after the leading published run, and
/// `element_rejected` when anything was rejected.
pub(super) fn record_outcomes(
    transaction: &sql::WriteTx<'_>,
    binding: &ImportBinding,
    cursor: i64,
    results: &[ElementResult<'_>],
) -> Result<(), CatalogError> {
    let state = matching(transaction, binding)?;
    if state.cursor != cursor || results.is_empty() || results.len() > MAX_PAGE {
        return Err(CatalogError::PreconditionFailed);
    }
    for (offset, result) in results.iter().enumerate() {
        let ordinal = cursor
            .checked_add(i64::try_from(offset).map_err(unavailable)? + 1)
            .filter(|ordinal| *ordinal <= binding.expected_elements)
            .ok_or(CatalogError::PreconditionFailed)?;
        if result.element.ordinal != ordinal {
            return Err(CatalogError::PreconditionFailed);
        }
    }
    for result in results {
        if result.outcome == ElementOutcome::Published {
            insert_observation(transaction, result.element)?;
        }
    }
    let published = results
        .iter()
        .take_while(|result| result.outcome == ElementOutcome::Published)
        .count();
    transaction.execute(
        "UPDATE import_journal SET cursor = ?1, failure = ?2 WHERE singleton = 1",
        params![
            cursor + i64::try_from(published).map_err(unavailable)?,
            (published != results.len()).then_some(JournalFailure::ElementRejected)
        ],
    )?;
    Ok(())
}

fn insert_observation(
    transaction: &sql::WriteTx<'_>,
    element: &ImportElement,
) -> Result<(), CatalogError> {
    let (inode, ctime_ns) = element.stat_columns();
    transaction.execute(
        "INSERT INTO import_observations VALUES (?1,?2,?3,?4)",
        params![element.ordinal, element.fingerprint, inode, ctime_ns],
    )?;
    Ok(())
}

pub(super) fn validate_failure(connection: &Connection) -> Result<(), CatalogError> {
    super::sql::validate(connection,
        "SELECT (SELECT count(*) FROM import_journal) <= 1
         AND NOT EXISTS(SELECT 1 FROM import_journal WHERE failure IS NOT NULL AND active = 0)
         AND NOT EXISTS(SELECT 1 FROM import_observations WHERE ordinal > coalesce((SELECT expected FROM import_journal),0))
         AND (SELECT count(*) FROM import_observations WHERE ordinal <= coalesce((SELECT cursor FROM import_journal),0)) = coalesce((SELECT cursor FROM import_journal),0)
         AND NOT EXISTS(SELECT 1 FROM import_journal WHERE failure IS NULL AND (SELECT count(*) FROM import_observations) != cursor)
         AND NOT EXISTS(SELECT 1 FROM import_journal WHERE active = 0 AND (SELECT complete FROM startup_seed) != 1)",
        "invalid import failure state")
}

impl ImportJournal {
    pub(crate) async fn progress(&self) -> Result<Option<ImportProgress>, AdminError> {
        self.0
            .run_admin(|connection| {
                let state = progress(connection)?;
                if let Some(state) = &state {
                    verify_destination(connection, state)?;
                }
                Ok(state)
            })
            .await
    }

    pub(crate) async fn fail(
        &self,
        binding: ImportBinding,
        reason: impl Into<String>,
    ) -> Result<(), AdminError> {
        let reason = JournalFailure::adapter(reason);
        self.0
            .run_admin(move |connection| {
                let transaction = connection.transaction()?;
                let state = progress(&transaction)?
                    .filter(|state| state.is_open() && state.binding == binding)
                    .ok_or(CatalogError::PreconditionFailed)?;
                if state.state == ImportState::Active {
                    transaction.execute(
                        "UPDATE import_journal SET failure = ?1 WHERE singleton = 1",
                        [reason],
                    )?;
                }
                transaction.commit().map_err(unavailable)
            })
            .await
    }

    pub async fn in_progress(&self) -> Result<bool, AdminError> {
        self.0.run_admin(|connection| in_progress(connection)).await
    }

    pub async fn begin(&self, binding: ImportBinding) -> Result<ImportProgress, AdminError> {
        if binding.expected_elements < 0 {
            return Err(AdminError::Invalid(
                "import count must be nonnegative".into(),
            ));
        }
        let expected = binding.expected_elements;
        self.0.run_admin(move |connection| {
            let transaction = connection.transaction()?;
            if let Some(state) = progress(&transaction)? {
                verify_destination(&transaction, &state)?;
                return if state.accepts(&binding) { Ok(state) }
                    else { Err(CatalogError::PreconditionFailed) };
            }
            let occupied: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM buckets) OR (SELECT complete FROM startup_seed WHERE singleton = 1)",
                [], |row| row.get(0),
            )?;
            if occupied { return Err(CatalogError::PreconditionFailed); }
            transaction.execute(
                "INSERT INTO import_journal (singleton,source,fingerprint,expected,cursor,active) VALUES (1, ?1, ?2, ?3, 0, 1)",
                params![binding.source, binding.fingerprint, expected],
            )?;
            transaction.commit()?;
            Ok(ImportProgress { binding, cursor: 0, state: ImportState::Active })
        }).await
    }

    pub(crate) async fn page(
        &self,
        binding: ImportBinding,
        after: i64,
        limit: usize,
    ) -> Result<Vec<ImportElement>, AdminError> {
        if !(1..=MAX_PAGE).contains(&limit) || !(0..=binding.expected_elements).contains(&after) {
            return Err(AdminError::Invalid(
                "import page requires a valid cursor and limit 1..=256".into(),
            ));
        }
        self.0.run_admin(move |connection| {
            matching(connection, &binding)?;
            connection.prepare_cached(
                "SELECT ordinal, fingerprint, inode, ctime_ns FROM import_observations WHERE ordinal > ?1 ORDER BY ordinal LIMIT ?2",
            )?.query_map(params![after, limit], ImportElement::read)?.collect::<rusqlite::Result<Vec<_>>>().map_err(unavailable)
        }).await
    }

    /// Caller must verify unchanged body digests before replacing a drifted observation.
    pub(crate) async fn revalidate(
        &self,
        binding: ImportBinding,
        before: ImportElement,
        after: ImportElement,
    ) -> Result<(), AdminError> {
        if before.ordinal != after.ordinal || before.fingerprint != after.fingerprint {
            return Err(AdminError::Invalid(
                "revalidation may only replace inode and ctime".into(),
            ));
        }
        self.0.run_admin(move |connection| {
            let transaction = connection.transaction()?;
            matching(&transaction, &binding)?;
            let (after_inode, after_ctime) = after.stat_columns();
            let (before_inode, before_ctime) = before.stat_columns();
            let updated = transaction.execute(
                "UPDATE import_observations SET inode = ?1, ctime_ns = ?2 WHERE ordinal = ?3 AND fingerprint = ?4 AND inode IS ?5 AND ctime_ns IS ?6",
                params![after_inode, after_ctime, before.ordinal, before.fingerprint, before_inode, before_ctime],
            )?;
            if updated != 1 { return Err(CatalogError::PreconditionFailed); }
            transaction.commit().map_err(unavailable)
        }).await
    }

    /// Adapter calls only after final fingerprint/count/digest validation succeeds.
    pub async fn finish(&self, binding: ImportBinding) -> Result<(), AdminError> {
        self.0
            .run_admin(move |connection| {
                let transaction = connection.transaction()?;
                let state = matching(&transaction, &binding)?;
                verify_destination(&transaction, &state)?;
                if state.cursor != binding.expected_elements {
                    return Err(CatalogError::PreconditionFailed);
                }
                transaction.execute(
                    "UPDATE import_journal SET active = 0 WHERE singleton = 1",
                    [],
                )?;
                transaction.execute(
                    "UPDATE startup_seed SET complete = 1 WHERE singleton = 1",
                    [],
                )?;
                transaction.commit().map_err(unavailable)
            })
            .await
    }
}
