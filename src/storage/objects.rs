use super::{Catalog, MAX_UPLOADS, system_time_micros, unavailable, unix_micros};
use crate::{
    bucket::{BucketAccess, CatalogError},
    hashing::{Hasher, Hashes},
    object::{
        ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate, ObjectRecord,
        UploadChunk,
    },
};
use bytes::Bytes;
use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params, types::Type,
};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{OwnedSemaphorePermit, mpsc, oneshot},
};

const CHUNK_SIZE: usize = 64 * 1024;
const OBJECT_COLUMNS: &str = "bucket, name, generation, metageneration, content_type, size,
    md5, crc32c, storage_class, time_created, updated, blob_id,
    content_encoding, content_disposition, content_language, cache_control, metadata,
    time_deleted, event_based_hold, component_count";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectListCursor {
    pub name: String,
    pub is_prefix: bool,
    pub generation: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectListQuery {
    pub bucket: String,
    pub prefix: String,
    pub delimiter: String,
    pub start_offset: String,
    pub end_offset: String,
    pub include_trailing_delimiter: bool,
    pub versions: bool,
    pub cursor: Option<ObjectListCursor>,
}

#[derive(Debug)]
pub struct ObjectListPage {
    pub items: Vec<ObjectRecord>,
    pub prefixes: Vec<String>,
    pub next: Option<ObjectListCursor>,
}

pub(super) const SCHEMA: &str = "CREATE TABLE objects (
            bucket TEXT NOT NULL COLLATE BINARY,
            name TEXT NOT NULL COLLATE BINARY,
            generation INTEGER NOT NULL CHECK(generation > 0),
            metageneration INTEGER NOT NULL CHECK(metageneration > 0),
            content_type TEXT NOT NULL,
            size INTEGER NOT NULL CHECK(size >= 0),
            md5 BLOB NOT NULL CHECK(length(md5) = 16),
            crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
            storage_class TEXT NOT NULL,
            time_created INTEGER NOT NULL,
            updated INTEGER NOT NULL,
            blob_id TEXT NOT NULL UNIQUE,
            PRIMARY KEY(bucket, name)
         ) WITHOUT ROWID;
         CREATE TABLE pending_blob_deletions (blob_id TEXT PRIMARY KEY) WITHOUT ROWID;
         PRAGMA user_version = 2;";

pub(super) const GENERATION_SCHEMA: &str = "CREATE TABLE object_generation (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    last_generation INTEGER NOT NULL CHECK(last_generation >= 0)
) WITHOUT ROWID;";

pub(super) const METADATA_SCHEMA: &str = "ALTER TABLE objects ADD COLUMN content_encoding TEXT;
    ALTER TABLE objects ADD COLUMN content_disposition TEXT;
    ALTER TABLE objects ADD COLUMN content_language TEXT;
    ALTER TABLE objects ADD COLUMN cache_control TEXT;
    ALTER TABLE objects ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';
    PRAGMA user_version = 4;";

pub(super) fn migrate_metadata(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction
        .execute_batch(METADATA_SCHEMA)
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

pub(super) const VERSIONING_SCHEMA: &str =
    "ALTER TABLE objects RENAME TO objects_before_versioning;
CREATE TABLE objects (
    bucket TEXT NOT NULL COLLATE BINARY,
    name TEXT NOT NULL COLLATE BINARY,
    generation INTEGER NOT NULL CHECK(generation > 0),
    metageneration INTEGER NOT NULL CHECK(metageneration > 0),
    content_type TEXT NOT NULL,
    size INTEGER NOT NULL CHECK(size >= 0),
    md5 BLOB NOT NULL CHECK(length(md5) = 16),
    crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295),
    storage_class TEXT NOT NULL,
    time_created INTEGER NOT NULL,
    updated INTEGER NOT NULL,
    blob_id TEXT NOT NULL UNIQUE,
    content_encoding TEXT,
    content_disposition TEXT,
    content_language TEXT,
    cache_control TEXT,
    metadata TEXT NOT NULL DEFAULT '{}',
    time_deleted INTEGER,
    event_based_hold INTEGER NOT NULL DEFAULT 0 CHECK(event_based_hold IN (0, 1)),
    PRIMARY KEY(bucket, name, generation)
) WITHOUT ROWID;
INSERT INTO objects (bucket, name, generation, metageneration, content_type, size,
    md5, crc32c, storage_class, time_created, updated, blob_id, content_encoding,
    content_disposition, content_language, cache_control, metadata, time_deleted,
    event_based_hold)
SELECT bucket, name, generation, metageneration, content_type, size, md5, crc32c,
    storage_class, time_created, updated, blob_id, content_encoding,
    content_disposition, content_language, cache_control, metadata, NULL, 0
FROM objects_before_versioning;
DROP TABLE objects_before_versioning;
CREATE UNIQUE INDEX objects_live ON objects(bucket, name) WHERE time_deleted IS NULL;
PRAGMA user_version = 9;";

pub(super) fn migrate_versioning(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction
        .execute_batch(VERSIONING_SCHEMA)
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

pub(super) fn migrate(connection: &mut Connection) -> Result<(), CatalogError> {
    let transaction = connection.transaction().map_err(unavailable)?;
    transaction.execute_batch(SCHEMA).map_err(unavailable)?;
    transaction
        .execute_batch(GENERATION_SCHEMA)
        .map_err(unavailable)?;
    transaction
        .execute("INSERT INTO object_generation VALUES (1, 0)", [])
        .map_err(unavailable)?;
    transaction
        .pragma_update(None, "user_version", 3)
        .map_err(unavailable)?;
    transaction.commit().map_err(unavailable)
}

pub(super) fn blob_path(data_dir: &Path, id: &str) -> Result<PathBuf, ObjectError> {
    super::super::blob_id::BlobId::parse(id)
        .map(|id| id.blob_path(data_dir))
        .ok_or_else(|| ObjectError::Unavailable("invalid opaque blob ID".into()))
}

#[cfg(unix)]
pub(super) fn sync_dir(path: &Path) -> io::Result<()> {
    crate::fsutil::sync_dir(path)
}

pub(super) fn publish_blob(source: &Path, destination: &Path) -> io::Result<()> {
    static SHARD_CREATION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let shard = destination.parent().expect("blob parent");
    {
        // A concurrent publisher must not pass a creator whose parent fsync is unfinished.
        let _creation = SHARD_CREATION
            .lock()
            .map_err(|error| io::Error::other(error.to_string()))?;
        match fs::create_dir(shard) {
            Ok(()) =>
            {
                #[cfg(unix)]
                if let Err(error) = sync_dir(shard.parent().expect("blobs directory")) {
                    fs::remove_dir(shard)?;
                    return Err(error);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(shard)?.is_dir() {
                    return Err(io::Error::other("blob shard is not a directory"));
                }
            }
            Err(error) => return Err(error),
        }
    }
    #[cfg(unix)]
    {
        fs::rename(source, destination)?;
        sync_dir(shard)
    }
    #[cfg(windows)]
    {
        // Preserve extended paths and keep publication on the same volume. Windows
        // uses write-through publication, not Unix directory-fsync durability.
        let source = source.canonicalize()?;
        let destination = shard
            .canonicalize()?
            .join(destination.file_name().expect("blob filename"));
        crate::fsutil::move_write_through(&source, &destination)
    }
}

pub(super) fn unlink(path: &Path) -> io::Result<()> {
    // Windows may defer removal until the final reader closes. Recovery also
    // scans unreferenced blobs, so a deferred deletion cannot become live data.
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn os_metadata(entry: &fs::DirEntry) -> bool {
    let name = entry.file_name();
    let name = name.to_string_lossy();
    name == ".DS_Store" || name.starts_with("._")
}

pub(super) fn reclaim(
    connection: &Connection,
    data_dir: &Path,
    id: &str,
) -> Result<(), ObjectError> {
    let referenced: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)",
            [id],
            |r| r.get(0),
        )
        .map_err(object_unavailable)?;
    if referenced {
        return Err(ObjectError::Unavailable(
            "deletion journal references a live blob".into(),
        ));
    }
    let path = blob_path(data_dir, id)?;
    unlink(&path).map_err(object_unavailable)?;
    #[cfg(unix)]
    if path.parent().expect("blob parent").exists() {
        sync_dir(path.parent().expect("blob parent")).map_err(object_unavailable)?;
    }
    connection
        .execute(
            "DELETE FROM pending_blob_deletions WHERE blob_id = ?1",
            [id],
        )
        .map_err(object_unavailable)?;
    Ok(())
}

pub(super) fn recover(
    connection: &Connection,
    data_dir: &Path,
    now: i64,
) -> Result<(), CatalogError> {
    let recover = || -> Result<(), ObjectError> {
        fs::create_dir_all(data_dir.join("staging")).map_err(object_unavailable)?;
        fs::create_dir_all(data_dir.join("blobs")).map_err(object_unavailable)?;
        #[cfg(unix)]
        {
            sync_dir(data_dir).map_err(object_unavailable)?;
            // A pre-crash shard creator may have stopped before making its parent durable.
            sync_dir(&data_dir.join("blobs")).map_err(object_unavailable)?;
        }
        super::resumable_recovery::recover(connection, data_dir).map_err(object_unavailable)?;
        super::derived::recover(connection, data_dir, now)?;
        for entry in fs::read_dir(data_dir.join("staging")).map_err(object_unavailable)? {
            let entry = entry.map_err(object_unavailable)?;
            if os_metadata(&entry) {
                continue;
            }
            let retained: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM resumable_sessions WHERE blob_id = ?1 AND state = 'ACTIVE') OR EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1) OR EXISTS(SELECT 1 FROM derived_inputs WHERE blob_id = ?1)", [entry.file_name().to_string_lossy().as_ref()], |r| r.get(0)).map_err(object_unavailable)?;
            if retained {
                continue;
            }
            unlink(&entry.path()).map_err(|error| {
                object_unavailable(format!("staging entry {}: {error}", entry.path().display()))
            })?;
        }
        #[cfg(unix)]
        sync_dir(&data_dir.join("staging")).map_err(object_unavailable)?;
        // Keyset pages bound memory independently of the number of objects or pending deletions.
        loop {
            let ids = connection
                .prepare("SELECT blob_id FROM pending_blob_deletions ORDER BY blob_id LIMIT 128")
                .map_err(object_unavailable)?
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(object_unavailable)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(object_unavailable)?;
            if ids.is_empty() {
                break;
            }
            for id in ids {
                reclaim(connection, data_dir, &id).map_err(|error| {
                    let reason = match error {
                        ObjectError::Unavailable(reason) => reason,
                        error => error.to_string(),
                    };
                    object_unavailable(format!("deletion journal blob {id}: {reason}"))
                })?;
            }
        }
        let mut after = String::new();
        loop {
            let rows = connection
                .prepare("SELECT blob_id, size FROM objects WHERE blob_id > ?1 ORDER BY blob_id LIMIT 128")
                .map_err(object_unavailable)?
                .query_map([&after], |r| Ok((r.get::<_, String>(0)?, read_size(r, 1)?)))
                .map_err(object_unavailable)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(object_unavailable)?;
            if rows.is_empty() {
                break;
            }
            for (id, size) in rows {
                let path = blob_path(data_dir, &id)?;
                let metadata = fs::symlink_metadata(&path).map_err(|error| {
                    object_unavailable(format!("referenced blob {id}: {error}"))
                })?;
                if !metadata.is_file() || metadata.len() != size {
                    return Err(object_unavailable(format!(
                        "referenced blob {id} has invalid size or type"
                    )));
                }
                after = id;
            }
        }
        // Directory iterators and indexed single-ID probes also keep orphan scans bounded.
        for shard in fs::read_dir(data_dir.join("blobs")).map_err(object_unavailable)? {
            let shard = shard.map_err(object_unavailable)?;
            if os_metadata(&shard) {
                continue;
            }
            if !shard.file_type().map_err(object_unavailable)?.is_dir() {
                return Err(object_unavailable(format!(
                    "invalid blob directory entry {}",
                    shard.path().display()
                )));
            }
            for blob in fs::read_dir(shard.path()).map_err(object_unavailable)? {
                let blob = blob.map_err(object_unavailable)?;
                if os_metadata(&blob) {
                    continue;
                }
                let id = blob.file_name().into_string().map_err(|_| {
                    object_unavailable(format!("invalid blob ID {}", blob.path().display()))
                })?;
                let expected = blob_path(data_dir, &id).map_err(|error| {
                    object_unavailable(format!("blob entry {}: {error}", blob.path().display()))
                })?;
                if expected != blob.path() {
                    return Err(object_unavailable(format!(
                        "blob in wrong shard {}",
                        blob.path().display()
                    )));
                }
                let referenced: bool = connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)",
                        [&id],
                        |r| r.get(0),
                    )
                    .map_err(object_unavailable)?;
                if !referenced {
                    unlink(&blob.path()).map_err(|error| {
                        object_unavailable(format!("blob entry {}: {error}", blob.path().display()))
                    })?;
                }
            }
            #[cfg(unix)]
            sync_dir(&shard.path()).map_err(object_unavailable)?;
        }
        Ok(())
    };
    recover().map_err(|error| match error {
        ObjectError::Unavailable(reason) => CatalogError::Unavailable(reason),
        error => unavailable(error),
    })
}

