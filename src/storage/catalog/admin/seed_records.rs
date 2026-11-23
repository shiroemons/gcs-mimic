//! What seeding recorded at each name it replayed ([`SeedRecord`]), and the buckets it took from
//! the fixture tree, as schema 59 keeps them.
//!
//! A record goes with the transaction that publishes, deletes or keeps what it describes, so the
//! catalog and the records never disagree after a commit. A bucket deletion and a purge drop the
//! records of the bucket's names in their own transaction, and a reset drops every record, so no
//! record claims an object one of them removed.
//!
//! A bucket mark names the incarnation of a bucket seeding created or took over for a top-level
//! fixture directory. A pass while the server runs creates a bucket only for a directory without
//! a mark, and leaves alone a marked directory whose bucket is gone or is another incarnation: a
//! bucket an API request, a purge or a reset deleted stays deleted until its directory is removed
//! and created again, or until a reseed or a restart replays the tree. Marks therefore outlive
//! the buckets they name. A startup replay or a reseed, which lists the whole tree or fails,
//! drops the mark of a directory it no longer found; a pass drops one only as it would delete a
//! file's object, once proofs of absence from consecutive passes, the first and the last a second
//! apart, showed the directory's name empty. A directory a pass could not read keeps its mark.

use super::replay::{Absence, Body, Fingerprint, SeedRecord, Version};
use crate::{
    bucket::CatalogError,
    fsutil::FileIdentity,
    hashing::Hashes,
    storage::catalog::{sql, system_time_micros, unix_micros},
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::collections::HashMap;

pub(in crate::storage::catalog) const SCHEMA_VERSION: i64 = 59;

/// Schema 59: the seed records and the bucket marks.
pub(in crate::storage::catalog) const SCHEMA: &str = "CREATE TABLE seed_objects (
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY,
    owned_generation INTEGER CHECK(owned_generation IS NULL OR (typeof(owned_generation) = 'integer' AND owned_generation > 0)),
    owned_metageneration INTEGER CHECK(owned_metageneration IS NULL OR (typeof(owned_metageneration) = 'integer' AND owned_metageneration > 0)),
    deleted_generation INTEGER CHECK(deleted_generation IS NULL OR (typeof(deleted_generation) = 'integer' AND deleted_generation > 0)),
    deleted_metageneration INTEGER CHECK(deleted_metageneration IS NULL OR (typeof(deleted_metageneration) = 'integer' AND deleted_metageneration > 0)),
    size INTEGER NOT NULL CHECK(typeof(size) = 'integer' AND size >= 0),
    md5 BLOB NOT NULL CHECK(typeof(md5) = 'blob' AND length(md5) = 16),
    crc32c INTEGER NOT NULL CHECK(typeof(crc32c) = 'integer' AND crc32c BETWEEN 0 AND 4294967295),
    device BLOB NOT NULL CHECK(typeof(device) = 'blob' AND length(device) = 8),
    inode BLOB NOT NULL CHECK(typeof(inode) = 'blob' AND length(inode) = 8),
    file_size BLOB NOT NULL CHECK(typeof(file_size) = 'blob' AND length(file_size) = 8),
    modified_ns BLOB NOT NULL CHECK(typeof(modified_ns) = 'blob' AND length(modified_ns) = 16),
    changed_ns BLOB NOT NULL CHECK(typeof(changed_ns) = 'blob' AND length(changed_ns) = 16),
    racy INTEGER NOT NULL CHECK(racy IN (0, 1)),
    absent_since INTEGER CHECK(absent_since IS NULL OR typeof(absent_since) = 'integer'),
    absent_pass INTEGER CHECK(absent_pass IS NULL OR (typeof(absent_pass) = 'integer' AND absent_pass > 0)),
    CHECK((owned_generation IS NULL) = (owned_metageneration IS NULL)),
    CHECK((deleted_generation IS NULL) = (deleted_metageneration IS NULL)),
    CHECK((absent_since IS NULL) = (absent_pass IS NULL)),
    PRIMARY KEY(bucket, name)
) WITHOUT ROWID;
CREATE TABLE seed_buckets (
    bucket TEXT PRIMARY KEY NOT NULL COLLATE BINARY,
    incarnation TEXT NOT NULL CHECK(typeof(incarnation) = 'text'),
    absent_since INTEGER CHECK(absent_since IS NULL OR typeof(absent_since) = 'integer'),
    absent_pass INTEGER CHECK(absent_pass IS NULL OR (typeof(absent_pass) = 'integer' AND absent_pass > 0)),
    CHECK((absent_since IS NULL) = (absent_pass IS NULL))
) WITHOUT ROWID;
PRAGMA user_version = 59;";

