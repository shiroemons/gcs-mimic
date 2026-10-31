//! Persistent XML multipart state; ordinary objects appear only at completion.

use super::{Catalog, objects, resumable, system_time_millis, unix_millis};
use crate::{
    auth::Permission,
    bucket::{BucketAccess, CatalogError},
    object::{ObjectError, ObjectRecord},
    storage::failure::object_unavailable,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{fmt, path::Path, time::SystemTime};

mod listing;
mod maintenance;
mod transfer;
pub(crate) use listing::UploadQuery;
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
    incarnation: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Part {
    pub number: u16,
    pub etag: String,
    pub size: u64,
    pub updated: SystemTime,
    blob_id: String,
    md5: [u8; 16],
    crc32c: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct CompletedPart {
    pub number: u16,
    pub etag: String,
}

pub(crate) struct Lease {
    pub upload: Upload,
    access: BucketAccess,
    _guard: resumable::WriterGuard,
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
    let value = serde_json::from_str(&options).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let options = resumable::decode_options(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(Upload {
        target: Target {
            id: row.get(0)?,
            bucket: row.get(1)?,
            name: row.get(2)?,
        },
        incarnation: row.get(3)?,
        options,
        created: system_time_millis(row.get(5)?)?,
    })
}

fn active(
    connection: &Connection,
    target: &Target,
    access: &BucketAccess,
    now: i64,
) -> Result<Upload, Error> {
    objects::authorized_bucket(connection, &target.bucket, access)?;
    connection.prepare_cached("SELECT u.id,u.bucket,u.name,u.incarnation,u.options,u.created FROM xml_multipart_uploads u JOIN bucket_incarnations b ON b.bucket=u.bucket AND b.incarnation=u.incarnation WHERE u.id=?1 AND u.bucket=?2 AND u.name=?3 AND u.deadline>?4")
        .and_then(|mut statement| statement.query_row(params![target.id,target.bucket,target.name,now],read_upload))
        .optional().map_err(object_unavailable)?.ok_or(Error::NoSuchUpload)
}

fn read_part(row: &rusqlite::Row<'_>) -> rusqlite::Result<Part> {
    let md5: [u8; 16] = row.get(3)?;
    Ok(Part {
        number: row.get(0)?,
        blob_id: row.get(1)?,
        size: row.get::<_, i64>(2)? as u64,
        md5,
        crc32c: row.get(4)?,
        updated: system_time_millis(row.get(5)?)?,
        etag: format!("\"{}\"", crate::hashing::encode_lower(&md5)),
    })
}

fn part(connection: &Connection, id: &str, number: u16) -> Result<Part, Error> {
    connection.prepare_cached("SELECT number,blob_id,size,md5,crc32c,updated FROM xml_multipart_parts WHERE upload_id=?1 AND number=?2")
        .and_then(|mut statement| statement.query_row(params![id,number],read_part))
        .optional().map_err(object_unavailable)?.ok_or(Error::InvalidPart)
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
        crate::json::validate_object_name(name)
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
        let now = unix_millis(self.0.clock.now())?;
        let deadline = now
            .checked_add(LIFETIME_MILLIS)
            .ok_or_else(|| object_unavailable("multipart deadline overflow"))?;
        let upload = self
            .run_object(move |connection| {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(object_unavailable)?;
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
                let encoded = resumable::encode_options(&options).to_string();
                if encoded.len() > 65536 {
                    return Err(ObjectError::TooLarge);
                }
                let count: i64 = transaction
                    .prepare_cached("SELECT count(*) FROM xml_multipart_uploads")
                    .and_then(|mut s| s.query_row([], |r| r.get(0)))
                    .map_err(object_unavailable)?;
                if count >= 128 {
                    return Err(object_unavailable(
                        "retained multipart session limit reached",
                    ));
                }
                let incarnation: String = transaction
                    .prepare_cached("SELECT incarnation FROM bucket_incarnations WHERE bucket=?1")
                    .and_then(|mut s| s.query_row([&target.bucket], |r| r.get(0)))
                    .map_err(object_unavailable)?;
                transaction
                    .prepare_cached(
                        "INSERT INTO xml_multipart_uploads VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    )
                    .and_then(|mut s| {
                        s.execute(params![
                            target.id,
                            target.bucket,
                            target.name,
                            incarnation,
                            encoded,
                            now,
                            deadline
                        ])
                    })
                    .map_err(object_unavailable)?;
                transaction.commit().map_err(object_unavailable)?;
                Ok(Upload {
                    target,
                    incarnation,
                    options,
                    created: system_time_millis(now).map_err(object_unavailable)?,
                })
            })
            .await?;
        self.0.maintenance_wake.notify_one();
        Ok(upload)
    }

    pub(crate) async fn lease_xml_multipart(
        &self,
        target: Target,
        access: BucketAccess,
    ) -> Result<Lease, Error> {
        let activity = self.activity()?;
        let id = format!("multipart:{}", target.id);
        if !self
            .0
            .session_writers
            .lock()
            .map_err(object_unavailable)?
            .insert(id.clone())
        {
            return Err(Error::Busy);
        }
        let guard = resumable::WriterGuard {
            catalog: self.clone(),
            id,
            _activity: activity,
        };
        let checked = access.clone();
        let now = unix_millis(self.0.clock.now())?;
        let upload = self
            .run_object(move |connection| {
                let transaction = connection.transaction().map_err(object_unavailable)?;
                Ok(active(&transaction, &target, &checked, now))
            })
            .await??;
        Ok(Lease {
            upload,
            access,
            _guard: guard,
        })
    }

    pub(crate) async fn list_xml_parts(
        &self,
        target: Target,
        access: BucketAccess,
        marker: u16,
        limit: usize,
    ) -> Result<(Upload, Vec<Part>, bool), Error> {
        let now = unix_millis(self.0.clock.now())?;
        self.run_object(move |connection| {
            let transaction=connection.transaction().map_err(object_unavailable)?;
            let result=(|| -> Result<_,Error> {
                let upload=active(&transaction,&target,&access,now)?;
                let mut parts=transaction.prepare_cached("SELECT number,blob_id,size,md5,crc32c,updated FROM xml_multipart_parts WHERE upload_id=?1 AND number>?2 ORDER BY number LIMIT ?3")
                    .and_then(|mut s|s.query_map(params![target.id,marker,(limit.clamp(1,1000)+1) as i64],read_part)?.collect::<rusqlite::Result<Vec<_>>>()).map_err(object_unavailable)?;
                let more=parts.len()>limit.clamp(1,1000); if more {parts.pop();}
                Ok((upload,parts,more))
            })();
            Ok(result)
        }).await?
    }

    pub(crate) async fn abort_xml_multipart(&self, lease: Lease) -> Result<(), Error> {
        let catalog = self.clone();
        self.run_object(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(object_unavailable)?;
            let result = (|| -> Result<(), Error> {
                active(
                    &transaction,
                    &lease.upload.target,
                    &lease.access,
                    unix_millis(catalog.0.clock.now())?,
                )?;
                maintenance::retire(&transaction, &lease.upload.target.id)?;
                objects::commit_publication(transaction)?;
                Ok(())
            })();
            if let Err(error) = maintenance::drain_storage(
                connection,
                &catalog.0.data_dir,
                catalog.0.memory.as_deref(),
            ) {
                catalog.multipart_cleanup_failed(&error);
            }
            Ok(result)
        })
        .await?
    }

    fn multipart_cleanup_failed(&self, error: &ObjectError) {
        *self
            .0
            .maintenance_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error.to_string());
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Warn,
            "multipart_cleanup_deferred",
            serde_json::json!({"message":error.to_string()}),
        );
        self.0.maintenance_wake.notify_one();
    }
}

pub(super) fn recover(
    connection: &Connection,
    data_dir: &Path,
    now: i64,
) -> Result<(), CatalogError> {
    maintenance::recover(connection, data_dir, now)
}

pub(super) fn validate(connection: &Connection) -> Result<(), CatalogError> {
    let bounded:bool=connection.prepare_cached("SELECT (SELECT count(*) FROM xml_multipart_uploads)<=128 AND NOT EXISTS(SELECT 1 FROM xml_multipart_parts p LEFT JOIN xml_multipart_uploads u ON u.id=p.upload_id WHERE u.id IS NULL)")
        .and_then(|mut s|s.query_row([],|r|r.get(0))).map_err(super::unavailable)?;
    if !bounded {
        return Err(super::unavailable("invalid multipart catalog state"));
    }
    let mut statement=connection.prepare_cached("SELECT id,bucket,name,incarnation,options,created FROM xml_multipart_uploads ORDER BY id LIMIT 128").map_err(super::unavailable)?;
    let mut rows = statement.query([]).map_err(super::unavailable)?;
    while let Some(row) = rows.next().map_err(super::unavailable)? {
        read_upload(row).map_err(super::unavailable)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