/// An open immutable generation. Unlinking its directory entry cannot change its bytes.
pub struct ObjectReader {
    file: tokio::fs::File,
    remaining: u64,
    /// Selected with the object and authorization, so a concurrent policy change cannot mix headers.
    pub(crate) publicly_readable: bool,
}

impl ObjectReader {
    pub async fn set_range(&mut self, offset: u64, length: u64) -> io::Result<()> {
        self.file.seek(io::SeekFrom::Start(offset)).await?;
        self.remaining = length;
        Ok(())
    }

    pub async fn next_chunk(&mut self) -> io::Result<Option<Bytes>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let mut buffer = vec![0; self.remaining.min(CHUNK_SIZE as u64) as usize];
        let length = self.file.read(&mut buffer).await?;
        if length == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        self.remaining -= length as u64;
        buffer.truncate(length);
        Ok(Some(Bytes::from(buffer)))
    }
}

impl Catalog {
    /// Each emitted entry requires one indexed seek, independent of a grouped prefix's size.
    #[cfg(test)]
    pub(crate) async fn list_objects(
        &self,
        query: ObjectListQuery,
        limit: usize,
    ) -> Result<ObjectListPage, ObjectError> {
        self.list_objects_authorized(query, limit, &BucketAccess::Permissive)
            .await
    }

    pub(crate) async fn list_objects_authorized(
        &self,
        query: ObjectListQuery,
        limit: usize,
        access: &BucketAccess,
    ) -> Result<ObjectListPage, ObjectError> {
        if !(1..=1000).contains(&limit) {
            return Err(object_unavailable(
                "object list limit must be between 1 and 1000",
            ));
        }
        let access = access.clone();
        self.run_object(move |connection| {
            let transaction = connection.transaction().map_err(object_unavailable)?;
            authorized_bucket(&transaction, &query.bucket, &access)?;
            list_page(&transaction, &query, limit)
        })
        .await
    }

