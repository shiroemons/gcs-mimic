use super::{Catalog, MAX_UPLOADS, system_time, unavailable, unix_millis};
use crate::{
    bucket::CatalogError,
    object::{
        ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate, ObjectRecord,
        UploadChunk,
    },
};
use bytes::Bytes;
use md5::{Digest, Md5};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params, types::Type};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{OwnedSemaphorePermit, mpsc, oneshot},
};

const CHUNK_SIZE: usize = 64 * 1024;
const OBJECT_COLUMNS: &str = "bucket, name, generation, metageneration, content_type, size,
    md5, crc32c, storage_class, time_created, updated, blob_id,
    content_encoding, content_disposition, content_language, cache_control, metadata";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectListCursor {
    pub name: String,
    pub is_prefix: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectListQuery {
    pub bucket: String,
    pub prefix: String,
    pub delimiter: String,
    pub start_offset: String,
    pub end_offset: String,
    pub include_trailing_delimiter: bool,
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

fn blob_path(data_dir: &Path, id: &str) -> Result<PathBuf, ObjectError> {
    if id.len() != 32
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ObjectError::Unavailable("invalid opaque blob ID".into()));
    }
    Ok(data_dir.join("blobs").join(&id[..2]).join(id))
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn publish_blob(source: &Path, destination: &Path) -> io::Result<()> {
    let shard = destination.parent().expect("blob parent");
    fs::create_dir_all(shard)?;
    #[cfg(unix)]
    {
        sync_dir(shard.parent().expect("blobs directory"))?;
        fs::rename(source, destination)?;
        sync_dir(shard)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

        // Preserve extended paths and keep publication on the same volume. Windows
        // uses write-through publication, not Unix directory-fsync durability.
        let source = source.canonicalize()?;
        let destination = shard
            .canonicalize()?
            .join(destination.file_name().expect("blob filename"));
        let wide = |path: &Path| -> io::Result<Vec<u16>> {
            let mut path: Vec<_> = path.as_os_str().encode_wide().collect();
            if path.contains(&0) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "NUL in blob path",
                ));
            }
            path.push(0);
            Ok(path)
        };
        let source = wide(&source)?;
        let destination = wide(&destination)?;
        // Both terminated UTF-16 buffers remain alive for this synchronous call.
        let moved = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