const SELECT_ALL: &str = "SELECT bucket, name, owned_generation, owned_metageneration, deleted_generation, deleted_metageneration, size, md5, crc32c, device, inode, file_size, modified_ns, changed_ns, racy, absent_since, absent_pass FROM seed_objects";
const SELECT_ONE: &str = "SELECT bucket, name, owned_generation, owned_metageneration, deleted_generation, deleted_metageneration, size, md5, crc32c, device, inode, file_size, modified_ns, changed_ns, racy, absent_since, absent_pass FROM seed_objects WHERE bucket = ?1 AND name = ?2";
const UPSERT: &str = "INSERT OR REPLACE INTO seed_objects (bucket, name, owned_generation, owned_metageneration, deleted_generation, deleted_metageneration, size, md5, crc32c, device, inode, file_size, modified_ns, changed_ns, racy, absent_since, absent_pass) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)";
const FOUND: &str = "UPDATE seed_objects SET absent_since = NULL, absent_pass = NULL WHERE bucket = ?1 AND name = ?2 AND absent_since IS NOT NULL";
const CLEAR_ABSENCES: &str = "UPDATE seed_objects SET absent_since = NULL, absent_pass = NULL WHERE absent_since IS NOT NULL";
const CLEAR_MARK_ABSENCES: &str = "UPDATE seed_buckets SET absent_since = NULL, absent_pass = NULL WHERE absent_since IS NOT NULL";
const FORGET: &str = "DELETE FROM seed_objects WHERE bucket = ?1 AND name = ?2";
const MARK: &str = "INSERT OR REPLACE INTO seed_buckets (bucket, incarnation) SELECT bucket, incarnation FROM bucket_incarnations WHERE bucket = ?1";
const UNMARK: &str = "DELETE FROM seed_buckets WHERE bucket = ?1";
const MARKS: &str = "SELECT bucket FROM seed_buckets";
const MARK_STATE: &str = "SELECT seed_buckets.incarnation, bucket_incarnations.incarnation FROM seed_buckets LEFT JOIN bucket_incarnations USING (bucket) WHERE seed_buckets.bucket = ?1";
const MARK_STATES: &str = "SELECT seed_buckets.bucket, seed_buckets.incarnation IS bucket_incarnations.incarnation, seed_buckets.absent_since, seed_buckets.absent_pass FROM seed_buckets LEFT JOIN bucket_incarnations USING (bucket)";
const MARK_ABSENCE: &str = "SELECT absent_since, absent_pass FROM seed_buckets WHERE bucket = ?1";
const MARK_ABSENT: &str =
    "UPDATE seed_buckets SET absent_since = ?2, absent_pass = ?3 WHERE bucket = ?1";
const MARK_FOUND: &str = "UPDATE seed_buckets SET absent_since = NULL, absent_pass = NULL WHERE bucket = ?1 AND absent_since IS NOT NULL";

/// The bucket and object name a record belongs to.
pub(crate) type RecordKey = (String, String);

/// Whether seeding took the bucket of a top-level directory, and whether that bucket is still
/// there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mark {
    /// Seeding never took the bucket, or forgot it once its directory went away.
    Unmarked,
    /// The bucket seeding took is the live one.
    Current,
    /// The bucket seeding took was deleted, or the live one is a later incarnation.
    Gone,
}

/// A marked directory: whether its bucket is still the one seeding took, and the run of passes
/// that proved the directory's name empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Marked {
    pub(crate) mark: Mark,
    pub(crate) absent: Option<Absence>,
}