    pub(super) async fn run_upload<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, ObjectError> + Send + 'static,
    ) -> Result<T, ObjectError> {
        let execution = self
            .0
            .execution
            .clone()
            .acquire_owned()
            .await
            .map_err(object_unavailable)?;
        self.run_upload_admitted(execution, operation).await
    }

    pub(super) async fn run_upload_admitted<T: Send + 'static>(
        &self,
        execution: OwnedSemaphorePermit,
        operation: impl FnOnce(&mut Connection) -> Result<T, ObjectError> + Send + 'static,
    ) -> Result<T, ObjectError> {
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let _execution = execution;
            let mut resources = inner
                .resources
                .lock()
                .map_err(|error| object_unavailable(error.to_string()))?;
            let resources = resources
                .as_mut()
                .ok_or_else(|| object_unavailable("catalog is closed"))?;
            super::change_journal::notify_on_write(
                &mut resources.connection,
                &inner.change_wake,
                operation,
            )
        })
        .await
        .map_err(object_unavailable)?
    }

    pub(super) async fn run_object<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, ObjectError> + Send + 'static,
    ) -> Result<T, ObjectError> {
        self.run(move |connection| Ok(operation(connection)))
            .await
            .map_err(object_unavailable)?
    }

    /// Advisory check that also rejects a bucket or an overwrite the caller is not authorized
    /// for. Publication repeats every part of it inside its own transaction.
    pub(crate) async fn check_upload(
        &self,
        bucket: &str,
        name: &str,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let access = access.clone();
        self.run_object(move |connection| {
            let transaction = connection.transaction().map_err(object_unavailable)?;
            let (_, allow_overwrite) = authorized_bucket(&transaction, &bucket, &access)?;
            let previous = lookup(&transaction, &bucket, &name)?;
            check_publication(previous.as_ref(), conditions, allow_overwrite)
        })
        .await
    }

    /// At most eight uploads stream concurrently; excess uploads fail without queuing.
    /// File I/O uses short Tokio filesystem jobs, never a blocking worker awaiting a body.
    #[allow(
        clippy::too_many_arguments,
        reason = "The shared storage contract fixes this signature"
    )]
    #[cfg(test)]
    pub(crate) async fn put_object(
        &self,
        bucket: &str,
        name: &str,
        content_type: String,
        expected: ExpectedHashes,
        conditions: ObjectConditions,
        max_bytes: u64,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
    ) -> Result<ObjectRecord, ObjectError> {
        let permit = self
            .prepare_object_upload(bucket, name, conditions, &BucketAccess::Permissive)
            .await?;
        self.put_object_prechecked(
            bucket,
            name,
            content_type,
            expected,
            conditions,
            max_bytes,
            body,
            permit,
            ObjectMetadataUpdate::default(),
            BucketAccess::Permissive,
        )
        .await
    }

    pub(crate) async fn prepare_object_upload(
        &self,
        bucket: &str,
        name: &str,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<OwnedSemaphorePermit, ObjectError> {
        let permit = self.admit_object_upload()?;
        self.check_upload(bucket, name, conditions, access).await?;
        Ok(permit)
    }

    pub(crate) fn admit_object_upload(&self) -> Result<OwnedSemaphorePermit, ObjectError> {
        let permit =
            self.0.uploads.clone().try_acquire_owned().map_err(|_| {
                object_unavailable(format!("upload limit of {MAX_UPLOADS} reached"))
            })?;
        if !self.is_healthy() {
            return Err(object_unavailable("catalog is closed"));
        }
        Ok(permit)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "The internal path retains the shared storage contract and its admission permit"
    )]
    pub(crate) async fn put_object_prechecked(
        &self,
        bucket: &str,
        name: &str,
        content_type: String,
        expected: ExpectedHashes,
        conditions: ObjectConditions,
        max_bytes: u64,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
        permit: OwnedSemaphorePermit,
        metadata: ObjectMetadataUpdate,
        // Rechecked at publication: the bucket may have been recreated under another project
        // while the body streamed, and overwriting needs storage.objects.delete there.
        access: BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        self.put_object_with_mode(
            bucket,
            name,
            content_type,
            expected,
            conditions,
            max_bytes,
            body,
            permit,
            metadata,
            access,
            super::admin::PublishMode::Replace,
            None,
        )
        .await
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Shared publication retains existing upload arguments"
    )]
    pub(super) async fn put_object_with_mode(
        &self,
        bucket: &str,
        name: &str,
        content_type: String,
        expected: ExpectedHashes,
        conditions: ObjectConditions,
        max_bytes: u64,
        body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
        permit: OwnedSemaphorePermit,
        metadata: ObjectMetadataUpdate,
        access: BucketAccess,
        mode: super::admin::PublishMode,
        identity: Option<super::admin::ImportedIdentity>,
    ) -> Result<ObjectRecord, ObjectError> {
        crate::object::validate_fixed_metadata("contentType", &content_type)?;
        metadata.validate()?;
        let catalog = self.clone();
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let (_request, cancellation) = oneshot::channel::<()>();
        // Cancellation must not release directory ownership during publication or cleanup.
        tokio::spawn(async move {
            let _permit = permit;
            let blob = super::super::blob_id::BlobId::generate().map_err(object_unavailable)?;
            let id = blob.to_hex();
            let stage = blob.staging_path(&catalog.0.data_dir);
            let published = blob.blob_path(&catalog.0.data_dir);
            let source = stage.clone();
            let destination = published.clone();
            let file = tokio::task::spawn_blocking(move || -> io::Result<File> {
                if destination.try_exists()? {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "blob ID collision",
                    ));
                }
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(source)
            })
            .await
            .map_err(object_unavailable)?
            .map_err(object_unavailable)?;
            let result = async {
                let (size, md5, crc32c) = write_body(
                    tokio::fs::File::from_std(file),
                    expected,
                    max_bytes,
                    body,
                    cancellation,
                )
                .await?;
                let source = stage.clone();
                let destination = published.clone();
                tokio::task::spawn_blocking(move || publish_blob(&source, &destination))
                    .await
                    .map_err(object_unavailable)?
                    .map_err(object_unavailable)?;
                let clock = catalog.0.clock.clone();
                let blob_id = id.clone();
                catalog
                    .run_upload(move |connection| {
                        let transaction = connection
                            .transaction_with_behavior(TransactionBehavior::Immediate)
                            .map_err(object_unavailable)?;
                        let (storage_class, allow_overwrite) =
                            authorized_bucket(&transaction, &bucket, &access)?;
                        let (versioning, default_hold) =
                            Catalog::bucket_settings_in_transaction(&transaction, &bucket)
                                .map_err(object_unavailable)?;
                        let previous = lookup(&transaction, &bucket, &name)?;
                        // Rechecked under the write lock so a concurrent create cannot be overwritten.
                        check_publication(previous.as_ref(), conditions, allow_overwrite)?;
                        if skip_same(mode, previous.as_ref(), size, md5, crc32c) {
                            return previous
                                .map(|(record, _)| record)
                                .ok_or(ObjectError::ObjectNotFound);
                        }
                        let now = clock.now();
                        if identity.is_none() {
                            now.duration_since(std::time::UNIX_EPOCH)
                                .map_err(object_unavailable)?;
                        }
                        let micros = unix_micros(now).map_err(object_unavailable)?;
                        let time = system_time_micros(micros).map_err(object_unavailable)?;
                        let (record, _retired, ()) = publish_in_transaction(
                            &transaction,
                            micros,
                            identity.map_or(GenerationAssignment::Automatic, |value| {
                                GenerationAssignment::Imported(value.generation())
                            }),
                            previous.as_ref(),
                            versioning,
                            &blob_id,
                            super::change_journal::origin(&access),
                            |generation| {
                                let mut record = ObjectRecord {
                                    bucket,
                                    name,
                                    generation,
                                    metageneration: 1,
                                    content_type,
                                    size,
                                    md5,
                                    crc32c,
                                    component_count: None,
                                    storage_class,
                                    content_encoding: None,
                                    content_disposition: None,
                                    content_language: None,
                                    cache_control: None,
                                    metadata: Default::default(),
                                    time_created: time,
                                    updated: time,
                                    time_deleted: None,
                                    event_based_hold: default_hold,
                                };
                                metadata.apply(&mut record)?;
                                inherit_metadata(mode, previous.as_ref(), &mut record);
                                if let Some(identity) = identity {
                                    record.time_created = identity.time_created();
                                    record.updated = identity.updated();
                                }
                                Ok((record, ()))
                            },
                        )?;
                        transaction.commit().map_err(object_unavailable)?;
                        Ok(record)
                    })
                    .await
            }
            .await;
            if result.is_ok() {
                catalog.reclaim_after_commit().await;
            }
            if result.is_err()
                || matches!(
                    mode,
                    super::admin::PublishMode::SkipIfSameHash
                        | super::admin::PublishMode::InheritAndSkipIfSameHash
                )
            {
                let data_dir = catalog.0.data_dir.clone();
                // Probe the committed reference before cleanup: SQL commit errors can be ambiguous.
                catalog
                    .run_upload(move |connection| {
                        unlink(&stage).map_err(object_unavailable)?;
                        let referenced: bool = connection
                            .query_row(
                                "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)",
                                [&id],
                                |r| r.get(0),
                            )
                            .map_err(object_unavailable)?;
                        if !referenced {
                            reclaim(connection, &data_dir, &id)?;
                        }
                        Ok(())
                    })
                    .await?;
            }
            result
        })
        .await
        .map_err(object_unavailable)?
    }

    #[cfg(test)]
    pub(crate) async fn get_object(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
    ) -> Result<ObjectRecord, ObjectError> {
        self.get_object_authorized(
            bucket,
            name,
            generation,
            conditions,
            &BucketAccess::Permissive,
        )
        .await
    }

    pub(crate) async fn get_object_authorized(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let access = access.clone();
        self.run_object(move |connection| {
            let transaction = connection.transaction().map_err(object_unavailable)?;
            selected(
                &transaction,
                &bucket,
                &name,
                generation,
                conditions,
                true,
                &access,
            )
            .map(|(record, _)| record)
        })
        .await
    }

    #[cfg(test)]
    pub(crate) async fn open_object(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
    ) -> Result<(ObjectRecord, ObjectReader), ObjectError> {
        self.open_object_authorized(
            bucket,
            name,
            generation,
            conditions,
            &BucketAccess::Permissive,
        )
        .await
    }

    /// The bucket's project is verified in the same call that resolves the generation and
    /// opens its blob, so the returned reader never belongs to an unauthorized bucket.
    pub(crate) async fn open_object_authorized(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(ObjectRecord, ObjectReader), ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let data_dir = self.0.data_dir.clone();
        let access = access.clone();
        self.run_object(move |connection| {
            open_selected(
                connection, &data_dir, &bucket, &name, generation, conditions, &access,
            )
        })
        .await
    }

    #[cfg(test)]
    pub(crate) async fn update_object_metadata(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        update: ObjectMetadataUpdate,
        validate: impl FnOnce(&ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<ObjectRecord, ObjectError> {
        self.update_object_metadata_authorized(
            bucket,
            name,
            generation,
            conditions,
            update,
            validate,
            &BucketAccess::Permissive,
        )
        .await
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "The public update contract plus the authorization it must recheck"
    )]
    pub(crate) async fn update_object_metadata_authorized(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        update: ObjectMetadataUpdate,
        validate: impl FnOnce(&ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
        access: &BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let clock = self.0.clock.clone();
        let access = access.clone();
        self.run_object(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(object_unavailable)?;
            let (mut record, _) = selected(
                &transaction,
                &bucket,
                &name,
                generation,
                conditions,
                false,
                &access,
            )?;
            validate(&record)?;
            update.apply(&mut record)?;
            record.metageneration = record
                .metageneration
                .checked_add(1)
                .ok_or_else(|| object_unavailable("metageneration exhausted"))?;
            let micros = unix_micros(clock.now()).map_err(object_unavailable)?;
            record.updated = system_time_micros(micros).map_err(object_unavailable)?;
            let metadata = serde_json::to_string(&record.metadata).map_err(object_unavailable)?;
            transaction
                .execute(
                    "UPDATE objects SET metageneration = ?3, updated = ?4, content_type = ?5,
                    content_encoding = ?6, content_disposition = ?7, content_language = ?8,
                    cache_control = ?9, metadata = ?10, event_based_hold = ?11
                    WHERE bucket = ?1 AND name = ?2 AND generation = ?12",
                    params![
                        bucket,
                        name,
                        record.metageneration,
                        micros,
                        record.content_type,
                        record.content_encoding,
                        record.content_disposition,
                        record.content_language,
                        record.cache_control,
                        metadata,
                        record.event_based_hold,
                        record.generation
                    ],
                )
                .map_err(object_unavailable)?;
            super::change_journal::object(
                &transaction,
                super::change_journal::origin(&access),
                super::change_journal::ChangeKind::Metadata,
                &record,
            )
            .map_err(object_unavailable)?;
            transaction.commit().map_err(object_unavailable)?;
            Ok(record)
        })
        .await
    }

    #[cfg(test)]
    pub(crate) async fn delete_object(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
    ) -> Result<(), ObjectError> {
        self.delete_object_authorized(
            bucket,
            name,
            generation,
            conditions,
            &BucketAccess::Permissive,
        )
        .await
    }

    pub(crate) async fn delete_object_authorized(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let access = access.clone();
        let clock = self.0.clock.clone();
        self.run_object(move |connection| {
            delete_selected(
                connection,
                clock.as_ref(),
                &bucket,
                &name,
                generation,
                conditions,
                &access,
                None,
            )
        })
        .await?;
        self.reclaim_after_commit().await;
        Ok(())
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "Shared selection preserves protocol conditions and access"
)]
pub(super) fn open_selected(
    connection: &mut Connection,
    data_dir: &Path,
    bucket: &str,
    name: &str,
    generation: Option<i64>,
    conditions: ObjectConditions,
    access: &BucketAccess,
) -> Result<(ObjectRecord, ObjectReader), ObjectError> {
    let transaction = connection.transaction().map_err(object_unavailable)?;
    let (record, id) = selected(
        &transaction,
        bucket,
        name,
        generation,
        conditions,
        true,
        access,
    )?;
    // Open under the same execution permit that serializes deletes and overwrites.
    let file = File::open(blob_path(data_dir, &id)?).map_err(object_unavailable)?;
    let remaining = record.size;
    let publicly_readable = super::policies::read_policy(&transaction, bucket)
        .map_err(object_unavailable)?
        .allows(
            None,
            crate::auth::OBJECTS_GET,
            super::policies::public_access_prevention(&transaction, bucket)
                .map_err(object_unavailable)?,
        );
    Ok((
        record,
        ObjectReader {
            file: tokio::fs::File::from_std(file),
            remaining,
            publicly_readable,
        },
    ))
}

#[allow(
    clippy::too_many_arguments,
    reason = "Shared selection preserves protocol conditions and access"
)]
pub(super) fn delete_selected(
    connection: &mut Connection,
    clock: &dyn crate::clock::Clock,
    bucket: &str,
    name: &str,
    generation: Option<i64>,
    conditions: ObjectConditions,
    access: &BucketAccess,
    sync_binding: Option<&super::sync_state::SyncBinding>,
) -> Result<(), ObjectError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(object_unavailable)?;
    let (record, id) = selected(
        &transaction,
        bucket,
        name,
        generation,
        conditions,
        false,
        access,
    )?;
    if record.event_based_hold {
        return Err(ObjectError::ActiveHold);
    }
    if let Some(binding) = sync_binding {
        super::sync_state::delete_in_transaction(&transaction, binding, &record)?;
    }
    let (versioning, _) = Catalog::bucket_settings_in_transaction(&transaction, bucket)
        .map_err(object_unavailable)?;
    super::change_journal::object(
        &transaction,
        super::change_journal::origin(access),
        super::change_journal::ChangeKind::Delete,
        &record,
    )
    .map_err(object_unavailable)?;
    retire_previous(
        &transaction,
        Some(&(record, id)),
        versioning && generation.is_none(),
        unix_micros(clock.now()).map_err(object_unavailable)?,
    )?;
    transaction.commit().map_err(object_unavailable)?;
    Ok(())
}

