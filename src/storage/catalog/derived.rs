//! Generation-pinned, resumable byte derivation shared by copy, rewrite and compose.

use super::{
    After, Catalog, UnixMillis, Work, objects,
    resumable::WriterGuard,
    resumable::writers::{ClaimError, KeyOrigin, WriterKey},
    system_time_micros,
    transfer_io::stage_path,
    unix_micros,
};
use crate::storage::blob_id::BlobId;
use crate::storage::catalog::sql::{self, CommitFailure};
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
pub(super) use source::PublicationPin;
pub(crate) use source::{DerivedSegment, DerivedSource, DerivedSourceSpec};

fn random_id() -> Result<BlobId, ObjectError> {
    BlobId::generate().map_err(object_unavailable)
}

fn deferred_cleanup(error: &dyn std::fmt::Display) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        crate::diagnostics::Event::DerivedCleanupDeferred,
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

// The `ephemeral` column: `CHECK(ephemeral IN (0, 1))`.
sql::integer_enum!(TransferKind, { Continuable => 0, Ephemeral => 1 });

impl TransferKind {
    /// How long a new transfer of this kind lives before maintenance reclaims it.
    fn lifetime_millis(self) -> i64 {
        match self {
            Self::Continuable => LIFETIME_MILLIS,
            Self::Ephemeral => EPHEMERAL_LIFETIME_MILLIS,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DerivedTransfer {
    pub id: BlobId,
    pub total: u64,
    pub offset: u64,
    pub created: std::time::SystemTime,
    pub deadline: std::time::SystemTime,
}

/// Owned by the single request that can advance this transfer. Recovery discards its SQL row.
pub(crate) struct RequestTransfer {
    progress: DerivedTransfer,
    hasher: Hasher,
}

impl RequestTransfer {
    pub(crate) fn progress(&self) -> &DerivedTransfer {
        &self.progress
    }
}

enum Checkpoint {
    Stored(BlobId),
    Request(RequestTransfer),
}

impl Checkpoint {
    fn id(&self) -> BlobId {
        match self {
            Self::Stored(id) => *id,
            Self::Request(state) => state.progress.id,
        }
    }

    fn read(
        &self,
        connection: &Connection,
        now: UnixMillis,
    ) -> Result<(DerivedTransfer, Hasher), ObjectError> {
        match self {
            Self::Stored(id) => transfer(connection, id, now),
            Self::Request(state) => {
                let deadline = sql::optional_scalar::<UnixMillis>(
                    connection,
                    "SELECT deadline FROM derived_transfers WHERE id=?1 AND ephemeral=1",
                    [state.progress.id],
                )?
                .ok_or(ObjectError::DerivedTransferNotFound)?;
                if now >= deadline {
                    return Err(ObjectError::DerivedTransferExpired);
                }
                let mut progress = state.progress.clone();
                progress.deadline = deadline.system_time()?;
                Ok((progress, state.hasher.clone()))
            }
        }
    }
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
    now: UnixMillis,
) -> Result<(DerivedTransfer, Hasher), ObjectError> {
    const SELECT: &str = "SELECT total, offset, md5_state, crc32c, created, deadline FROM derived_transfers WHERE id = ?1";
    let (total, offset, hasher, created, deadline) = sql::row(connection, SELECT, [id], |row| {
        let total = row.get(0)?;
        let offset = row.get(1)?;
        let md5_state = sql::blob(row, 2)?;
        let crc32c = row.get(3)?;
        let created: UnixMillis = row.get(4)?;
        let deadline: UnixMillis = row.get(5)?;
        Ok((
            total,
            offset,
            Hasher::from_md5_state(md5_state, crc32c, offset),
            created,
            deadline,
        ))
    })
    .optional()?
    .ok_or(ObjectError::DerivedTransferNotFound)?;
    if now >= deadline {
        return Err(ObjectError::DerivedTransferExpired);
    }
    let hasher = hasher.map_err(object_unavailable)?;
    Ok((
        DerivedTransfer {
            id: *id,
            total,
            offset,
            created: created.system_time()?,
            deadline: deadline.system_time()?,
        },
        hasher,
    ))
}

impl Catalog {
    fn derived_guard(&self, id: &BlobId) -> Result<WriterGuard, ObjectError> {
        self.claim_writer(WriterKey::Derived(*id), KeyOrigin::Holder)
            .map_err(derived_claim_error)
    }
}

fn derived_claim_error(error: ClaimError) -> ObjectError {
    match error {
        ClaimError::Busy => ObjectError::DerivedTransferBusy,
        error @ (ClaimError::Poisoned | ClaimError::Catalog(_)) => CatalogError::from(error).into(),
    }
}

#[cfg(test)]
mod tests;
