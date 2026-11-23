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
//! the buckets they name; only a replay that listed the whole tree drops one, for a directory it
//! no longer found.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "only the pass while the server runs, wired in the next step, clears a proof of absence or reads a bucket's mark"
    )
)]

use super::replay::{Body, Fingerprint, SeedRecord, Version};
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
    CHECK((owned_generation IS NULL) = (owned_metageneration IS NULL)),
    CHECK((deleted_generation IS NULL) = (deleted_metageneration IS NULL)),
    PRIMARY KEY(bucket, name)
) WITHOUT ROWID;
CREATE TABLE seed_buckets (
    bucket TEXT PRIMARY KEY NOT NULL COLLATE BINARY,
    incarnation TEXT NOT NULL CHECK(typeof(incarnation) = 'text')
) WITHOUT ROWID;
PRAGMA user_version = 59;";

const SELECT_ALL: &str = "SELECT bucket, name, owned_generation, owned_metageneration, deleted_generation, deleted_metageneration, size, md5, crc32c, device, inode, file_size, modified_ns, changed_ns, racy, absent_since FROM seed_objects";
const SELECT_ONE: &str = "SELECT bucket, name, owned_generation, owned_metageneration, deleted_generation, deleted_metageneration, size, md5, crc32c, device, inode, file_size, modified_ns, changed_ns, racy, absent_since FROM seed_objects WHERE bucket = ?1 AND name = ?2";
const UPSERT: &str = "INSERT OR REPLACE INTO seed_objects (bucket, name, owned_generation, owned_metageneration, deleted_generation, deleted_metageneration, size, md5, crc32c, device, inode, file_size, modified_ns, changed_ns, racy, absent_since) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)";
const FOUND: &str = "UPDATE seed_objects SET absent_since = NULL WHERE bucket = ?1 AND name = ?2 AND absent_since IS NOT NULL";
const FORGET: &str = "DELETE FROM seed_objects WHERE bucket = ?1 AND name = ?2";
const MARK: &str = "INSERT OR REPLACE INTO seed_buckets SELECT bucket, incarnation FROM bucket_incarnations WHERE bucket = ?1";
const UNMARK: &str = "DELETE FROM seed_buckets WHERE bucket = ?1";
const MARKS: &str = "SELECT bucket FROM seed_buckets";
const MARK_STATE: &str = "SELECT seed_buckets.incarnation, bucket_incarnations.incarnation FROM seed_buckets LEFT JOIN bucket_incarnations USING (bucket) WHERE seed_buckets.bucket = ?1";

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
        absent_since: row
            .get::<_, Option<i64>>(15)?
            .map(system_time_micros)
            .transpose()?,
    };
    Ok(((row.get(0)?, row.get(1)?), record))
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
    let absent_since = record.absent_since.map(unix_micros).transpose()?;
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

/// Drops the record at `bucket`/`name`.
pub(crate) fn forget(
    connection: &Connection,
    bucket: &str,
    name: &str,
) -> Result<(), CatalogError> {
    sql::exec(connection, FORGET, [bucket, name])?;
    Ok(())
}

/// Marks the live bucket `bucket` as one seeding took for its directory.
pub(crate) fn mark(connection: &Connection, bucket: &str) -> Result<(), CatalogError> {
    sql::exec(connection, MARK, [bucket])?;
    Ok(())
}

/// Forgets that seeding took a bucket for the directory `bucket`, which a replay that listed the
/// whole tree no longer found.
pub(crate) fn unmark(connection: &Connection, bucket: &str) -> Result<(), CatalogError> {
    sql::exec(connection, UNMARK, [bucket])?;
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

#[cfg(test)]
mod tests;