async fn write_body(
    mut file: tokio::fs::File,
    mut expected: ExpectedHashes,
    max_bytes: u64,
    mut body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
    mut cancellation: oneshot::Receiver<()>,
) -> Result<(u64, [u8; 16], u32), ObjectError> {
    let mut size = 0_u64;
    let mut hasher = Hasher::default();
    loop {
        let chunk = tokio::select! {
            biased;
            _ = &mut cancellation => return Err(ObjectError::BodyAborted("upload cancelled".into())),
            chunk = body.recv() => chunk,
        };
        let chunk = chunk
            .ok_or_else(|| ObjectError::BodyAborted("missing body EOF".into()))?
            .map_err(|error| {
                if error.kind() == io::ErrorKind::StorageFull {
                    ObjectError::Io(error.kind())
                } else {
                    ObjectError::BodyAborted(error.to_string())
                }
            })?;
        let chunk = match chunk {
            UploadChunk::End => break,
            UploadChunk::EndWithHashes(hashes) => {
                if hashes.md5.zip(expected.md5).is_some_and(|(a, b)| a != b)
                    || hashes
                        .crc32c
                        .zip(expected.crc32c)
                        .is_some_and(|(a, b)| a != b)
                {
                    return Err(ObjectError::ChecksumMismatch);
                }
                expected.md5 = expected.md5.or(hashes.md5);
                expected.crc32c = expected.crc32c.or(hashes.crc32c);
                break;
            }
            UploadChunk::Data(chunk) => chunk,
        };
        size = size
            .checked_add(chunk.len() as u64)
            .filter(|size| *size <= max_bytes)
            .ok_or(ObjectError::TooLarge)?;
        for part in chunk.chunks(CHUNK_SIZE) {
            hasher.update(part);
            file.write_all(part).await.map_err(object_unavailable)?;
        }
    }
    let Hashes { md5, crc32c } = hasher.finish();
    if expected.md5.is_some_and(|value| value != md5)
        || expected.crc32c.is_some_and(|value| value != crc32c)
    {
        return Err(ObjectError::ChecksumMismatch);
    }
    file.sync_all().await.map_err(object_unavailable)?;
    Ok((size, md5, crc32c))
}

pub(super) fn object_unavailable(error: impl std::fmt::Display + std::any::Any) -> ObjectError {
    if let Some(reason) = super::admin::corrupt_reason(&error) {
        return ObjectError::Corrupt(reason);
    }
    match super::admin::io_kind(&error) {
        Some(kind) => ObjectError::Io(kind),
        None => ObjectError::Unavailable(error.to_string()),
    }
}

fn prefix_end(prefix: &str) -> Option<Vec<u8>> {
    let mut end = prefix.as_bytes().to_vec();
    let index = end.iter().rposition(|byte| *byte != u8::MAX)?;
    end[index] += 1;
    end.truncate(index + 1);
    Some(end)
}

fn list_seek_sql(versions: bool, after: bool, bounded: bool) -> String {
    let (source, live, order) = if versions {
        ("objects", "", "name, generation")
    } else {
        // The primary key includes history; a live seek must not walk noncurrent rows.
        (
            "objects INDEXED BY objects_live",
            "AND time_deleted IS NULL",
            "name",
        )
    };
    let comparison = if after { ">" } else { ">=" };
    let upper = if bounded {
        "AND name < CAST(?3 AS TEXT)"
    } else {
        ""
    };
    format!(
        "SELECT {OBJECT_COLUMNS} FROM {source} WHERE bucket = ?1 AND name {comparison} CAST(?2 AS TEXT) {upper} {live} ORDER BY {order} LIMIT 1"
    )
}

pub(super) fn list_page(
    connection: &Connection,
    query: &ObjectListQuery,
    limit: usize,
) -> Result<ObjectListPage, ObjectError> {
    let mut page = ObjectListPage {
        items: Vec::new(),
        prefixes: Vec::new(),
        next: None,
    };
    let mut cursor = query.cursor.clone();
    let mut lower = query
        .prefix
        .as_bytes()
        .max(query.start_offset.as_bytes())
        .to_vec();
    let mut upper = prefix_end(&query.prefix);
    if !query.end_offset.is_empty()
        && upper
            .as_deref()
            .is_none_or(|end| query.end_offset.as_bytes() < end)
    {
        upper = Some(query.end_offset.as_bytes().to_vec());
    }
    let upper_clause = if upper.is_some() {
        "AND name < CAST(?3 AS TEXT)"
    } else {
        ""
    };
    let mut inclusive = connection
        .prepare(&list_seek_sql(query.versions, false, upper.is_some()))
        .map_err(object_unavailable)?;
    let mut exclusive = connection
        .prepare(&list_seek_sql(query.versions, true, upper.is_some()))
        .map_err(object_unavailable)?;
    let mut versioned = connection.prepare(&format!(
        "SELECT {OBJECT_COLUMNS} FROM objects WHERE bucket = ?1 AND (name, generation) > (CAST(?2 AS TEXT), ?4) {upper_clause} ORDER BY name, generation LIMIT 1"
    )).map_err(object_unavailable)?;
    loop {
        let mut after = false;
        let mut after_generation = None;
        let mut trailing_prefix = false;
        if let Some(previous) = &cursor {
            let bound = if previous.is_prefix {
                let Some(end) = prefix_end(&previous.name) else {
                    break;
                };
                end
            } else {
                previous.name.as_bytes().to_vec()
            };
            if bound >= lower {
                lower = bound;
                // A trailing marker emits an item and prefix as separate page entries.
                after = !previous.is_prefix && !trailing_marker(&previous.name, query);
                if query.versions && !previous.is_prefix {
                    after_generation = previous.generation;
                    if trailing_marker(&previous.name, query) {
                        let more: bool = connection.query_row(
                            "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND name = ?2 AND generation > ?3)",
                            params![query.bucket, previous.name, previous.generation], |row| row.get(0),
                        ).map_err(object_unavailable)?;
                        if !more {
                            after_generation = None;
                            trailing_prefix = true;
                        }
                    }
                }
            }
        }
        if upper
            .as_deref()
            .is_some_and(|upper| lower.as_slice() >= upper)
        {
            break;
        }
        let statement = if after_generation.is_some() {
            &mut versioned
        } else if after {
            &mut exclusive
        } else {
            &mut inclusive
        };
        let record = if let Some(generation) = after_generation {
            statement.query_row(params![query.bucket, lower, upper, generation], read_object)
        } else {
            match &upper {
                Some(upper) => {
                    statement.query_row(params![query.bucket, lower, upper], read_object)
                }
                None => statement.query_row(params![query.bucket, lower], read_object),
            }
        }
        .optional()
        .map_err(object_unavailable)?;
        let Some((record, _)) = record else { break };
        let group = group_prefix(&record.name, query);
        let is_prefix = group.is_some_and(|_| {
            !trailing_marker(&record.name, query)
                || cursor.as_ref().is_some_and(|cursor| {
                    !cursor.is_prefix
                        && cursor.name == record.name
                        && (!query.versions || trailing_prefix)
                })
        });
        if page.items.len() + page.prefixes.len() == limit {
            page.next = cursor;
            break;
        }
        let name = if is_prefix {
            group.expect("grouped entry").to_owned()
        } else {
            record.name.clone()
        };
        cursor = Some(ObjectListCursor {
            name: name.clone(),
            is_prefix,
            generation: (query.versions && !is_prefix).then_some(record.generation),
        });
        if is_prefix {
            page.prefixes.push(name);
        } else {
            page.items.push(record);
        }
    }
    Ok(page)
}

fn group_prefix<'a>(name: &'a str, query: &ObjectListQuery) -> Option<&'a str> {
    if query.delimiter.is_empty() {
        return None;
    }
    let remainder = name.strip_prefix(&query.prefix)?;
    let index = remainder.find(&query.delimiter)?;
    Some(&name[..query.prefix.len() + index + query.delimiter.len()])
}

fn trailing_marker(name: &str, query: &ObjectListQuery) -> bool {
    query.include_trailing_delimiter
        && group_prefix(name, query) == Some(name)
        && name
            .strip_suffix(&query.delimiter)
            .is_some_and(|stem| !stem.ends_with(&query.delimiter))
}

