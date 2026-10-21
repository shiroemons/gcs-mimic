use super::{Catalog, MAX_UPLOADS, system_time, unavailable, unix_millis};
use crate::{
    bucket::CatalogError,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord, UploadChunk},
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
    md5, crc32c, storage_class, time_created, updated, blob_id";

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

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn unlink(path: &Path) -> io::Result<()> {
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
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        self.run_object(move |connection| {
            bucket_class(connection, &bucket)?;
            let previous = lookup(connection, &bucket, &name)?;
            check_conditions(
                previous.as_ref().map(|(record, _)| record),
                conditions,
                false,
            )
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
        let permit = self.prepare_object_upload(bucket, name, conditions).await?;
        self.put_object_prechecked(
            bucket,
            name,
            content_type,
            expected,
            conditions,
            max_bytes,
            body,
            permit,
        )
        .await
    }

    pub(crate) async fn prepare_object_upload(
        &self,
        bucket: &str,
        name: &str,
        conditions: ObjectConditions,
    ) -> Result<OwnedSemaphorePermit, ObjectError> {
        let permit =
            self.0.uploads.clone().try_acquire_owned().map_err(|_| {
                object_unavailable(format!("upload limit of {MAX_UPLOADS} reached"))
            })?;
        if !self.is_healthy() {
            return Err(object_unavailable("catalog is closed"));
        }
        self.check_object_upload(bucket, name, conditions).await?;
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
                tokio::task::spawn_blocking(move || -> io::Result<()> {
                    let shard = destination.parent().expect("blob parent");
                    fs::create_dir_all(shard)?;
                    sync_dir(shard.parent().expect("blobs directory"))?;
                    fs::rename(source, &destination)?;
                    sync_dir(shard)
                }).await.map_err(object_unavailable)?.map_err(object_unavailable)?;
                let data_dir = catalog.0.data_dir.clone();
                let clock = catalog.0.clock.clone();
                let blob_id = id.clone();
                catalog.run_upload(move |connection| {
                    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)
                        .map_err(object_unavailable)?;
                    let storage_class = bucket_class(&transaction, &bucket)?;
                    let previous = lookup(&transaction, &bucket, &name)?;
                    check_conditions(previous.as_ref().map(|(record, _)| record), conditions, false)?;
                    let now = clock.now();
                    let micros = i64::try_from(now.duration_since(UNIX_EPOCH)
                        .map_err(object_unavailable)?.as_micros()).map_err(object_unavailable)?;
                    let last: i64 = transaction.query_row("SELECT last_generation FROM object_generation WHERE singleton = 1", [], |row| row.get(0)).map_err(object_unavailable)?;
                    let generation = micros.max(last.checked_add(1).ok_or_else(|| object_unavailable("generation exhausted"))?);
                    transaction.execute("UPDATE object_generation SET last_generation = ?1 WHERE singleton = 1", [generation]).map_err(object_unavailable)?;
                    let millis = unix_millis(now).map_err(object_unavailable)?;
                    let time = system_time(millis).map_err(object_unavailable)?;
                    let record = ObjectRecord {
                        bucket, name, generation, metageneration: 1,
                        content_type, size, md5, crc32c, storage_class,
                        time_created: time, updated: time,
                    };
                    transaction.execute(
                        "INSERT OR REPLACE INTO objects VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        params![record.bucket, record.name, record.generation, record.metageneration,
                            record.content_type, i64::try_from(size).map_err(object_unavailable)?,
                            &record.md5[..], record.crc32c, record.storage_class, millis, millis, blob_id],
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
    expected: ExpectedHashes,
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
                .prepare_object_upload("bucket", "object", conditions)
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