fn unlink(path: &Path) -> io::Result<()> {
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

fn reclaim(connection: &Connection, data_dir: &Path, id: &str) -> Result<(), ObjectError> {
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

pub(super) fn recover(connection: &Connection, data_dir: &Path) -> Result<(), CatalogError> {
    let recover = || -> Result<(), ObjectError> {
        fs::create_dir_all(data_dir.join("staging")).map_err(object_unavailable)?;
        fs::create_dir_all(data_dir.join("blobs")).map_err(object_unavailable)?;
        #[cfg(unix)]
        sync_dir(data_dir).map_err(object_unavailable)?;
        for entry in fs::read_dir(data_dir.join("staging")).map_err(object_unavailable)? {
            let entry = entry.map_err(object_unavailable)?;
            if os_metadata(&entry) {
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
}

impl ObjectReader {
    pub async fn next_chunk(&mut self) -> io::Result<Option<Bytes>> {
        let mut buffer = vec![0; CHUNK_SIZE];
        let length = self.file.read(&mut buffer).await?;
        buffer.truncate(length);
        Ok((length != 0).then(|| Bytes::from(buffer)))
    }
}

impl Catalog {
    /// Each emitted entry requires one indexed seek, independent of a grouped prefix's size.
    pub async fn list_objects(
        &self,
        query: ObjectListQuery,
        limit: usize,
    ) -> Result<ObjectListPage, ObjectError> {
        if !(1..=1000).contains(&limit) {
            return Err(object_unavailable(
                "object list limit must be between 1 and 1000",
            ));
        }
        self.run_object(move |connection| {
            bucket_class(connection, &query.bucket)?;
            list_page(connection, &query, limit)
        })
        .await
    }

    async fn run_upload<T: Send + 'static>(
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
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let _execution = execution;
            let mut resources = inner.resources.lock().map_err(object_unavailable)?;
            let resources = resources
                .as_mut()
                .ok_or_else(|| object_unavailable("catalog is closed"))?;
            operation(&mut resources.connection)
        })
        .await
        .map_err(object_unavailable)?
    }

    async fn run_object<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T, ObjectError> + Send + 'static,
    ) -> Result<T, ObjectError> {
        self.run(move |connection| Ok(operation(connection)))
            .await
            .map_err(object_unavailable)?
    }

    /// Advisory only: publication rechecks inside its transaction after the body completes.
    pub async fn check_object_upload(
        &self,
        bucket: &str,
        name: &str,
        conditions: ObjectConditions,
    ) -> Result<(), ObjectError> {
        self.check_upload(bucket, name, conditions, true).await
    }

    /// Advisory check that also rejects an overwrite the caller is not allowed to perform.
    pub(crate) async fn check_upload(
        &self,
        bucket: &str,
        name: &str,
        conditions: ObjectConditions,
        allow_overwrite: bool,
    ) -> Result<(), ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        self.run_object(move |connection| {
            bucket_class(connection, &bucket)?;
            let previous = lookup(connection, &bucket, &name)?;
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
    pub async fn put_object(
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
            .prepare_object_upload(bucket, name, conditions, true)
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
            true,
        )
        .await
    }

    pub(crate) async fn prepare_object_upload(
        &self,
        bucket: &str,
        name: &str,
        conditions: ObjectConditions,
        allow_overwrite: bool,
    ) -> Result<OwnedSemaphorePermit, ObjectError> {
        let permit = self.admit_object_upload()?;
        self.check_upload(bucket, name, conditions, allow_overwrite)
            .await?;
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
        // False when the caller lacks permission to delete a live object it would replace.
        allow_overwrite: bool,
    ) -> Result<ObjectRecord, ObjectError> {
        let catalog = self.clone();
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let (_request, cancellation) = oneshot::channel::<()>();
        // Cancellation must not release directory ownership during publication or cleanup.
        tokio::spawn(async move {
            let _permit = permit;
            let mut random = [0; 16];
            getrandom::fill(&mut random).map_err(object_unavailable)?;
            let id: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
            let stage = catalog.0.data_dir.join("staging").join(&id);
            let published = blob_path(&catalog.0.data_dir, &id)?;
            let source = stage.clone();
            let destination = published.clone();
            let file = tokio::task::spawn_blocking(move || -> io::Result<File> {
                if destination.try_exists()? {
                    return Err(io::Error::new(io::ErrorKind::AlreadyExists, "blob ID collision"));
                }
                fs::OpenOptions::new().write(true).create_new(true).open(source)
            }).await.map_err(object_unavailable)?.map_err(object_unavailable)?;
            let result = async {
                let (size, md5, crc32c) = write_body(tokio::fs::File::from_std(file), expected, max_bytes, body, cancellation).await?;
                let source = stage.clone();
                let destination = published.clone();
                tokio::task::spawn_blocking(move || publish_blob(&source, &destination))
                    .await.map_err(object_unavailable)?.map_err(object_unavailable)?;
                let data_dir = catalog.0.data_dir.clone();
                let clock = catalog.0.clock.clone();
                let blob_id = id.clone();
                catalog.run_upload(move |connection| {
                    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)
                        .map_err(object_unavailable)?;
                    let storage_class = bucket_class(&transaction, &bucket)?;
                    let previous = lookup(&transaction, &bucket, &name)?;
                    // Rechecked under the write lock so a concurrent create cannot be overwritten.
                    check_publication(previous.as_ref(), conditions, allow_overwrite)?;
                    let now = clock.now();
                    let micros = i64::try_from(now.duration_since(UNIX_EPOCH)
                        .map_err(object_unavailable)?.as_micros()).map_err(object_unavailable)?;
                    let last: i64 = transaction.query_row("SELECT last_generation FROM object_generation WHERE singleton = 1", [], |row| row.get(0)).map_err(object_unavailable)?;
                    let generation = micros.max(last.checked_add(1).ok_or_else(|| object_unavailable("generation exhausted"))?);
                    transaction.execute("UPDATE object_generation SET last_generation = ?1 WHERE singleton = 1", [generation]).map_err(object_unavailable)?;
                    let millis = unix_millis(now).map_err(object_unavailable)?;
                    let time = system_time(millis).map_err(object_unavailable)?;
                    let mut record = ObjectRecord {
                        bucket, name, generation, metageneration: 1,
                        content_type, size, md5, crc32c, storage_class,
                        content_encoding: None, content_disposition: None,
                        content_language: None, cache_control: None, metadata: Default::default(),
                        time_created: time, updated: time,
                    };
                    metadata.apply(&mut record)?;
                    let custom_metadata = serde_json::to_string(&record.metadata).map_err(object_unavailable)?;
                    transaction.execute(
                        "INSERT OR REPLACE INTO objects (bucket, name, generation, metageneration, content_type, size, md5, crc32c, storage_class, time_created, updated, blob_id, content_encoding, content_disposition, content_language, cache_control, metadata) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                        params![record.bucket, record.name, record.generation, record.metageneration,
                            record.content_type, i64::try_from(size).map_err(object_unavailable)?,
                            &record.md5[..], record.crc32c, record.storage_class, millis, millis, blob_id,
                            record.content_encoding, record.content_disposition, record.content_language,
                            record.cache_control, custom_metadata],
                    ).map_err(object_unavailable)?;
                    if let Some((_, old_id)) = &previous {
                        transaction.execute("INSERT INTO pending_blob_deletions VALUES (?1)", [old_id])
                            .map_err(object_unavailable)?;
                    }
                    transaction.commit().map_err(object_unavailable)?;
                    // Once committed, failure to reclaim must never remove the new live blob.
                    if let Some((_, old_id)) = previous
                        && let Err(error) = reclaim(connection, &data_dir, &old_id) {
                        eprintln!("deferred blob reclaim {old_id}: {error}");
                    }
                    Ok(record)
                }).await
            }.await;
            if result.is_err() {
                let data_dir = catalog.0.data_dir.clone();
                // Probe the committed reference before cleanup: SQL commit errors can be ambiguous.
                catalog.run_upload(move |connection| {
                    unlink(&stage).map_err(object_unavailable)?;
                    let referenced: bool = connection.query_row(
                        "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)",
                        [&id], |r| r.get(0),
                    ).map_err(object_unavailable)?;
                    if !referenced {
                        reclaim(connection, &data_dir, &id)?;
                    }
                    Ok(())
                }).await?;
            }
            result
        }).await.map_err(object_unavailable)?
    }

    pub async fn get_object(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
    ) -> Result<ObjectRecord, ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        self.run_object(move |connection| {
            selected(connection, &bucket, &name, generation, conditions, true)
                .map(|(record, _)| record)
        })
        .await
    }

    pub async fn open_object(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
    ) -> Result<(ObjectRecord, ObjectReader), ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let data_dir = self.0.data_dir.clone();
        self.run_object(move |connection| {
            let (record, id) = selected(connection, &bucket, &name, generation, conditions, true)?;
            // Open under the same execution permit that serializes deletes and overwrites.
            let file = File::open(blob_path(&data_dir, &id)?).map_err(object_unavailable)?;
            Ok((
                record,
                ObjectReader {
                    file: tokio::fs::File::from_std(file),
                },
            ))
        })
        .await
    }

    pub async fn update_object_metadata(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        update: ObjectMetadataUpdate,
        validate: impl FnOnce(&ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<ObjectRecord, ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let clock = self.0.clock.clone();
        self.run_object(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(object_unavailable)?;
            let (mut record, _) =
                selected(&transaction, &bucket, &name, generation, conditions, false)?;
            validate(&record)?;
            update.apply(&mut record)?;
            record.metageneration = record
                .metageneration
                .checked_add(1)
                .ok_or_else(|| object_unavailable("metageneration exhausted"))?;
            let millis = unix_millis(clock.now()).map_err(object_unavailable)?;
            record.updated = system_time(millis).map_err(object_unavailable)?;
            let metadata = serde_json::to_string(&record.metadata).map_err(object_unavailable)?;
            transaction
                .execute(
                    "UPDATE objects SET metageneration = ?3, updated = ?4, content_type = ?5,
                    content_encoding = ?6, content_disposition = ?7, content_language = ?8,
                    cache_control = ?9, metadata = ?10 WHERE bucket = ?1 AND name = ?2",
                    params![
                        bucket,
                        name,
                        record.metageneration,
                        millis,
                        record.content_type,
                        record.content_encoding,
                        record.content_disposition,
                        record.content_language,
                        record.cache_control,
                        metadata
                    ],
                )
                .map_err(object_unavailable)?;
            transaction.commit().map_err(object_unavailable)?;
            Ok(record)
        })
        .await
    }

    pub async fn delete_object(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
    ) -> Result<(), ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let data_dir = self.0.data_dir.clone();
        self.run_object(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(object_unavailable)?;
            let (_, id) = selected(&transaction, &bucket, &name, generation, conditions, false)?;
            transaction
                .execute(
                    "DELETE FROM objects WHERE bucket = ?1 AND name = ?2",
                    params![bucket, name],
                )
                .map_err(object_unavailable)?;
            transaction
                .execute("INSERT INTO pending_blob_deletions VALUES (?1)", [&id])
                .map_err(object_unavailable)?;
            transaction.commit().map_err(object_unavailable)?;
            if let Err(error) = reclaim(connection, &data_dir, &id) {
                eprintln!("deferred blob reclaim {id}: {error}");
            }
            Ok(())
        })
        .await
    }
}