/// Resolves the bucket and re-validates `access` against the owning project observed on the
/// same connection, before any object state is read or changed.
///
/// Returns the storage class and whether replacing a live object is allowed. A missing bucket
/// is reported before authorization, matching the adapter's missing-bucket rule.
pub(super) fn authorized_bucket(
    connection: &Connection,
    bucket: &str,
    access: &BucketAccess,
) -> Result<(String, bool), ObjectError> {
    let (storage_class, project): (String, String) = connection
        .query_row(
            "SELECT storage_class, project FROM buckets WHERE name = ?1",
            [bucket],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(object_unavailable)?
        .ok_or(ObjectError::BucketNotFound)?;
    let allow_overwrite =
        super::check_access(connection, bucket, &project, access).map_err(|error| match error {
            CatalogError::Forbidden => ObjectError::Forbidden,
            error => object_unavailable(error),
        })?;
    Ok((storage_class, allow_overwrite))
}

pub(super) fn lookup(
    connection: &Connection,
    bucket: &str,
    name: &str,
) -> Result<Option<(ObjectRecord, String)>, ObjectError> {
    connection
        .query_row(
            &format!("SELECT {OBJECT_COLUMNS} FROM objects WHERE bucket = ?1 AND name = ?2 AND time_deleted IS NULL"),
            params![bucket, name],
            read_object,
        )
        .optional()
        .map_err(object_unavailable)
}

pub(super) fn selected(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: Option<i64>,
    conditions: ObjectConditions,
    read: bool,
    access: &BucketAccess,
) -> Result<(ObjectRecord, String), ObjectError> {
    authorized_bucket(connection, bucket, access)?;
    let (record, id) = if let Some(generation) = generation {
        connection.query_row(
            &format!("SELECT {OBJECT_COLUMNS} FROM objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3"),
            params![bucket, name, generation], read_object,
        ).optional().map_err(object_unavailable)?
    } else {
        lookup(connection, bucket, name)?
    }.ok_or(ObjectError::ObjectNotFound)?;
    check_conditions(Some(&record), conditions, read)?;
    Ok((record, id))
}

/// Preconditions are evaluated first: a failing precondition means nothing would be replaced.
pub(super) fn check_publication(
    previous: Option<&(ObjectRecord, String)>,
    conditions: ObjectConditions,
    allow_overwrite: bool,
) -> Result<(), ObjectError> {
    check_conditions(previous.map(|(record, _)| record), conditions, false)?;
    if previous.is_some() && !allow_overwrite {
        return Err(ObjectError::OverwriteForbidden);
    }
    if previous.is_some_and(|(record, _)| record.event_based_hold) {
        return Err(ObjectError::ActiveHold);
    }
    Ok(())
}

pub(super) fn skip_same(
    mode: super::admin::PublishMode,
    previous: Option<&(ObjectRecord, String)>,
    size: u64,
    md5: [u8; 16],
    crc32c: u32,
) -> bool {
    matches!(
        mode,
        super::admin::PublishMode::SkipIfSameHash
            | super::admin::PublishMode::InheritAndSkipIfSameHash
    ) && previous.is_some_and(|(record, _)| {
        record.size == size && record.md5 == md5 && record.crc32c == crc32c
    })
}

pub(super) fn inherit_metadata(
    mode: super::admin::PublishMode,
    previous: Option<&(ObjectRecord, String)>,
    record: &mut ObjectRecord,
) {
    if matches!(
        mode,
        super::admin::PublishMode::InheritFromReplaced
            | super::admin::PublishMode::InheritAndSkipIfSameHash
    ) && let Some((previous, _)) = previous
    {
        record.content_type.clone_from(&previous.content_type);
        record
            .content_encoding
            .clone_from(&previous.content_encoding);
        record
            .content_disposition
            .clone_from(&previous.content_disposition);
        record
            .content_language
            .clone_from(&previous.content_language);
        record.cache_control.clone_from(&previous.cache_control);
        record.metadata.clone_from(&previous.metadata);
    }
}

#[derive(Clone, Copy)]
pub(super) enum GenerationAssignment {
    Automatic,
    Imported(super::admin::Generation),
}

#[allow(
    clippy::too_many_arguments,
    reason = "Shared transaction receives generation assignment explicitly"
)]
/// The caller owns protocol-specific completion and commits before reclaiming retired bytes.
pub(super) fn publish_in_transaction<T>(
    transaction: &Transaction<'_>,
    micros: i64,
    assignment: GenerationAssignment,
    previous: Option<&(ObjectRecord, String)>,
    versioning: bool,
    blob_id: &str,
    origin: super::admin::Origin,
    prepare: impl FnOnce(i64) -> Result<(ObjectRecord, T), ObjectError>,
) -> Result<(ObjectRecord, Option<String>, T), ObjectError> {
    let last: i64 = transaction
        .query_row(
            "SELECT last_generation FROM object_generation WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(object_unavailable)?;
    let generation = match assignment {
        GenerationAssignment::Automatic => micros.max(
            last.checked_add(1)
                .ok_or_else(|| object_unavailable("generation exhausted"))?,
        ),
        GenerationAssignment::Imported(generation) => generation.get(),
    };
    let (record, prepared) = prepare(generation)?;
    if matches!(assignment, GenerationAssignment::Imported(_)) {
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM objects WHERE bucket = ?1 AND name = ?2 AND generation = ?3)",
            params![record.bucket, record.name, generation], |row| row.get(0),
        ).map_err(object_unavailable)?;
        if exists {
            return Err(ObjectError::PreconditionFailed);
        }
    }
    let metadata = serde_json::to_string(&record.metadata).map_err(object_unavailable)?;
    let retired = retire_previous(transaction, previous, versioning, micros)?;
    transaction
        .execute(
            "UPDATE object_generation SET last_generation = ?1 WHERE singleton = 1",
            [last.max(generation)],
        )
        .map_err(object_unavailable)?;
    transaction.execute(
        "INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,content_encoding,content_disposition,content_language,cache_control,metadata,event_based_hold,component_count) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?19,?11,?12,?13,?14,?15,?16,?17,?18)",
        params![record.bucket, record.name, generation, record.metageneration,
            record.content_type, i64::try_from(record.size).map_err(object_unavailable)?,
            &record.md5[..], record.crc32c, record.storage_class, unix_micros(record.time_created).map_err(object_unavailable)?, blob_id,
            record.content_encoding, record.content_disposition, record.content_language,
            record.cache_control, metadata, record.event_based_hold, record.component_count,
            unix_micros(record.updated).map_err(object_unavailable)?],
    ).map_err(object_unavailable)?;
    super::change_journal::object(
        transaction,
        origin,
        super::change_journal::ChangeKind::Publish,
        &record,
    )
    .map_err(object_unavailable)?;
    Ok((record, retired, prepared))
}

impl Catalog {
    pub(super) async fn reclaim_after_commit(&self) {
        if let Err(error) = self.reclaim_pending().await {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "blob_reclaim_deferred",
                serde_json::json!({"message":format!("deferred blob reclamation: {error}")}),
            );
        }
    }

    pub(super) async fn reclaim_pending(&self) -> Result<(), ObjectError> {
        let permit = self
            .0
            .reclamation
            .clone()
            .acquire_owned()
            .await
            .map_err(object_unavailable)?;
        let catalog = self.clone();
        // Detached callers retain the cleanup slot until filesystem work and SQL settle.
        let task = tokio::spawn(async move {
            let _permit = permit;
            let ids = catalog
                .run_upload(|connection| {
                    let ids = connection
                        .prepare(
                            // Cancelled sessions need their marker until staging cleanup also succeeds.
                            "SELECT blob_id FROM pending_blob_deletions AS pending WHERE NOT EXISTS (SELECT 1 FROM resumable_sessions AS session WHERE session.blob_id = pending.blob_id AND session.state != 'COMPLETE') ORDER BY blob_id LIMIT 128",
                        )
                        .map_err(object_unavailable)?
                        .query_map([], |row| row.get::<_, String>(0))
                        .map_err(object_unavailable)?
                        .collect::<rusqlite::Result<Vec<_>>>()
                        .map_err(object_unavailable)?;
                    for id in &ids {
                        let referenced: bool = connection
                            .query_row(
                                "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)",
                                [id],
                                |row| row.get(0),
                            )
                            .map_err(object_unavailable)?;
                        if referenced {
                            return Err(object_unavailable(
                                "deletion journal references a live blob",
                            ));
                        }
                    }
                    Ok(ids)
                })
                .await?;
            if ids.is_empty() {
                return Ok(());
            }
            let data_dir = catalog.0.data_dir.clone();
            let (removed, failure) = tokio::task::spawn_blocking(move || {
                let mut shards = std::collections::BTreeMap::<PathBuf, Vec<String>>::new();
                let mut failure = None;
                for id in ids {
                    let path = blob_path(&data_dir, &id)?;
                    match unlink(&path) {
                        Ok(()) => shards
                            .entry(path.parent().expect("blob parent").to_owned())
                            .or_default()
                            .push(id),
                        Err(error) => {
                            crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "blob_reclaim_deferred", serde_json::json!({"message":format!("deferred blob reclaim {id}: {error}")}));
                            failure = Some(object_unavailable(error));
                        }
                    }
                }
                let mut removed = Vec::new();
                for (shard, ids) in shards {
                    #[cfg(unix)]
                    if shard.exists()
                        && let Err(error) = sync_dir(&shard)
                    {
                        crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, "blob_reclaim_deferred", serde_json::json!({"message":format!("deferred blob reclaim directory: {error}")}));
                        failure = Some(object_unavailable(error));
                        continue;
                    }
                    #[cfg(not(unix))]
                    let _ = shard;
                    removed.extend(ids);
                }
                Ok::<_, ObjectError>((removed, failure))
            })
            .await
            .map_err(object_unavailable)??;
            catalog
                .run_upload(move |connection| {
                    let transaction = connection.transaction().map_err(object_unavailable)?;
                    for id in removed {
                        transaction
                            .execute(
                                "DELETE FROM pending_blob_deletions WHERE blob_id = ?1",
                                [&id],
                            )
                            .map_err(object_unavailable)?;
                        transaction
                            .execute("DELETE FROM derived_deletions WHERE blob_id = ?1", [&id])
                            .map_err(object_unavailable)?;
                    }
                    transaction.commit().map_err(object_unavailable)
                })
                .await?;
            failure.map_or(Ok(()), Err)
        });
        task.await.map_err(object_unavailable)?
    }
}

pub(super) fn retire_previous(
    connection: &Connection,
    previous: Option<&(ObjectRecord, String)>,
    retain: bool,
    micros: i64,
) -> Result<Option<String>, ObjectError> {
    let Some((_, id)) = previous else {
        return Ok(None);
    };
    if retain {
        connection
            .execute(
                "UPDATE objects SET time_deleted = ?2 WHERE blob_id = ?1",
                params![id, micros],
            )
            .map_err(object_unavailable)?;
        Ok(None)
    } else {
        connection
            .execute("DELETE FROM objects WHERE blob_id = ?1", [id])
            .map_err(object_unavailable)?;
        connection
            .execute("INSERT INTO pending_blob_deletions VALUES (?1)", [id])
            .map_err(object_unavailable)?;
        Ok(Some(id.clone()))
    }
}

fn check_conditions(
    record: Option<&ObjectRecord>,
    conditions: ObjectConditions,
    read: bool,
) -> Result<(), ObjectError> {
    let generation = record.map_or(0, |record| record.generation);
    let metageneration = record.map(|record| record.metageneration);
    if conditions
        .if_generation_match
        .is_some_and(|value| value != generation)
        || conditions
            .if_metageneration_match
            .is_some_and(|value| Some(value) != metageneration)
    {
        return Err(ObjectError::PreconditionFailed);
    }
    if conditions
        .if_generation_not_match
        .is_some_and(|value| record.is_none() || value == generation)
        || conditions
            .if_metageneration_not_match
            .is_some_and(|value| metageneration.is_none() || Some(value) == metageneration)
    {
        return Err(if read {
            ObjectError::NotModified
        } else {
            ObjectError::PreconditionFailed
        });
    }
    Ok(())
}

fn read_object(row: &Row<'_>) -> rusqlite::Result<(ObjectRecord, String)> {
    let md5: Vec<u8> = row.get(6)?;
    let md5 = md5.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(6, Type::Blob, "invalid MD5 length".into())
    })?;
    Ok((
        ObjectRecord {
            bucket: row.get(0)?,
            name: row.get(1)?,
            generation: row.get(2)?,
            metageneration: row.get(3)?,
            content_type: row.get(4)?,
            content_encoding: row.get(12)?,
            content_disposition: row.get(13)?,
            content_language: row.get(14)?,
            cache_control: row.get(15)?,
            metadata: serde_json::from_str(&row.get::<_, String>(16)?).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(16, Type::Text, Box::new(error))
            })?,
            size: read_size(row, 5)?,
            md5,
            crc32c: row.get(7)?,
            component_count: row.get(19)?,
            storage_class: row.get(8)?,
            time_created: system_time_micros(row.get(9)?)?,
            updated: system_time_micros(row.get(10)?)?,
            time_deleted: row
                .get::<_, Option<i64>>(17)?
                .map(system_time_micros)
                .transpose()?,
            event_based_hold: row.get(18)?,
        },
        row.get(11)?,
    ))
}