/// A row of [`SELECT_ALL`] or [`SELECT_ONE`], whose columns follow the table's.
fn record(row: &Row<'_>) -> rusqlite::Result<(RecordKey, SeedRecord)> {
    const INVALID: &str = "invalid seed record";
    let version = |generation: usize| -> rusqlite::Result<Option<Version>> {
        let pair = row
            .get::<_, Option<i64>>(generation)?
            .zip(row.get::<_, Option<i64>>(generation + 1)?);
        Ok(pair.map(|(generation, metageneration)| Version {
            generation,
            metageneration,
        }))
    };
    let unsigned = |index: usize| -> rusqlite::Result<u64> {
        Ok(u64::from_be_bytes(sql::fixed_blob(row, index, INVALID)?))
    };
    let nanos = |index: usize| -> rusqlite::Result<i128> {
        Ok(i128::from_be_bytes(sql::fixed_blob(row, index, INVALID)?))
    };
    let size: i64 = row.get(6)?;
    let identity = FileIdentity {
        device: unsigned(9)?,
        inode: unsigned(10)?,
        size: unsigned(11)?,
        modified_ns: nanos(12)?,
        changed_ns: nanos(13)?,
    };
    let record = SeedRecord {
        owned: version(2)?,
        deleted: version(4)?,
        body: Body {
            size: u64::try_from(size)
                .map_err(|error| sql::conversion_error(6, rusqlite::types::Type::Integer, error))?,
            hashes: Hashes {
                md5: sql::fixed_blob(row, 7, INVALID)?,
                crc32c: row.get(8)?,
            },
        },
        fingerprint: Fingerprint::stored(identity, row.get(14)?),
        absent: absence(row, 15)?,
    };
    Ok(((row.get(0)?, row.get(1)?), record))
}

/// The run of proofs of absence in the columns `since` and `since + 1` of `row`, which the
/// table's checks keep both set or both null.
fn absence(row: &Row<'_>, since: usize) -> rusqlite::Result<Option<Absence>> {
    let Some((micros, pass)) = row
        .get::<_, Option<i64>>(since)?
        .zip(row.get::<_, Option<i64>>(since + 1)?)
    else {
        return Ok(None);
    };
    let pass = u64::try_from(pass)
        .map_err(|error| sql::conversion_error(since + 1, rusqlite::types::Type::Integer, error))?;
    Ok(Some(Absence {
        since: system_time_micros(micros)?,
        pass,
    }))
}

/// The columns that keep `absence`.
fn absence_columns(absence: Option<Absence>) -> Result<(Option<i64>, Option<i64>), CatalogError> {
    let Some(Absence { since, pass }) = absence else {
        return Ok((None, None));
    };
    Ok((
        Some(unix_micros(since)?),
        Some(i64::try_from(pass).map_err(crate::storage::catalog::unavailable)?),
    ))
}

/// Every record, by bucket and name. A seed tree holds at most what its entry limit admits, and
/// a record carries no body, so a replay keeps them all while it runs.
pub(crate) fn all(connection: &Connection) -> Result<HashMap<RecordKey, SeedRecord>, CatalogError> {
    Ok(connection
        .prepare_cached(SELECT_ALL)?
        .query_map([], record)?
        .collect::<rusqlite::Result<_>>()?)
}

/// The record at `bucket`/`name`, as the caller's transaction sees it.
pub(crate) fn get(
    connection: &Connection,
    bucket: &str,
    name: &str,
) -> Result<Option<SeedRecord>, CatalogError> {
    Ok(sql::row(connection, SELECT_ONE, [bucket, name], record)
        .optional()?
        .map(|(_, record)| record))
}

/// Keeps `record` at `bucket`/`name`, replacing any record there.
pub(crate) fn save(
    connection: &Connection,
    bucket: &str,
    name: &str,
    record: &SeedRecord,
) -> Result<(), CatalogError> {
    let identity = record.fingerprint.identity();
    let pair = |version: Option<Version>| {
        (
            version.map(|version| version.generation),
            version.map(|version| version.metageneration),
        )
    };
    let (owned_generation, owned_metageneration) = pair(record.owned);
    let (deleted_generation, deleted_metageneration) = pair(record.deleted);
    let (absent_since, absent_pass) = absence_columns(record.absent)?;
    sql::exec(
        connection,
        UPSERT,
        params![
            bucket,
            name,
            owned_generation,
            owned_metageneration,
            deleted_generation,
            deleted_metageneration,
            i64::try_from(record.body.size).map_err(crate::storage::catalog::unavailable)?,
            record.body.hashes.md5,
            record.body.hashes.crc32c,
            identity.device.to_be_bytes(),
            identity.inode.to_be_bytes(),
            identity.size.to_be_bytes(),
            identity.modified_ns.to_be_bytes(),
            identity.changed_ns.to_be_bytes(),
            record.fingerprint.racy(),
            absent_since,
            absent_pass,
        ],
    )?;
    Ok(())
}