async fn write_body(
    mut file: tokio::fs::File,
    mut expected: ExpectedHashes,
    max_bytes: u64,
    mut body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
    mut cancellation: oneshot::Receiver<()>,
) -> Result<(u64, [u8; 16], u32), ObjectError> {
    let mut size = 0_u64;
    let mut md5 = Md5::new();
    let mut crc32c = 0;
    loop {
        let chunk = tokio::select! {
            biased;
            _ = &mut cancellation => return Err(ObjectError::BodyAborted("upload cancelled".into())),
            chunk = body.recv() => chunk,
        };
        let chunk = chunk
            .ok_or_else(|| ObjectError::BodyAborted("missing body EOF".into()))?
            .map_err(|error| ObjectError::BodyAborted(error.to_string()))?;
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
            md5.update(part);
            crc32c = crc32c::crc32c_append(crc32c, part);
            file.write_all(part).await.map_err(object_unavailable)?;
        }
    }
    let md5: [u8; 16] = md5.finalize().into();
    if expected.md5.is_some_and(|value| value != md5)
        || expected.crc32c.is_some_and(|value| value != crc32c)
    {
        return Err(ObjectError::ChecksumMismatch);
    }
    file.sync_all().await.map_err(object_unavailable)?;
    Ok((size, md5, crc32c))
}