fn read_size(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    value.try_into().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(index, Type::Integer, Box::new(error))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::UNIX_EPOCH;

    /// # Safety
    /// `counter` must point to a live, aligned AtomicUsize whenever SQLite can invoke this callback.
    #[allow(unsafe_code)]
    unsafe extern "C" fn count_progress(counter: *mut std::ffi::c_void) -> std::ffi::c_int {
        // SAFETY: The caller guarantees a live, aligned AtomicUsize whenever SQLite can call us.
        let counter = unsafe { &*counter.cast::<AtomicUsize>() };
        i32::from(counter.fetch_add(1, Ordering::Relaxed) >= 100)
    }

    #[test]
    fn versioning_migration_copies_named_columns_from_permuted_legacy_table() {
        let mut connection = Connection::open_in_memory().unwrap();
        // Catalog validation rejects altered schemas; exercise migration mapping independently.
        let legacy = SCHEMA.replace(
            "bucket TEXT NOT NULL COLLATE BINARY,\n            name TEXT NOT NULL COLLATE BINARY,",
            "name TEXT NOT NULL COLLATE BINARY,\n            bucket TEXT NOT NULL COLLATE BINARY,",
        );
        assert_ne!(legacy, SCHEMA);
        connection.execute_batch(&legacy).unwrap();
        connection.execute_batch(METADATA_SCHEMA).unwrap();
        let columns = "bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,content_encoding,content_disposition,content_language,cache_control,metadata";
        connection.execute_batch(&format!(
            "INSERT INTO objects ({columns}) VALUES ('bucket','object',7,3,'text/plain',5,zeroblob(16),123,'NEARLINE',1234,5678,'opaque-blob','identity','inline','en','private','{{\"team\":\"storage\"}}');"
        )).unwrap();
        let selected = format!("SELECT {columns} FROM objects");
        let read_values = |row: &Row<'_>| {
            (0..17)
                .map(|index| row.get::<_, rusqlite::types::Value>(index))
                .collect::<rusqlite::Result<Vec<_>>>()
        };
        let before = connection.query_row(&selected, [], read_values).unwrap();
        migrate_versioning(&mut connection).unwrap();
        assert_eq!(
            connection.query_row(&selected, [], read_values).unwrap(),
            before
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT time_deleted, event_based_hold FROM objects",
                    [],
                    |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, bool>(1)?))
                )
                .unwrap(),
            (None, false)
        );
    }

    #[tokio::test]
    async fn object_range_reader_seeks_sparse_blob_and_bounds_every_chunk() {
        use std::io::{Seek, Write};

        struct SparseBlob(PathBuf);
        impl Drop for SparseBlob {
            fn drop(&mut self) {
                fs::remove_file(&self.0).unwrap();
            }
        }
        let path = temp_catalog_dir("range-sparse");
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let _blob = SparseBlob(path);
        let size = 64_u64 * 1024 * 1024;
        file.set_len(size).unwrap();
        file.seek(io::SeekFrom::Start(size - 3)).unwrap();
        file.write_all(b"end").unwrap();
        let mut reader = ObjectReader {
            file: tokio::fs::File::from_std(file),
            remaining: size,
            publicly_readable: false,
        };
        let offset = size - CHUNK_SIZE as u64 - 3;
        reader
            .set_range(offset, CHUNK_SIZE as u64 + 3)
            .await
            .unwrap();
        assert_eq!(reader.file.stream_position().await.unwrap(), offset);
        let chunk = reader.next_chunk().await.unwrap().unwrap();
        assert_eq!(chunk.len(), CHUNK_SIZE);
        assert!(chunk.iter().all(|byte| *byte == 0));
        assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"end"[..]);
        assert!(reader.next_chunk().await.unwrap().is_none());
        assert_eq!(reader.file.stream_position().await.unwrap(), size);
        reader.set_range(size - 3, 2).await.unwrap();
        assert_eq!(reader.next_chunk().await.unwrap().unwrap(), b"en"[..]);
        assert!(reader.next_chunk().await.unwrap().is_none());
        assert_eq!(reader.file.stream_position().await.unwrap(), size - 1);
        reader.set_range(size, 1).await.unwrap();
        assert_eq!(
            reader.next_chunk().await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        drop(reader);
    }

    #[test]
    fn object_list_seeks_past_large_prefixes_with_bounded_sql_work() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection.execute_batch("WITH RECURSIVE entries(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM entries WHERE n < 20000)
            INSERT INTO objects SELECT 'bucket', printf('a/%05d', n), n, 1, 'text/plain', 0, zeroblob(16), 0, 'STANDARD', 0, 0, printf('%032x', n) FROM entries;
            INSERT INTO objects VALUES('bucket', 'z', 20001, 1, 'text/plain', 0, zeroblob(16), 0, 'STANDARD', 0, 0, '00000000000000000000000000004e21');").unwrap();
        connection.execute_batch(METADATA_SCHEMA).unwrap();
        connection.execute_batch(VERSIONING_SCHEMA).unwrap();
        connection
            .execute_batch(super::super::derived::SCHEMA)
            .unwrap();
        let counter = AtomicUsize::new(0);
        // Interrupt after 10,000 VM instructions so a row scan cannot masquerade as a seek.
        // SAFETY: This live connection invokes the callback synchronously with the live counter.
        #[allow(unsafe_code)]
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                connection.handle(),
                100,
                Some(count_progress),
                std::ptr::from_ref(&counter).cast_mut().cast(),
            );
        }
        let mut query = ObjectListQuery {
            bucket: "bucket".into(),
            delimiter: "/".into(),
            ..Default::default()
        };
        let page = list_page(&connection, &query, 1).unwrap();
        assert_eq!(page.prefixes, ["a/"]);
        assert!(page.items.is_empty());
        query.cursor = page.next;
        let page = list_page(&connection, &query, 1).unwrap();
        assert_eq!(page.items[0].name, "z");
        assert!(page.next.is_none());
        query.cursor = None;
        query.versions = true;
        let page = list_page(&connection, &query, 1).unwrap();
        assert_eq!(page.prefixes, ["a/"]);
        query.cursor = page.next;
        let page = list_page(&connection, &query, 1).unwrap();
        assert_eq!(page.items[0].name, "z");
        assert!(page.next.is_none());
        // SAFETY: Unregister on the live connection before the callback's counter is dropped.
        #[allow(unsafe_code)]
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                connection.handle(),
                0,
                None,
                std::ptr::null_mut(),
            );
        }
        assert!(counter.load(Ordering::Relaxed) < 100);
    }

    #[test]
    fn object_list_skips_noncurrent_rows_with_bounded_sql_work() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection.execute_batch(METADATA_SCHEMA).unwrap();
        connection.execute_batch(VERSIONING_SCHEMA).unwrap();
        connection
            .execute_batch(super::super::derived::SCHEMA)
            .unwrap();
        connection.execute_batch("WITH RECURSIVE entries(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM entries WHERE n < 20000)
            INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,time_deleted)
            SELECT 'bucket', CASE WHEN n <= 10000 THEN 'a/history' ELSE printf('y/%05d',n) END, n,1,'text/plain',0,zeroblob(16),0,'STANDARD',0,0,printf('%032x',n),1 FROM entries;
            INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id)
            VALUES ('bucket','c',20001,1,'text/plain',0,zeroblob(16),0,'STANDARD',0,0,printf('%032x',20001)),
            ('bucket','d/file',20002,1,'text/plain',0,zeroblob(16),0,'STANDARD',0,0,printf('%032x',20002)),
            ('bucket','z',20003,1,'text/plain',0,zeroblob(16),0,'STANDARD',0,0,printf('%032x',20003));").unwrap();
        for versions in [false, true] {
            for after in [false, true] {
                for bounded in [false, true] {
                    let sql = format!(
                        "EXPLAIN QUERY PLAN {}",
                        list_seek_sql(versions, after, bounded)
                    );
                    let mut statement = connection.prepare(&sql).unwrap();
                    let read = |row: &Row<'_>| row.get::<_, String>(3);
                    let plan: Vec<String> = if bounded {
                        statement
                            .query_map(params!["bucket", "", "zz"], read)
                            .unwrap()
                            .collect::<Result<_, _>>()
                            .unwrap()
                    } else {
                        statement
                            .query_map(params!["bucket", ""], read)
                            .unwrap()
                            .collect::<Result<_, _>>()
                            .unwrap()
                    };
                    assert!(
                        plan.iter().any(|step| step.starts_with("SEARCH objects")),
                        "{plan:?}"
                    );
                    assert!(
                        plan.iter().all(|step| !step.contains("TEMP B-TREE")),
                        "{plan:?}"
                    );
                    if !versions {
                        assert!(
                            plan.iter()
                                .any(|step| step.contains("USING INDEX objects_live")),
                            "{plan:?}"
                        );
                    }
                }
            }
        }
        let counter = AtomicUsize::new(0);
        // One budget covers multiple pages and history-only prefixes, independent of history size.
        // SAFETY: This live connection invokes the callback synchronously with the live counter.
        #[allow(unsafe_code)]
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                connection.handle(),
                100,
                Some(count_progress),
                std::ptr::from_ref(&counter).cast_mut().cast(),
            );
        }
        for delimiter in ["", "/"] {
            let mut query = ObjectListQuery {
                bucket: "bucket".into(),
                delimiter: delimiter.into(),
                ..Default::default()
            };
            for expected in ["c", if delimiter.is_empty() { "d/file" } else { "d/" }, "z"] {
                let page = list_page(&connection, &query, 1).unwrap();
                let names: Vec<_> = page
                    .items
                    .iter()
                    .map(|record| record.name.as_str())
                    .chain(page.prefixes.iter().map(String::as_str))
                    .collect();
                assert_eq!(names, [expected]);
                query.cursor = page.next;
            }
            assert!(query.cursor.is_none());
        }
        let mut query = ObjectListQuery {
            bucket: "bucket".into(),
            delimiter: "/".into(),
            versions: true,
            ..Default::default()
        };
        for expected in ["a/", "c", "d/", "y/", "z"] {
            let page = list_page(&connection, &query, 1).unwrap();
            let names: Vec<_> = page
                .items
                .iter()
                .map(|record| record.name.as_str())
                .chain(page.prefixes.iter().map(String::as_str))
                .collect();
            assert_eq!(names, [expected]);
            query.cursor = page.next;
        }
        query.delimiter.clear();
        query.prefix = "a/".into();
        query.cursor = Some(ObjectListCursor {
            name: "a/history".into(),
            is_prefix: false,
            generation: Some(9999),
        });
        let page = list_page(&connection, &query, 1).unwrap();
        assert_eq!(page.items[0].generation, 10000);
        assert!(page.next.is_none());
        // SAFETY: Unregister on the live connection before the callback's counter is dropped.
        #[allow(unsafe_code)]
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                connection.handle(),
                0,
                None,
                std::ptr::null_mut(),
            );
        }
        assert!(counter.load(Ordering::Relaxed) < 100);
    }

    #[test]
    fn metadata_migration_rolls_back_all_columns_and_version_after_late_failure() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection.execute_batch(GENERATION_SCHEMA).unwrap();
        connection.execute_batch("INSERT INTO object_generation VALUES (1,7);
            INSERT INTO objects VALUES ('bucket','object',7,1,'text/plain',3,zeroblob(16),0,'STANDARD',0,0,'a0000000000000000000000000000000');
            ALTER TABLE objects ADD COLUMN metadata TEXT;
            PRAGMA user_version = 3;").unwrap();
        let before: String = connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name='objects'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // The final ALTER fails after four successful statements, exercising transaction rollback.
        assert!(migrate_metadata(&mut connection).is_err());
        let after: String = connection
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE name='objects'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, before);
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            3
        );
        let data: (i64, String, i64) = connection
            .query_row(
                "SELECT generation,content_type,size FROM objects",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(data, (7, "text/plain".into(), 3));
    }

    #[tokio::test]
    async fn prepared_upload_skips_advisory_repeat_but_rechecks_publication() {
        let directory = std::env::temp_dir().join(format!(
            "gcs-mimic-prechecked-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let catalog = Catalog::open(&directory).await.unwrap();
        for checksum_mismatch in [true, false] {
            catalog
                .insert_bucket(crate::bucket::BucketRecord {
                    name: "bucket".into(),
                    project: "project".into(),
                    location: "US".into(),
                    location_type: "multi-region".into(),
                    storage_class: "STANDARD".into(),
                    labels: Default::default(),
                    versioning_enabled: false,
                    default_event_based_hold: false,
                    public_access_prevention_enforced: false,
                    cors: Vec::new(),
                    metageneration: 1,
                    time_created: UNIX_EPOCH,
                    updated: UNIX_EPOCH,
                })
                .await
                .unwrap();
            let conditions = ObjectConditions::default();
            let permit = catalog
                .prepare_object_upload("bucket", "object", conditions, &BucketAccess::Permissive)
                .await
                .unwrap();
            catalog.delete_bucket("bucket", None, None).await.unwrap();
            let (sender, body) = mpsc::channel(2);
            sender
                .send(Ok(UploadChunk::Data(Bytes::from_static(b"new"))))
                .await
                .unwrap();
            sender.send(Ok(UploadChunk::End)).await.unwrap();
            drop(sender);
            let expected = ExpectedHashes {
                md5: checksum_mismatch.then_some([0; 16]),
                crc32c: None,
            };
            let error = catalog
                .put_object_prechecked(
                    "bucket",
                    "object",
                    "text/plain".into(),
                    expected,
                    conditions,
                    1000,
                    body,
                    permit,
                    ObjectMetadataUpdate::default(),
                    BucketAccess::Permissive,
                )
                .await
                .unwrap_err();
            assert_eq!(
                error,
                if checksum_mismatch {
                    ObjectError::ChecksumMismatch
                } else {
                    ObjectError::BucketNotFound
                }
            );
            assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
        }
        catalog.close().await.unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn forbidden_overwrite_is_rechecked_atomically_after_a_concurrent_create() {
        let directory = std::env::temp_dir().join(format!(
            "gcs-mimic-no-overwrite-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let catalog = Catalog::open(&directory).await.unwrap();
        catalog
            .insert_bucket(crate::bucket::BucketRecord {
                name: "bucket".into(),
                project: "project".into(),
                location: "US".into(),
                location_type: "multi-region".into(),
                storage_class: "STANDARD".into(),
                labels: Default::default(),
                versioning_enabled: false,
                default_event_based_hold: false,
                public_access_prevention_enforced: false,
                cors: Vec::new(),
                metageneration: 1,
                time_created: UNIX_EPOCH,
                updated: UNIX_EPOCH,
            })
            .await
            .unwrap();
        let upload = |allow_overwrite, bytes: &'static [u8]| {
            let catalog = catalog.clone();
            async move {
                let permit = catalog
                    .prepare_object_upload(
                        "bucket",
                        "object",
                        ObjectConditions::default(),
                        &project_access(allow_overwrite),
                    )
                    .await?;
                let (sender, body) = mpsc::channel(2);
                sender
                    .send(Ok(UploadChunk::Data(Bytes::from_static(bytes))))
                    .await
                    .unwrap();
                sender.send(Ok(UploadChunk::End)).await.unwrap();
                Ok::<_, ObjectError>((permit, body))
            }
        };
        // The advisory check passes while the object is absent; another writer then creates it.
        let (permit, body) = upload(false, b"late").await.unwrap();
        let (winner_permit, winner_body) = upload(true, b"winner").await.unwrap();
        let publish = |body, permit, allow_overwrite| {
            catalog.put_object_prechecked(
                "bucket",
                "object",
                "text/plain".into(),
                ExpectedHashes::default(),
                ObjectConditions::default(),
                1000,
                body,
                permit,
                ObjectMetadataUpdate::default(),
                project_access(allow_overwrite),
            )
        };
        let winner = publish(winner_body, winner_permit, true).await.unwrap();
        assert_eq!(
            publish(body, permit, false).await.unwrap_err(),
            ObjectError::OverwriteForbidden
        );
        let current = catalog
            .get_object("bucket", "object", None, Default::default())
            .await
            .unwrap();
        assert_eq!(current, winner);
        assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
        // A failing precondition takes precedence: nothing would have been replaced.
        let conditions = ObjectConditions {
            if_generation_match: Some(0),
            ..Default::default()
        };
        let mismatch = ObjectConditions {
            if_generation_match: Some(winner.generation + 1),
            ..Default::default()
        };
        for (conditions, error) in [
            (conditions, ObjectError::PreconditionFailed),
            (mismatch, ObjectError::PreconditionFailed),
            (Default::default(), ObjectError::OverwriteForbidden),
        ] {
            assert_eq!(
                catalog
                    .check_upload("bucket", "object", conditions, &project_access(false))
                    .await
                    .unwrap_err(),
                error
            );
        }
        assert!(
            catalog
                .check_upload(
                    "bucket",
                    "fresh",
                    Default::default(),
                    &project_access(false)
                )
                .await
                .is_ok()
        );
        catalog.close().await.unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    fn project_access(overwrite: bool) -> BucketAccess {
        policy_access("project", Some("project"), overwrite)
    }

    fn policy_access(project: &str, observed: Option<&str>, overwrite: bool) -> BucketAccess {
        use crate::auth::{Auth, Registry};
        let roles = if overwrite {
            vec!["roles/storage.admin"]
        } else {
            vec!["roles/storage.objectViewer", "roles/storage.objectCreator"]
        };
        let auth = Auth::strict(
            Registry::parse(
                serde_json::json!({"identities":[{
                    "id":"local", "tokens":["token"], "project_roles":{project:roles}
                }]})
                .to_string()
                .as_bytes(),
            )
            .unwrap(),
        );
        let mut headers = hyper::HeaderMap::new();
        headers.insert("authorization", "Bearer token".parse().unwrap());
        let identity = auth
            .authenticate(&headers, None)
            .unwrap()
            .unwrap()
            .principal()
            .unwrap();
        BucketAccess::Policy {
            identity: Some(identity.clone()),
            permission: crate::auth::Permission::ObjectsCreate,
            project: observed.map(str::to_owned),
        }
    }

    fn temp_catalog_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gcs-mimic-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    async fn create_bucket(catalog: &Catalog, project: &str) {
        catalog
            .insert_bucket(crate::bucket::BucketRecord {
                name: "bucket".into(),
                project: project.into(),
                location: "US".into(),
                location_type: "multi-region".into(),
                storage_class: "STANDARD".into(),
                labels: Default::default(),
                versioning_enabled: false,
                default_event_based_hold: false,
                public_access_prevention_enforced: false,
                cors: Vec::new(),
                metageneration: 1,
                time_created: UNIX_EPOCH,
                updated: UNIX_EPOCH,
            })
            .await
            .unwrap();
    }

    fn body(bytes: &'static [u8]) -> mpsc::Receiver<Result<UploadChunk, io::Error>> {
        let (sender, body) = mpsc::channel(2);
        sender
            .try_send(Ok(UploadChunk::Data(Bytes::from_static(bytes))))
            .unwrap();
        sender.try_send(Ok(UploadChunk::End)).unwrap();
        body
    }

    async fn put(catalog: &Catalog, name: &str, bytes: &'static [u8]) -> ObjectRecord {
        catalog
            .put_object(
                "bucket",
                name,
                "text/plain".into(),
                ExpectedHashes::default(),
                ObjectConditions::default(),
                1000,
                body(bytes),
            )
            .await
            .unwrap()
    }

    async fn publish(
        catalog: &Catalog,
        name: &str,
        permit: OwnedSemaphorePermit,
        access: BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        catalog
            .put_object_prechecked(
                "bucket",
                name,
                "text/plain".into(),
                ExpectedHashes::default(),
                ObjectConditions::default(),
                1000,
                body(b"late"),
                permit,
                ObjectMetadataUpdate::default(),
                access,
            )
            .await
    }

    /// Recreate "bucket" under `project` holding one object named "object".
    async fn recreate_bucket(catalog: &Catalog, project: &str) -> ObjectRecord {
        for name in ["object", "fresh"] {
            let _ = catalog
                .delete_object("bucket", name, None, Default::default())
                .await;
        }
        let _ = catalog.delete_bucket("bucket", None, None).await;
        create_bucket(catalog, project).await;
        put(catalog, "object", b"other project").await
    }

    /// Every bucket and object operation must fail with `Forbidden` and change nothing.
    async fn assert_all_forbidden(
        catalog: &Catalog,
        access: &BucketAccess,
        current: &ObjectRecord,
    ) {
        let conditions = ObjectConditions::default();
        assert_eq!(
            catalog
                .check_upload("bucket", "object", conditions, access)
                .await,
            Err(ObjectError::Forbidden)
        );
        assert_eq!(
            catalog
                .get_object_authorized("bucket", "object", None, conditions, access)
                .await,
            Err(ObjectError::Forbidden)
        );
        assert_eq!(
            catalog
                .open_object_authorized("bucket", "object", None, conditions, access)
                .await
                .map(|(record, _)| record),
            Err(ObjectError::Forbidden)
        );
        let query = ObjectListQuery {
            bucket: "bucket".into(),
            ..Default::default()
        };
        assert_eq!(
            catalog
                .list_objects_authorized(query, 10, access)
                .await
                .map(|page| page.items),
            Err(ObjectError::Forbidden)
        );
        let update = ObjectMetadataUpdate {
            content_type: Some(Some("application/json".into())),
            ..Default::default()
        };
        assert_eq!(
            catalog
                .update_object_metadata_authorized(
                    "bucket",
                    "object",
                    None,
                    conditions,
                    update,
                    |_| Ok(()),
                    access,
                )
                .await,
            Err(ObjectError::Forbidden)
        );
        assert_eq!(
            catalog
                .delete_object_authorized("bucket", "object", None, conditions, access)
                .await,
            Err(ObjectError::Forbidden)
        );
        assert_eq!(
            catalog.get_bucket_authorized("bucket", access).await,
            Err(CatalogError::Forbidden)
        );
        // Authorization precedes the non-empty check, so a non-empty bucket also reports 403.
        assert_eq!(
            catalog
                .delete_bucket_authorized("bucket", None, None, access)
                .await,
            Err(CatalogError::Forbidden)
        );
        assert_eq!(
            catalog
                .get_object("bucket", "object", None, conditions)
                .await
                .as_ref(),
            Ok(current)
        );
        assert!(catalog.get_bucket("bucket").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn pap_is_rechecked_after_upload_admission_and_before_reads() {
        use crate::auth::{Binding, OBJECTS_CREATE, OBJECTS_GET, Policy};
        for parent in [false, true] {
            let directory = temp_catalog_dir("pap-recheck");
            let catalog = Catalog::open(&directory).await.unwrap();
            create_bucket(&catalog, "project").await;
            let current = put(&catalog, "object", b"original").await;
            let policy = Policy {
                bindings: vec![Binding {
                    role: "roles/storage.admin".into(),
                    members: vec!["allUsers".into()],
                }],
                etag: None,
            };
            let stored = catalog
                .set_policy("bucket", policy, &BucketAccess::Permissive)
                .await
                .unwrap();
            let read = catalog
                .bucket_access("bucket", None, OBJECTS_GET)
                .await
                .unwrap();
            let create = catalog
                .bucket_access("bucket", None, OBJECTS_CREATE)
                .await
                .unwrap();
            let permit = catalog
                .prepare_object_upload("bucket", "fresh", Default::default(), &create)
                .await
                .unwrap();
            if parent {
                catalog
                    .configure_public_access_prevention(&["project".into(), "project".into()])
                    .await
                    .unwrap();
            } else {
                catalog
                    .patch_bucket(
                        "bucket",
                        crate::bucket::BucketMetadataUpdate {
                            public_access_prevention_enforced: Some(true),
                            ..Default::default()
                        },
                        None,
                        None,
                        &BucketAccess::Permissive,
                    )
                    .await
                    .unwrap();
            }
            assert_all_forbidden(&catalog, &read, &current).await;
            assert_eq!(
                publish(&catalog, "fresh", permit, create).await,
                Err(ObjectError::Forbidden)
            );
            assert_eq!(
                catalog
                    .get_policy("bucket", &BucketAccess::Permissive)
                    .await
                    .unwrap(),
                stored
            );
            assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
            catalog.close().await.unwrap();
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test]
    async fn revoked_bucket_binding_is_rechecked_inside_operations() {
        use crate::auth::{
            Auth, BUCKETS_GET_IAM, BUCKETS_SET_IAM, Binding, OBJECTS_CREATE, OBJECTS_GET, Policy,
            Registry,
        };
        for public in [false, true] {
            let directory = temp_catalog_dir("revoked-policy");
            let catalog = Catalog::open(&directory).await.unwrap();
            create_bucket(&catalog, "project").await;
            let current = put(&catalog, "object", b"original").await;
            let auth = Auth::strict(
                Registry::parse(br#"{"identities":[{"id":"local", "tokens":["token"]}]}"#).unwrap(),
            );
            let mut headers = hyper::HeaderMap::new();
            headers.insert("authorization", "Bearer token".parse().unwrap());
            let identity = auth
                .authenticate(&headers, None)
                .unwrap()
                .unwrap()
                .principal()
                .unwrap();
            let identity = if public { None } else { Some(identity) };
            let granted = Policy {
                bindings: vec![Binding {
                    role: "roles/storage.admin".into(),
                    members: vec![if public { "allUsers" } else { "user:local" }.into()],
                }],
                etag: None,
            };
            catalog
                .set_policy("bucket", granted, &BucketAccess::Permissive)
                .await
                .unwrap();
            let read = catalog
                .bucket_access("bucket", identity, OBJECTS_GET)
                .await
                .unwrap();
            let create = catalog
                .bucket_access("bucket", identity, OBJECTS_CREATE)
                .await
                .unwrap();
            let get_iam = catalog
                .bucket_access("bucket", identity, BUCKETS_GET_IAM)
                .await
                .unwrap();
            let set_iam = catalog
                .bucket_access("bucket", identity, BUCKETS_SET_IAM)
                .await
                .unwrap();
            let permit = catalog
                .prepare_object_upload("bucket", "fresh", Default::default(), &create)
                .await
                .unwrap();
            // These accesses passed advisory authorization; transaction rechecks must observe
            // revocation before selecting a blob or publishing an upload.
            catalog
                .set_policy(
                    "bucket",
                    Policy {
                        bindings: vec![],
                        etag: None,
                    },
                    &BucketAccess::Permissive,
                )
                .await
                .unwrap();
            assert_all_forbidden(&catalog, &read, &current).await;
            assert_eq!(
                catalog.get_policy("bucket", &get_iam).await,
                Err(CatalogError::Forbidden)
            );
            assert_eq!(
                catalog
                    .set_policy(
                        "bucket",
                        Policy {
                            bindings: vec![],
                            etag: None
                        },
                        &set_iam
                    )
                    .await,
                Err(CatalogError::Forbidden)
            );
            assert_eq!(
                publish(&catalog, "fresh", permit, create).await,
                Err(ObjectError::Forbidden)
            );
            assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
            catalog.close().await.unwrap();
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[tokio::test]
    async fn operations_recheck_the_authorized_project_after_bucket_recreation() {
        let directory = temp_catalog_dir("recreated-bucket");
        let catalog = Catalog::open(&directory).await.unwrap();
        create_bucket(&catalog, "project").await;
        let access = project_access(true);
        // The advisory check passes in the authorized project; the bucket is then deleted and
        // recreated under another project before the body completes.
        let permit = catalog
            .prepare_object_upload("bucket", "object", Default::default(), &access)
            .await
            .unwrap();
        let current = recreate_bucket(&catalog, "other-project").await;
        assert_eq!(
            publish(&catalog, "object", permit, access.clone()).await,
            Err(ObjectError::Forbidden)
        );
        let permit = catalog.admit_object_upload().unwrap();
        assert_eq!(
            publish(&catalog, "fresh", permit, access.clone()).await,
            Err(ObjectError::Forbidden)
        );
        assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
        assert_all_forbidden(&catalog, &access, &current).await;
        let page = catalog
            .list_objects(
                ObjectListQuery {
                    bucket: "bucket".into(),
                    ..Default::default()
                },
                10,
            )
            .await
            .unwrap();
        assert_eq!(page.items, vec![current.clone()]);
        // An access authorized against the bucket's actual project still succeeds.
        let actual = policy_access("other-project", Some("other-project"), false);
        assert_eq!(
            catalog
                .get_object_authorized("bucket", "object", None, Default::default(), &actual)
                .await,
            Ok(current)
        );
        catalog.close().await.unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn missing_bucket_access_reauthorizes_against_the_actual_project() {
        let directory = temp_catalog_dir("missing-bucket-access");
        let catalog = Catalog::open(&directory).await.unwrap();
        let BucketAccess::Policy {
            identity,
            permission,
            ..
        } = policy_access("project", None, false)
        else {
            unreachable!()
        };
        let access = catalog
            .bucket_access("bucket", identity.as_ref(), permission.as_str())
            .await
            .unwrap();
        let conditions = ObjectConditions::default();
        // Still missing: the caller holds the permission somewhere, so it learns 404.
        assert_eq!(
            catalog
                .check_upload("bucket", "object", conditions, &access)
                .await,
            Err(ObjectError::BucketNotFound)
        );
        assert_eq!(
            catalog
                .get_object_authorized("bucket", "object", None, conditions, &access)
                .await,
            Err(ObjectError::BucketNotFound)
        );
        assert_eq!(
            catalog
                .delete_object_authorized("bucket", "object", None, conditions, &access)
                .await,
            Err(ObjectError::BucketNotFound)
        );
        assert_eq!(
            catalog.get_bucket_authorized("bucket", &access).await,
            Ok(None)
        );
        assert_eq!(
            catalog
                .delete_bucket_authorized("bucket", None, None, &access)
                .await,
            Err(CatalogError::NotFound)
        );
        // Created later in a project where the caller lacks the permission.
        create_bucket(&catalog, "project").await;
        let permit = catalog
            .prepare_object_upload("bucket", "object", conditions, &access)
            .await
            .unwrap();
        let current = recreate_bucket(&catalog, "other-project").await;
        assert_eq!(
            publish(&catalog, "object", permit, access.clone()).await,
            Err(ObjectError::Forbidden)
        );
        assert_all_forbidden(&catalog, &access, &current).await;
        // Created later in a permitted project: overwriting still needs storage.objects.delete
        // there and never defaults to allowed.
        recreate_bucket(&catalog, "other-project").await;
        let current = recreate_bucket(&catalog, "project").await;
        let permit = catalog.admit_object_upload().unwrap();
        assert_eq!(
            publish(&catalog, "object", permit, access.clone()).await,
            Err(ObjectError::OverwriteForbidden)
        );
        assert_eq!(
            catalog
                .get_object_authorized("bucket", "object", None, conditions, &access)
                .await,
            Ok(current)
        );
        let permit = catalog.admit_object_upload().unwrap();
        assert!(publish(&catalog, "fresh", permit, access).await.is_ok());
        let deleter = policy_access("project", None, true);
        let permit = catalog.admit_object_upload().unwrap();
        assert!(publish(&catalog, "object", permit, deleter).await.is_ok());
        assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_v1_migration_never_commits_an_unreopenable_v2_catalog() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(super::super::BUCKET_SCHEMA)
            .unwrap();
        // Force the second DDL to fail after the object tables and v2 marker were written.
        connection.execute_batch(GENERATION_SCHEMA).unwrap();
        assert!(migrate(&mut connection).is_err());
        assert_eq!(
            connection
                .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        let objects: i64 = connection.query_row("SELECT count(*) FROM sqlite_schema WHERE name IN ('objects', 'pending_blob_deletions')", [], |row| row.get(0)).unwrap();
        assert_eq!(objects, 0);
    }
}