/// Clears any proof of absence of the record at `bucket`/`name` ([`SeedRecord::found`]), writing
/// only where one was kept.
pub(crate) fn found(connection: &Connection, bucket: &str, name: &str) -> Result<(), CatalogError> {
    sql::exec(connection, FOUND, [bucket, name])?;
    Ok(())
}

/// Clears every proof of absence, of files and of marked directories, as a startup replay and a
/// reseed do: the passes after them start every wait over, and pass numbers, which start over
/// with the server, never meet a proof an earlier server kept.
pub(crate) fn clear_absences(connection: &Connection) -> Result<(), CatalogError> {
    sql::exec(connection, CLEAR_ABSENCES, [])?;
    sql::exec(connection, CLEAR_MARK_ABSENCES, [])?;
    Ok(())
}

/// Drops the record at `bucket`/`name`.
pub(crate) fn forget(
    connection: &Connection,
    bucket: &str,
    name: &str,
) -> Result<(), CatalogError> {
    sql::exec(connection, FORGET, [bucket, name])?;
    Ok(())
}

/// Marks the live bucket `bucket` as one seeding took for its directory, without a proof that
/// the directory is gone.
pub(crate) fn mark(connection: &Connection, bucket: &str) -> Result<(), CatalogError> {
    sql::exec(connection, MARK, [bucket])?;
    Ok(())
}

/// Forgets that seeding took a bucket for the directory `bucket`, which a replay that listed the
/// whole tree no longer found, or which consecutive passes proved gone.
pub(crate) fn unmark(connection: &Connection, bucket: &str) -> Result<(), CatalogError> {
    sql::exec(connection, UNMARK, [bucket])?;
    Ok(())
}

/// The run of passes that proved the marked directory `bucket` gone; none where it is unmarked.
pub(crate) fn mark_absence(
    connection: &Connection,
    bucket: &str,
) -> Result<Option<Absence>, CatalogError> {
    Ok(
        sql::row(connection, MARK_ABSENCE, [bucket], |row| absence(row, 0))
            .optional()?
            .flatten(),
    )
}

/// Keeps `absence` as the run of passes that proved the marked directory `bucket` gone.
pub(crate) fn mark_absent(
    connection: &Connection,
    bucket: &str,
    absence: Absence,
) -> Result<(), CatalogError> {
    let (since, pass) = absence_columns(Some(absence))?;
    sql::exec(connection, MARK_ABSENT, params![bucket, since, pass])?;
    Ok(())
}

/// Clears any proof that the marked directory `bucket` is gone, writing only where one was kept.
pub(crate) fn mark_found(connection: &Connection, bucket: &str) -> Result<(), CatalogError> {
    sql::exec(connection, MARK_FOUND, [bucket])?;
    Ok(())
}

/// The directories whose buckets seeding took.
pub(crate) fn marks(connection: &Connection) -> Result<Vec<String>, CatalogError> {
    Ok(connection
        .prepare_cached(MARKS)?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Whether seeding took the bucket `bucket`, and whether it is still the live one.
pub(crate) fn mark_of(connection: &Connection, bucket: &str) -> Result<Mark, CatalogError> {
    let state: Option<(String, Option<String>)> =
        sql::row(connection, MARK_STATE, [bucket], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    Ok(match state {
        None => Mark::Unmarked,
        Some((marked, Some(live))) if marked == live => Mark::Current,
        Some(_) => Mark::Gone,
    })
}

/// Each marked directory with whether its bucket is still the one seeding took, and its proofs of
/// absence; a directory missing here is [`Mark::Unmarked`].
pub(crate) fn mark_states(
    connection: &Connection,
) -> Result<HashMap<String, Marked>, CatalogError> {
    Ok(connection
        .prepare_cached(MARK_STATES)?
        .query_map([], |row| {
            let current: bool = row.get(1)?;
            let mark = if current { Mark::Current } else { Mark::Gone };
            let absent = absence(row, 2)?;
            Ok((row.get(0)?, Marked { mark, absent }))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