fn object_unavailable(error: impl std::fmt::Display) -> ObjectError {
    ObjectError::Unavailable(error.to_string())
}

fn prefix_end(prefix: &str) -> Option<Vec<u8>> {
    let mut end = prefix.as_bytes().to_vec();
    let index = end.iter().rposition(|byte| *byte != u8::MAX)?;
    end[index] += 1;
    end.truncate(index + 1);
    Some(end)
}

fn list_page(
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
    let mut inclusive = connection.prepare(&format!(
        "SELECT {OBJECT_COLUMNS} FROM objects WHERE bucket = ?1 AND name >= CAST(?2 AS TEXT) {upper_clause} ORDER BY name LIMIT 1"
    )).map_err(object_unavailable)?;
    let mut exclusive = connection.prepare(&format!(
        "SELECT {OBJECT_COLUMNS} FROM objects WHERE bucket = ?1 AND name > CAST(?2 AS TEXT) {upper_clause} ORDER BY name LIMIT 1"
    )).map_err(object_unavailable)?;
    loop {
        let mut after = false;
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
            }
        }
        if upper
            .as_deref()
            .is_some_and(|upper| lower.as_slice() >= upper)
        {
            break;
        }
        let statement = if after {
            &mut exclusive
        } else {
            &mut inclusive
        };
        let record = match &upper {
            Some(upper) => statement.query_row(params![query.bucket, lower, upper], read_object),
            None => statement.query_row(params![query.bucket, lower], read_object),
        }
        .optional()
        .map_err(object_unavailable)?;
        let Some((record, _)) = record else { break };
        let group = group_prefix(&record.name, query);
        let is_prefix = group.is_some_and(|_| {
            !trailing_marker(&record.name, query)
                || cursor
                    .as_ref()
                    .is_some_and(|cursor| !cursor.is_prefix && cursor.name == record.name)
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

fn bucket_class(connection: &Connection, bucket: &str) -> Result<String, ObjectError> {
    connection
        .query_row(
            "SELECT storage_class FROM buckets WHERE name = ?1",
            [bucket],
            |r| r.get(0),
        )
        .optional()
        .map_err(object_unavailable)?
        .ok_or(ObjectError::BucketNotFound)
}

fn lookup(
    connection: &Connection,
    bucket: &str,
    name: &str,
) -> Result<Option<(ObjectRecord, String)>, ObjectError> {
    connection
        .query_row(
            &format!("SELECT {OBJECT_COLUMNS} FROM objects WHERE bucket = ?1 AND name = ?2"),
            params![bucket, name],
            read_object,
        )
        .optional()
        .map_err(object_unavailable)
}

fn selected(
    connection: &Connection,
    bucket: &str,
    name: &str,
    generation: Option<i64>,
    conditions: ObjectConditions,
    read: bool,
) -> Result<(ObjectRecord, String), ObjectError> {
    bucket_class(connection, bucket)?;
    let (record, id) = lookup(connection, bucket, name)?.ok_or(ObjectError::ObjectNotFound)?;
    if generation.is_some_and(|generation| generation != record.generation) {
        return Err(ObjectError::ObjectNotFound);
    }
    check_conditions(Some(&record), conditions, read)?;
    Ok((record, id))
}

/// Preconditions are evaluated first: a failing precondition means nothing would be replaced.
fn check_publication(
    previous: Option<&(ObjectRecord, String)>,
    conditions: ObjectConditions,
    allow_overwrite: bool,
) -> Result<(), ObjectError> {
    check_conditions(previous.map(|(record, _)| record), conditions, false)?;
    if previous.is_some() && !allow_overwrite {
        return Err(ObjectError::OverwriteForbidden);
    }
    Ok(())
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
            storage_class: row.get(8)?,
            time_created: system_time(row.get(9)?)?,
            updated: system_time(row.get(10)?)?,
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

    #[test]
    fn object_list_seeks_past_large_prefixes_with_bounded_sql_work() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        unsafe extern "C" fn progress(counter: *mut std::ffi::c_void) -> std::ffi::c_int {
            // The counter outlives the connection's registered callback on this thread.
            let counter = unsafe { &*counter.cast::<AtomicUsize>() };
            i32::from(counter.fetch_add(1, Ordering::Relaxed) >= 100)
        }
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch(SCHEMA).unwrap();
        connection.execute_batch("WITH RECURSIVE entries(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM entries WHERE n < 20000)
            INSERT INTO objects SELECT 'bucket', printf('a/%05d', n), n, 1, 'text/plain', 0, zeroblob(16), 0, 'STANDARD', 0, 0, printf('%032x', n) FROM entries;
            INSERT INTO objects VALUES('bucket', 'z', 20001, 1, 'text/plain', 0, zeroblob(16), 0, 'STANDARD', 0, 0, '00000000000000000000000000004e21');").unwrap();
        connection.execute_batch(METADATA_SCHEMA).unwrap();
        let counter = AtomicUsize::new(0);
        // Interrupt after 10,000 VM instructions so a row scan cannot masquerade as a seek.
        unsafe {
            rusqlite::ffi::sqlite3_progress_handler(
                connection.handle(),
                100,
                Some(progress),
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
                    metageneration: 1,
                    time_created: UNIX_EPOCH,
                    updated: UNIX_EPOCH,
                })
                .await
                .unwrap();
            let conditions = ObjectConditions::default();
            let permit = catalog
                .prepare_object_upload("bucket", "object", conditions, true)
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
                    true,
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
                        allow_overwrite,
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
                allow_overwrite,
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
                    .check_upload("bucket", "object", conditions, false)
                    .await
                    .unwrap_err(),
                error
            );
        }
        assert!(
            catalog
                .check_upload("bucket", "fresh", Default::default(), false)
                .await
                .is_ok()
        );
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
