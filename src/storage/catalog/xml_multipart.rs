//! Persistent XML multipart state; ordinary objects appear only at completion.

use super::resumable::writers::{ClaimError, KeyOrigin, WriterKey};
use super::{Catalog, UnixMillis, objects, resumable};
use crate::storage::blob_id::BlobId;
use crate::storage::catalog::{Incarnation, bucket_incarnation, sql};
use crate::{
    auth::Permission,
    bucket::{BucketAccess, CatalogError},
    object::{ObjectError, ObjectRecord},
    storage::failure::object_unavailable,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{fmt, path::Path, time::SystemTime};

mod listing;
mod maintenance;
mod transfer;
pub(crate) use listing::{UploadPage, UploadQuery};
pub(crate) use transfer::PartUpload;

pub(super) const SCHEMA: &str = "CREATE TABLE xml_multipart_uploads (
    id TEXT PRIMARY KEY CHECK(length(id) = 32),
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY CHECK(length(CAST(name AS BLOB)) BETWEEN 1 AND 1024),
    incarnation TEXT NOT NULL CHECK(length(incarnation) = 32),
    options TEXT NOT NULL CHECK(length(CAST(options AS BLOB)) <= 65536),
    created INTEGER NOT NULL,
    deadline INTEGER NOT NULL CHECK(deadline = created + 604800000)
) WITHOUT ROWID;
CREATE INDEX xml_multipart_uploads_list ON xml_multipart_uploads(bucket, name, id);
CREATE INDEX xml_multipart_uploads_deadline ON xml_multipart_uploads(deadline, id);
CREATE TABLE xml_multipart_parts (
    upload_id TEXT NOT NULL REFERENCES xml_multipart_uploads(id) ON DELETE CASCADE,
    number INTEGER NOT NULL CHECK(number BETWEEN 1 AND 10000),
    blob_id TEXT NOT NULL UNIQUE CHECK(length(blob_id) = 32),
    size INTEGER NOT NULL CHECK(size BETWEEN 0 AND 5368709120),
    md5 BLOB NOT NULL CHECK(length(md5) = 16),
    crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
    updated INTEGER NOT NULL,
    PRIMARY KEY(upload_id, number)
) WITHOUT ROWID;
CREATE TABLE xml_multipart_deletions (
    blob_id TEXT PRIMARY KEY CHECK(length(blob_id) = 32)
) WITHOUT ROWID;
PRAGMA user_version = 37;";

pub(crate) const MAX_PARTS: usize = 10_000;
const MAX_PART_NUMBER: u16 = 10_000;
pub(crate) const MAX_PART_BYTES: u64 = 5 * 1024 * 1024 * 1024;
const MIN_PART_BYTES: u64 = 5 * 1024 * 1024;
const LIFETIME_MILLIS: i64 = 604_800_000;

pub(crate) struct Options {
    pub content_type: String,
    pub metadata: crate::object::ObjectMetadataUpdate,
}

