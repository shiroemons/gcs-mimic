//! Generation-pinned, resumable byte derivation shared by copy, rewrite and compose.

use super::{
    Catalog, objects,
    resumable::WriterGuard,
    resumable::writers::{ClaimError, KeyOrigin, WriterKey},
    system_time_micros, system_time_millis,
    transfer_io::{self, stage_path},
    unix_micros, unix_millis,
};
use crate::storage::blob_id::BlobId;
use crate::storage::catalog::sql;
use crate::{
    bucket::{BucketAccess, CatalogError},
    hashing::{Hasher, Hashes},
    object::{
        ComponentFailure, ComponentFailureKind, ExpectedHashes, ObjectConditions, ObjectError,
        ObjectMetadataUpdate, ObjectRecord, ObjectTarget,
    },
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{fs, path::Path};

const BUFFER_SIZE: usize = 64 * 1024;
const LIFETIME_MILLIS: i64 = 604_800_000;
const EPHEMERAL_LIFETIME_MILLIS: i64 = 900_000;
pub(super) const SCHEMA: &str = "ALTER TABLE objects ADD COLUMN component_count INTEGER CHECK(component_count IS NULL OR component_count > 0);
CREATE TABLE derived_transfers (
    id TEXT PRIMARY KEY CHECK(length(id) = 32),
    total INTEGER NOT NULL CHECK(total >= 0),
    offset INTEGER NOT NULL CHECK(offset >= 0 AND offset <= total),
    md5_state BLOB NOT NULL,
    crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
    ephemeral INTEGER NOT NULL CHECK(ephemeral IN (0, 1)),
    created INTEGER NOT NULL,
    deadline INTEGER NOT NULL CHECK(deadline > created),
    publishing INTEGER NOT NULL DEFAULT 0 CHECK(publishing IN (0, 1))
) WITHOUT ROWID;
CREATE TABLE derived_deletions (
    blob_id TEXT PRIMARY KEY CHECK(length(blob_id) = 32),
    transfer_id TEXT NOT NULL CHECK(length(transfer_id) = 32)
) WITHOUT ROWID;
CREATE TABLE derived_inputs (
    transfer_id TEXT NOT NULL,
    position INTEGER NOT NULL CHECK(position BETWEEN 0 AND 31),
    blob_id TEXT NOT NULL UNIQUE CHECK(length(blob_id) = 32),
    start INTEGER NOT NULL CHECK(start >= 0),
    length INTEGER NOT NULL CHECK(length >= 0),
    size INTEGER NOT NULL CHECK(size >= start AND length <= size - start),
    PRIMARY KEY(transfer_id, position)
) WITHOUT ROWID;
PRAGMA user_version = 10;";

use crate::storage::failure::object_unavailable;

#[cfg(test)]
use super::fault::{self, Step};

mod advance;
mod begin;
mod cleanup;
mod publish;
mod recovery;
mod source;

pub(crate) use cleanup::DerivedRequestGuard;
pub(crate) use publish::DerivedDestination;
pub(super) use recovery::recover;
pub(crate) use source::{DerivedSegment, DerivedSource, DerivedSourceSpec};

fn random_id() -> Result<BlobId, ObjectError> {
    BlobId::generate().map_err(object_unavailable)
}

fn deferred_cleanup(error: &dyn std::fmt::Display) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        "derived_cleanup_deferred",
        serde_json::json!({"message":format!("unreferenced derived data is reclaimed later: {error}")}),
    );
}

/// Whether a derived transfer can be continued by later requests, or belongs to one request
/// and is reclaimed when that request or the process ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransferKind {
    Continuable,
    Ephemeral,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DerivedTransfer {
    pub id: BlobId,
    pub total: u64,
    pub offset: u64,
    pub created: std::time::SystemTime,
    pub deadline: std::time::SystemTime,
}

struct Input {
    id: BlobId,
    start: u64,
    length: u64,
    size: u64,
}

fn inputs(connection: &Connection, id: &BlobId) -> Result<Vec<Input>, ObjectError> {
    connection.prepare_cached("SELECT blob_id, start, length, size FROM derived_inputs WHERE transfer_id = ?1 ORDER BY position LIMIT 33")?.query_map([id], |row| Ok(Input { id: row.get(0)?, start: row.get(1)?, length: row.get(2)?, size: row.get(3)? }))?.collect::<rusqlite::Result<_>>().map_err(object_unavailable)
}

pub(super) fn transfer(
    connection: &Connection,
    id: &BlobId,
    now: i64,
) -> Result<(DerivedTransfer, Hasher), ObjectError> {
    const SELECT: &str = "SELECT total, offset, md5_state, crc32c, created, deadline FROM derived_transfers WHERE id = ?1";
    let (total, offset, state, crc, created, deadline): (u64, u64, Vec<u8>, u32, i64, i64) =
        sql::row(connection, SELECT, [id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .optional()?
        .ok_or(ObjectError::DerivedTransferNotFound)?;
    if now >= deadline {
        return Err(ObjectError::DerivedTransferExpired);
    }
    let hasher = Hasher::from_md5_state(&state, crc, offset).map_err(object_unavailable)?;
    Ok((
        DerivedTransfer {
            id: *id,
            total,
            offset,
            created: system_time_millis(created)?,
            deadline: system_time_millis(deadline)?,
        },
        hasher,
    ))
}

impl Catalog {
    fn derived_guard(&self, id: &BlobId) -> Result<WriterGuard, ObjectError> {
        self.claim_writer(WriterKey::Derived(*id), KeyOrigin::Catalog)
            .map_err(|error| match error {
                ClaimError::Busy => ObjectError::DerivedTransferBusy,
                error => CatalogError::from(error).into(),
            })
    }
}

#[cfg(test)]
mod tests;