#[derive(Debug)]
pub(crate) enum Error {
    NoSuchUpload,
    InvalidPart,
    InvalidPartOrder,
    PartTooSmall,
    Busy,
    Object(ObjectError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoSuchUpload => f.write_str("The multipart upload does not exist"),
            Self::InvalidPart => f.write_str("A requested part or its ETag does not match"),
            Self::InvalidPartOrder => f.write_str("Parts must be in strictly ascending order"),
            Self::PartTooSmall => f.write_str("Every part except the last must be at least 5 MiB"),
            Self::Busy => f.write_str("A multipart operation is already in progress; retry"),
            Self::Object(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Object(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ObjectError> for Error {
    fn from(error: ObjectError) -> Self {
        Self::Object(error)
    }
}

impl From<CatalogError> for Error {
    fn from(error: CatalogError) -> Self {
        Self::Object(error.into())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub bucket: String,
    pub name: String,
    pub id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Upload {
    pub target: Target,
    pub options: resumable::ResumableOptions,
    pub created: SystemTime,
    incarnation: Incarnation,
}

#[derive(Clone, Debug)]
pub(crate) struct Part {
    pub number: u16,
    pub etag: String,
    pub size: u64,
    pub updated: SystemTime,
    blob_id: BlobId,
    md5: [u8; 16],
    crc32c: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct CompletedPart {
    pub number: u16,
    pub etag: String,
}

/// Writes one part. Other part numbers of the upload may write at the same time, so this lease
/// offers nothing that reads or retires the whole upload.
pub(crate) struct PartLease {
    pub upload: Upload,
    access: BucketAccess,
    number: u16,
    _guard: resumable::WriterGuard,
}

/// Owns a whole upload: no part is writing, and none starts until this lease drops.
pub(crate) struct UploadLease {
    pub upload: Upload,
    access: BucketAccess,
    _guard: resumable::WriterGuard,
}

fn claim_failure(error: ClaimError) -> Error {
    match error {
        ClaimError::Busy => Error::Busy,
        error @ (ClaimError::Poisoned | ClaimError::Catalog(_)) => CatalogError::from(error).into(),
    }
}

fn access_for(access: &BucketAccess, permission: Permission) -> BucketAccess {
    let mut access = access.clone();
    if let BucketAccess::Policy {
        permission: value, ..
    } = &mut access
    {
        *value = permission;
    }
    access
}

fn read_upload(row: &rusqlite::Row<'_>) -> rusqlite::Result<Upload> {
    let options: String = row.get(4)?;
    let value = serde_json::from_str(&options)
        .map_err(|error| sql::conversion_error(4, rusqlite::types::Type::Text, error))?;
    let options = resumable::decode_options(&value)
        .map_err(|error| sql::conversion_error(4, rusqlite::types::Type::Text, error))?;
    Ok(Upload {
        target: Target {
            id: row.get(0)?,
            bucket: row.get(1)?,
            name: row.get(2)?,
        },
        incarnation: row.get(3)?,
        options,
        created: row.get::<_, UnixMillis>(5)?.system_time()?,
    })
}

fn active(
    connection: &Connection,
    target: &Target,
    access: &BucketAccess,
    now: UnixMillis,
) -> Result<Upload, Error> {
    objects::authorized_bucket(connection, &target.bucket, access)?;
    sql::row(connection, "SELECT u.id,u.bucket,u.name,u.incarnation,u.options,u.created FROM xml_multipart_uploads u JOIN bucket_incarnations b ON b.bucket=u.bucket AND b.incarnation=u.incarnation WHERE u.id=?1 AND u.bucket=?2 AND u.name=?3 AND u.deadline>?4", params![target.id,target.bucket,target.name,now],read_upload).optional().map_err(object_unavailable)?.ok_or(Error::NoSuchUpload)
}

fn read_part(row: &rusqlite::Row<'_>) -> rusqlite::Result<Part> {
    let md5: [u8; 16] = row.get(3)?;
    Ok(Part {
        number: row.get(0)?,
        blob_id: row.get(1)?,
        size: row.get(2)?,
        md5,
        crc32c: row.get(4)?,
        updated: row.get::<_, UnixMillis>(5)?.system_time()?,
        etag: format!("\"{}\"", crate::hashing::encode_lower(&md5)),
    })
}

fn part(connection: &Connection, id: &str, number: u16) -> Result<Part, Error> {
    sql::row(connection, "SELECT number,blob_id,size,md5,crc32c,updated FROM xml_multipart_parts WHERE upload_id=?1 AND number=?2", params![id,number],read_part).optional().map_err(object_unavailable)?.ok_or(Error::InvalidPart)
}

impl Catalog {
    pub(crate) async fn begin_xml_multipart(
        &self,
        bucket: &str,
        name: &str,
        options: Options,
        access: BucketAccess,
    ) -> Result<Upload, Error> {
        let mut stored = resumable::ResumableOptions::new(BucketAccess::Permissive);
        stored.content_type = options.content_type;
        stored.metadata = options.metadata;
        let mut options = stored;
        crate::object::name::validate(name)
            .map_err(|_| ObjectError::InvalidDerivedArgument("Invalid object name".into()))?;
        options.metadata.validate()?;
        crate::object::validate_fixed_metadata("contentType", &options.content_type)?;
        options.access = BucketAccess::Permissive;
        let target = Target {
            bucket: bucket.into(),
            name: name.into(),
            id: super::super::blob_id::BlobId::generate()
                .map_err(object_unavailable)?
                .to_hex(),
        };
        let now = UnixMillis::try_from(self.0.clock.now())?;
        let deadline = now
            .checked_add_millis(LIFETIME_MILLIS)
            .ok_or_else(|| object_unavailable("multipart deadline overflow"))?;
        let upload = self
            .run(move |connection| {
                let transaction = sql::immediate(connection)?;
                let (storage_class, _) =
                    objects::authorized_bucket(&transaction, &target.bucket, &access)?;
                objects::authorized_bucket(
                    &transaction,
                    &target.bucket,
                    &access_for(&access, Permission::ObjectsCreate),
                )?;
                if options.metadata.storage_class.is_none() {
                    options.metadata.storage_class = Some(storage_class);
                }
                let encoded =
                    resumable::encode_new_options(&transaction, &target.bucket, &options)?;
                if encoded.len() > 65536 {
                    return Err(ObjectError::TooLarge);
                }
                let incarnation = bucket_incarnation(&transaction, &target.bucket)?;
                sql::exec(
                    &transaction,
                    "INSERT INTO xml_multipart_uploads VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        target.id,
                        target.bucket,
                        target.name,
                        incarnation,
                        encoded,
                        now,
                        deadline
                    ],
                )?;
                transaction.commit()?;
                Ok(Upload {
                    target,
                    incarnation,
                    options,
                    created: now.system_time()?,
                })
            })
            .await?;
        self.0.maintenance_wake.notify_one();
        Ok(upload)
    }

    /// Leases one part number. Only the same number waits; a body already streaming under another
    /// number does not hold this one back.
    pub(crate) async fn lease_xml_part(
        &self,
        target: Target,
        number: u16,
        access: BucketAccess,
    ) -> Result<PartLease, Error> {
        if !(1..=MAX_PART_NUMBER).contains(&number) {
            return Err(Error::InvalidPart);
        }
        let guard = self
            .wait_for_writer(
                WriterKey::MultipartPart(target.id.clone(), number),
                KeyOrigin::Client,
            )
            .await
            .map_err(claim_failure)?;
        let upload = self.active_upload(target, &access).await?;
        Ok(PartLease {
            upload,
            access,
            number,
            _guard: guard,
        })
    }

    /// Leases a whole upload for completion or abort, once the parts in flight have finished.
    pub(crate) async fn lease_xml_multipart(
        &self,
        target: Target,
        access: BucketAccess,
    ) -> Result<UploadLease, Error> {
        let guard = self
            .wait_for_whole_upload(&target.id)
            .await
            .map_err(claim_failure)?;
        let upload = self.active_upload(target, &access).await?;
        Ok(UploadLease {
            upload,
            access,
            _guard: guard,
        })
    }

    async fn active_upload(&self, target: Target, access: &BucketAccess) -> Result<Upload, Error> {
        let access = access.clone();
        let now = UnixMillis::try_from(self.0.clock.now())?;
        self.run(move |connection| {
            let transaction = connection.transaction().map_err(object_unavailable)?;
            active(&transaction, &target, &access, now)
        })
        .await
    }

    pub(crate) async fn list_xml_parts(
        &self,
        target: Target,
        access: BucketAccess,
        marker: u16,
        limit: usize,
    ) -> Result<(Upload, Vec<Part>, bool), Error> {
        let now = UnixMillis::try_from(self.0.clock.now())?;
        self.run(move |connection| -> Result<_, Error> {
            let transaction=connection.transaction().map_err(object_unavailable)?;
            let upload=active(&transaction,&target,&access,now)?;
            let mut parts=transaction.prepare_cached("SELECT number,blob_id,size,md5,crc32c,updated FROM xml_multipart_parts WHERE upload_id=?1 AND number>?2 ORDER BY number LIMIT ?3")
                .and_then(|mut s|s.query_map(params![target.id,marker,limit.clamp(1,1000)+1],read_part)?.collect::<rusqlite::Result<Vec<_>>>()).map_err(object_unavailable)?;
            let more=parts.len()>limit.clamp(1,1000); if more {parts.pop();}
            Ok((upload,parts,more))
        }).await
    }

    pub(crate) async fn abort_xml_multipart(&self, lease: UploadLease) -> Result<(), Error> {
        let permit = self.admit_object_upload()?;
        let catalog = self.clone();
        crate::task::spawn_supervised("xml-multipart-abort", async move {
            let _permit = permit;
            let operation = catalog.clone();
            let result = catalog
                .run(move |connection| {
                    let transaction = sql::immediate(connection).map_err(object_unavailable)?;
                    active(
                        &transaction,
                        &lease.upload.target,
                        &lease.access,
                        UnixMillis::try_from(operation.0.clock.now())?,
                    )?;
                    maintenance::retire(&transaction, &lease.upload.target.id)?;
                    objects::commit_publication(transaction)?;
                    operation.0.maintenance_wake.notify_one();
                    Ok(())
                })
                .await;
            if let Err(error) = catalog.drain_xml_multipart().await {
                catalog.multipart_cleanup_failed(error);
            }
            result
        })
        .await
        .map_err(object_unavailable)?
    }

    fn multipart_cleanup_failed(&self, error: ObjectError) {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Warn,
            "multipart_cleanup_deferred",
            serde_json::json!({"message":error.to_string()}),
        );
        self.record_maintenance_error(Some(crate::storage::failure::BackendCause::from_error(
            error,
        )));
        self.0.maintenance_wake.notify_one();
    }
}

pub(super) fn recover(
    connection: &Connection,
    data_dir: &Path,
    now: UnixMillis,
) -> Result<(), CatalogError> {
    maintenance::recover(connection, data_dir, now)
}

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    let consistent: bool = sql::scalar(
        connection,
        "SELECT NOT EXISTS(SELECT 1 FROM xml_multipart_parts p LEFT JOIN xml_multipart_uploads u ON u.id=p.upload_id WHERE u.id IS NULL)",
        [],
    )?;
    if !consistent {
        return Err(super::unavailable("invalid multipart catalog state"));
    }
    // Row by row: nothing bounds how many uploads a catalog holds, so neither may this check.
    let mut statement = connection.prepare_cached(
        "SELECT id,bucket,name,incarnation,options,created FROM xml_multipart_uploads ORDER BY id",
    )?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        read_upload(row)?;
    }
    Ok(())
}

#[cfg(test)]
impl Upload {
    /// An upload as listings report it; the incarnation only matters to the catalog.
    pub(crate) fn for_test(
        target: Target,
        options: resumable::ResumableOptions,
        created: SystemTime,
    ) -> Self {
        Self {
            target,
            options,
            created,
            incarnation: "00000000000000000000000000000000".parse().unwrap(),
        }
    }
}

#[cfg(test)]
impl Part {
    /// A part as listings report it; the blob and hashes only matter to completion.
    pub(crate) fn for_test(number: u16, etag: &str, size: u64, updated: SystemTime) -> Self {
        Self {
            number,
            etag: etag.to_owned(),
            size,
            updated,
            blob_id: BlobId::generate().unwrap(),
            md5: [0; 16],
            crc32c: 0,
        }
    }
}

#[cfg(test)]
mod tests;
