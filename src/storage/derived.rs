//! Generation-pinned, resumable byte derivation shared by copy, rewrite and compose.

use super::{Catalog, objects, resumable::WriterGuard, system_time, unix_millis};
use crate::{
    bucket::BucketAccess,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectMetadataUpdate, ObjectRecord},
};
use md5::{
    Digest, Md5,
    digest::common::hazmat::{SerializableState, SerializedState},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

const BUFFER_SIZE: usize = 64 * 1024;
const MAX_TRANSFERS: usize = 128;
pub(super) const SCHEMA: &str = "CREATE TABLE derived_transfers (
    id TEXT PRIMARY KEY CHECK(length(id) = 32),
    total INTEGER NOT NULL CHECK(total >= 0),
    offset INTEGER NOT NULL CHECK(offset >= 0 AND offset <= total),
    md5_state BLOB NOT NULL,
    crc32c INTEGER NOT NULL CHECK(crc32c BETWEEN 0 AND 4294967295)
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

fn unavailable(error: impl std::fmt::Display) -> ObjectError {
    ObjectError::Unavailable(error.to_string())
}

fn random_id() -> Result<String, ObjectError> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(unavailable)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn stage_path(data_dir: &Path, id: &str) -> Result<PathBuf, ObjectError> {
    objects::blob_path(data_dir, id)?;
    Ok(data_dir.join("staging").join(id))
}

/// A metadata snapshot and independent link to immutable bytes, including after source deletion.
/// Unconsumed handles clean their staging link; `begin_derived` transfers ownership to SQLite.
pub struct DerivedSource {
    pub object: ObjectRecord,
    path: Option<PathBuf>,
    id: String,
}

impl Drop for DerivedSource {
    fn drop(&mut self) {
        if let Some(path) = &self.path
            && let Err(error) = objects::unlink(path)
        {
            eprintln!("deferred derived source cleanup: {error}");
        }
    }
}

pub struct DerivedSegment {
    pub source: DerivedSource,
    pub offset: u64,
    pub length: u64,
}

impl DerivedSource {
    pub fn whole(self) -> DerivedSegment {
        let length = self.object.size;
        DerivedSegment {
            source: self,
            offset: 0,
            length,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DerivedTransfer {
    pub id: String,
    pub total: u64,
    pub offset: u64,
}

/// Adapters supply inherited or replacement metadata explicitly; bytes and destination defaults
/// are transport independent. An explicit class is used by rewrite, otherwise the bucket default.
#[derive(Clone, Debug, Default)]
pub struct DerivedDestination {
    pub bucket: String,
    pub name: String,
    pub content_type: String,
    pub metadata: ObjectMetadataUpdate,
    pub storage_class: Option<String>,
    pub expected: ExpectedHashes,
    pub conditions: ObjectConditions,
    pub access: BucketAccess,
}

struct Input {
    id: String,
    start: u64,
    length: u64,
    size: u64,
}

fn inputs(connection: &Connection, id: &str) -> Result<Vec<Input>, ObjectError> {
    connection.prepare("SELECT blob_id, start, length, size FROM derived_inputs WHERE transfer_id = ?1 ORDER BY position LIMIT 33")
        .map_err(unavailable)?.query_map([id], |row| Ok(Input { id: row.get(0)?, start: row.get::<_, i64>(1)? as u64, length: row.get::<_, i64>(2)? as u64, size: row.get::<_, i64>(3)? as u64 }))
        .map_err(unavailable)?.collect::<rusqlite::Result<_>>().map_err(unavailable)
}

fn transfer(connection: &Connection, id: &str) -> Result<(DerivedTransfer, Md5, u32), ObjectError> {
    let (total, offset, state, crc): (i64, i64, Vec<u8>, u32) = connection
        .query_row(
            "SELECT total, offset, md5_state, crc32c FROM derived_transfers WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(unavailable)?
        .ok_or(ObjectError::ObjectNotFound)?;
    let state = SerializedState::<Md5>::try_from(state.as_slice()).map_err(unavailable)?;
    let md5 = Md5::deserialize(&state).map_err(unavailable)?;
    Ok((
        DerivedTransfer {
            id: id.into(),
            total: total as u64,
            offset: offset as u64,
        },
        md5,
        crc,
    ))
}

impl Catalog {
    /// Call only from a publication callback: the caller's transaction owns all checks/mutations.
    pub fn check_derived_source_in_transaction(
        connection: &Connection,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<ObjectRecord, ObjectError> {
        objects::selected(
            connection, bucket, name, generation, conditions, false, access,
        )
        .map(|value| value.0)
    }

    /// Hard deletion is intentionally separate from versioning retirement. Any later callback
    /// failure rolls this deletion back together with the destination and deletion journal.
    pub fn delete_derived_source_in_transaction(
        connection: &Connection,
        bucket: &str,
        name: &str,
        generation: i64,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        let (object, id) = objects::selected(
            connection,
            bucket,
            name,
            Some(generation),
            conditions,
            false,
            access,
        )?;
        if object.event_based_hold {
            return Err(ObjectError::ActiveHold);
        }
        connection
            .execute("DELETE FROM objects WHERE blob_id = ?1", [&id])
            .map_err(unavailable)?;
        connection
            .execute("INSERT INTO pending_blob_deletions VALUES (?1)", [id])
            .map_err(unavailable)?;
        Ok(())
    }

    /// Source NotMatch conditions are mutation conditions (412), not conditional GETs (304).
    pub async fn resolve_derived_source(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<DerivedSource, ObjectError> {
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let access = access.clone();
        let data_dir = self.0.data_dir.clone();
        self.run_object(move |connection| {
            let transaction = connection.transaction().map_err(unavailable)?;
            let (object, original) = objects::selected(
                &transaction,
                &bucket,
                &name,
                generation,
                conditions,
                false,
                &access,
            )?;
            let id = random_id()?;
            let path = stage_path(&data_dir, &id)?;
            fs::hard_link(objects::blob_path(&data_dir, &original)?, &path).map_err(unavailable)?;
            Ok(DerivedSource {
                object,
                path: Some(path),
                id,
            })
        })
        .await
    }

    /// Input handles are consumed only after durable ownership commits. No source catalog row
    /// is needed on later calls, so a token can safely outlive source overwrite or deletion.
    pub async fn begin_derived(
        &self,
        mut segments: Vec<DerivedSegment>,
        max_bytes: u64,
    ) -> Result<DerivedTransfer, ObjectError> {
        if segments.is_empty() || segments.len() > 32 {
            return Err(unavailable("derived transfer requires 1 to 32 sources"));
        }
        let total = segments.iter().try_fold(0_u64, |total, segment| {
            if segment.offset > segment.source.object.size
                || segment.length > segment.source.object.size - segment.offset
            {
                return Err(unavailable("derived source range is out of bounds"));
            }
            total
                .checked_add(segment.length)
                .filter(|total| *total <= max_bytes && *total <= i64::MAX as u64)
                .ok_or(ObjectError::TooLarge)
        })?;
        let _permit = self.admit_object_upload()?;
        let data_dir = self.0.data_dir.clone();
        self.run_object(move |connection| {
            let count: i64 = connection
                .query_row("SELECT count(*) FROM derived_transfers", [], |row| {
                    row.get(0)
                })
                .map_err(unavailable)?;
            if count >= MAX_TRANSFERS as i64 {
                return Err(unavailable("derived transfer limit reached"));
            }
            let id = random_id()?;
            let path = stage_path(&data_dir, &id)?;
            fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&path)
                .and_then(|file| file.sync_all())
                .map_err(unavailable)?;
            let mut preserve = false;
            let result = (|| {
                #[cfg(unix)]
                objects::sync_dir(&data_dir.join("staging")).map_err(unavailable)?;
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(unavailable)?;
                transaction
                    .execute(
                        "INSERT INTO derived_transfers VALUES (?1, ?2, 0, ?3, 0)",
                        params![id, total as i64, Md5::new().serialize().as_slice()],
                    )
                    .map_err(unavailable)?;
                for (position, segment) in segments.iter().enumerate() {
                    transaction
                        .execute(
                            "INSERT INTO derived_inputs VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            params![
                                id,
                                position as i64,
                                segment.source.id,
                                segment.offset as i64,
                                segment.length as i64,
                                segment.source.object.size as i64
                            ],
                        )
                        .map_err(unavailable)?;
                }
                if let Err(error) = transaction.commit() {
                    // A failed commit can be ambiguous; retain every confirmed reference.
                    preserve = true;
                    let committed: bool = connection
                        .query_row(
                            "SELECT EXISTS(SELECT 1 FROM derived_transfers WHERE id = ?1)",
                            [&id],
                            |row| row.get(0),
                        )
                        .map_err(unavailable)?;
                    if !committed {
                        preserve = false;
                        return Err(unavailable(error));
                    }
                }
                Ok(DerivedTransfer {
                    id,
                    total,
                    offset: 0,
                })
            })();
            match result {
                Ok(transfer) => {
                    for segment in &mut segments {
                        segment.source.path = None;
                    }
                    Ok(transfer)
                }
                Err(error) => {
                    if preserve {
                        for segment in &mut segments {
                            segment.source.path = None;
                        }
                        return Err(error);
                    }
                    objects::unlink(&path).map_err(unavailable)?;
                    Err(error)
                }
            }
        })
        .await
    }

    fn derived_guard(&self, id: &str) -> Result<WriterGuard, ObjectError> {
        let id = format!("derived:{id}");
        let mut writers = self.0.session_writers.lock().map_err(unavailable)?;
        if !writers.insert(id.clone()) {
            return Err(unavailable("derived transfer is already in use"));
        }
        Ok(WriterGuard {
            catalog: self.clone(),
            id,
        })
    }

    pub async fn get_derived(&self, id: &str) -> Result<DerivedTransfer, ObjectError> {
        let id = id.to_owned();
        self.run_object(move |connection| transfer(connection, &id).map(|value| value.0))
            .await
    }

    /// Every durable offset includes the matching incremental hash state. File fsync precedes
    /// SQLite commit; an interrupted tail is discarded before retry, never counted twice.
    pub async fn advance_derived(
        &self,
        id: &str,
        budget: u64,
    ) -> Result<DerivedTransfer, ObjectError> {
        let permit = self.admit_object_upload()?;
        let guard = self.derived_guard(id)?;
        let catalog = self.clone();
        let id = id.to_owned();
        // Detached ownership keeps cancellation from racing directory closure or cleanup.
        tokio::spawn(async move {
            let (_permit, _guard) = (permit, guard);
            let query = id.clone();
            let (mut progress, mut md5, mut crc, sources) = catalog.run_upload(move |connection| {
                let (progress, md5, crc) = transfer(connection, &query)?;
                Ok((progress, md5, crc, inputs(connection, &query)?))
            }).await?;
            let path = stage_path(&catalog.0.data_dir, &id)?;
            let mut output = tokio::fs::OpenOptions::new().write(true).open(path).await.map_err(unavailable)?;
            output.set_len(progress.offset).await.map_err(unavailable)?;
            output.seek(io::SeekFrom::Start(progress.offset)).await.map_err(unavailable)?;
            let target = progress.offset.saturating_add(budget).min(progress.total);
            let mut skip = progress.offset;
            let mut buffer = [0_u8; BUFFER_SIZE];
            for source in sources {
                if skip >= source.length { skip -= source.length; continue; }
                if progress.offset == target { break; }
                let mut input = tokio::fs::File::open(stage_path(&catalog.0.data_dir, &source.id)?).await.map_err(unavailable)?;
                input.seek(io::SeekFrom::Start(source.start + skip)).await.map_err(unavailable)?;
                let mut remaining = (source.length - skip).min(target - progress.offset);
                skip = 0;
                while remaining > 0 {
                    let length = remaining.min(BUFFER_SIZE as u64) as usize;
                    input.read_exact(&mut buffer[..length]).await.map_err(unavailable)?;
                    output.write_all(&buffer[..length]).await.map_err(unavailable)?;
                    md5.update(&buffer[..length]);
                    crc = crc32c::crc32c_append(crc, &buffer[..length]);
                    progress.offset += length as u64;
                    remaining -= length as u64;
                }
            }
            output.sync_all().await.map_err(unavailable)?;
            catalog.run_upload(move |connection| {
                connection.execute("UPDATE derived_transfers SET offset = ?2, md5_state = ?3, crc32c = ?4 WHERE id = ?1", params![id, progress.offset as i64, md5.serialize().as_slice(), crc]).map_err(unavailable)?;
                Ok(progress)
            }).await
        }).await.map_err(unavailable)?
    }

    /// The callback joins protocol-specific completion state to the same destination commit.
    pub async fn publish_derived(
        &self,
        id: &str,
        destination: DerivedDestination,
        complete: impl FnOnce(&Connection, &ObjectRecord) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<ObjectRecord, ObjectError> {
        let permit = self.admit_object_upload()?;
        let guard = self.derived_guard(id)?;
        let catalog = self.clone();
        let id = id.to_owned();
        tokio::spawn(async move {
            let (_permit, _guard) = (permit, guard);
            let query = id.clone();
            let data_dir = catalog.0.data_dir.clone();
            let clock = catalog.0.clock.clone();
            let result = catalog.run_upload(move |connection| {
                let (progress, md5, crc32c) = transfer(connection, &query)?;
                let sources = inputs(connection, &query)?;
                if progress.offset != progress.total { return Err(unavailable("derived transfer is incomplete")); }
                let md5: [u8; 16] = md5.finalize().into();
                if destination.expected.md5.is_some_and(|hash| hash != md5) || destination.expected.crc32c.is_some_and(|hash| hash != crc32c) { return Err(ObjectError::ChecksumMismatch); }
                let stage = stage_path(&data_dir, &query)?;
                objects::publish_blob(&stage, &objects::blob_path(&data_dir, &query)?).map_err(unavailable)?;
                let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
                let (default_class, allow_overwrite) = objects::authorized_bucket(&transaction, &destination.bucket, &destination.access)?;
                let (versioning, default_hold) = Catalog::bucket_settings_in_transaction(&transaction, &destination.bucket).map_err(unavailable)?;
                let previous = objects::lookup(&transaction, &destination.bucket, &destination.name)?;
                objects::check_publication(previous.as_ref(), destination.conditions, allow_overwrite)?;
                let now = clock.now();
                let micros = i64::try_from(now.duration_since(UNIX_EPOCH).map_err(unavailable)?.as_micros()).map_err(unavailable)?;
                let last: i64 = transaction.query_row("SELECT last_generation FROM object_generation WHERE singleton = 1", [], |row| row.get(0)).map_err(unavailable)?;
                let generation = micros.max(last.checked_add(1).ok_or_else(|| unavailable("generation exhausted"))?);
                let millis = unix_millis(now).map_err(unavailable)?;
                let time = system_time(millis).map_err(unavailable)?;
                let storage_class = destination.storage_class.unwrap_or(default_class);
                crate::bucket::validate_storage_class(&storage_class).map_err(|error| ObjectError::UnsupportedMetadata(error.to_string()))?;
                let mut record = ObjectRecord {
                    bucket: destination.bucket, name: destination.name, generation, metageneration: 1,
                    content_type: destination.content_type, size: progress.total, md5, crc32c, storage_class,
                    content_encoding: None, content_disposition: None, content_language: None,
                    cache_control: None, metadata: Default::default(), time_created: time, updated: time,
                    time_deleted: None, event_based_hold: default_hold,
                };
                destination.metadata.apply(&mut record)?;
                let metadata = serde_json::to_string(&record.metadata).map_err(unavailable)?;
                let retired = objects::retire_previous(&transaction, previous.as_ref(), versioning, millis)?;
                transaction.execute("UPDATE object_generation SET last_generation = ?1 WHERE singleton = 1", [generation]).map_err(unavailable)?;
                transaction.execute("INSERT INTO objects (bucket,name,generation,metageneration,content_type,size,md5,crc32c,storage_class,time_created,updated,blob_id,content_encoding,content_disposition,content_language,cache_control,metadata,event_based_hold) VALUES (?1,?2,?3,1,?4,?5,?6,?7,?8,?9,?9,?10,?11,?12,?13,?14,?15,?16)", params![record.bucket, record.name, generation, record.content_type, record.size as i64, &md5[..], crc32c, record.storage_class, millis, query, record.content_encoding, record.content_disposition, record.content_language, record.cache_control, metadata, record.event_based_hold]).map_err(unavailable)?;
                complete(&transaction, &record)?;
                transaction.execute("DELETE FROM derived_inputs WHERE transfer_id = ?1", [&query]).map_err(unavailable)?;
                transaction.execute("DELETE FROM derived_transfers WHERE id = ?1", [&query]).map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
                for source in sources {
                    if let Err(error) = objects::unlink(&stage_path(&data_dir, &source.id)?) {
                        eprintln!("deferred derived source cleanup: {error}");
                    }
                }
                if let Some(old) = retired && let Err(error) = objects::reclaim(connection, &data_dir, &old) { eprintln!("deferred blob reclaim {old}: {error}"); }
                let pending = connection.prepare("SELECT blob_id FROM pending_blob_deletions ORDER BY blob_id LIMIT 128").map_err(unavailable)?
                    .query_map([], |row| row.get::<_, String>(0)).map_err(unavailable)?.collect::<rusqlite::Result<Vec<_>>>().map_err(unavailable)?;
                for old in pending {
                    if let Err(error) = objects::reclaim(connection, &data_dir, &old) { eprintln!("deferred blob reclaim {old}: {error}"); }
                }
                Ok(record)
            }).await;
            // Check SQL references even after an ambiguous commit error before removing bytes.
            catalog.cleanup_derived(&id).await?;
            result
        }).await.map_err(unavailable)?
    }

    async fn cleanup_derived(&self, id: &str) -> Result<(), ObjectError> {
        let id = id.to_owned();
        let data_dir = self.0.data_dir.clone();
        self.run_upload(move |connection| {
            let sources = inputs(connection, &id)?;
            let transaction = connection.transaction().map_err(unavailable)?;
            transaction
                .execute("DELETE FROM derived_inputs WHERE transfer_id = ?1", [&id])
                .map_err(unavailable)?;
            transaction
                .execute("DELETE FROM derived_transfers WHERE id = ?1", [&id])
                .map_err(unavailable)?;
            transaction.commit().map_err(unavailable)?;
            for source in sources {
                objects::unlink(&stage_path(&data_dir, &source.id)?).map_err(unavailable)?;
            }
            objects::unlink(&stage_path(&data_dir, &id)?).map_err(unavailable)?;
            let referenced: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM objects WHERE blob_id = ?1)",
                    [&id],
                    |row| row.get(0),
                )
                .map_err(unavailable)?;
            if !referenced {
                objects::reclaim(connection, &data_dir, &id)?;
            }
            Ok(())
        })
        .await
    }

    pub async fn cancel_derived(&self, id: &str) -> Result<(), ObjectError> {
        let _guard = self.derived_guard(id)?;
        self.cleanup_derived(id).await
    }
}

pub(super) fn recover(connection: &Connection, data_dir: &Path) -> Result<(), ObjectError> {
    let ids = connection
        .prepare("SELECT id FROM derived_transfers ORDER BY id LIMIT 129")
        .map_err(unavailable)?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(unavailable)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(unavailable)?;
    if ids.len() > MAX_TRANSFERS {
        return Err(unavailable("too many derived transfers"));
    }
    for id in ids {
        let (progress, _, _) = transfer(connection, &id)?;
        let sources = inputs(connection, &id)?;
        if sources.is_empty()
            || sources.len() > 32
            || sources.iter().map(|source| source.length).sum::<u64>() != progress.total
        {
            return Err(unavailable("invalid derived sources"));
        }
        let stage = stage_path(data_dir, &id)?;
        if !stage.try_exists().map_err(unavailable)? {
            // Rename may have reached disk immediately before the catalog transaction rolled back.
            fs::rename(objects::blob_path(data_dir, &id)?, &stage).map_err(unavailable)?;
        }
        let output = fs::OpenOptions::new()
            .write(true)
            .open(stage)
            .map_err(unavailable)?;
        if output.metadata().map_err(unavailable)?.len() < progress.offset {
            return Err(unavailable("derived prefix is shorter than its checkpoint"));
        }
        output.set_len(progress.offset).map_err(unavailable)?;
        output.sync_all().map_err(unavailable)?;
        for source in sources {
            let metadata =
                fs::symlink_metadata(stage_path(data_dir, &source.id)?).map_err(unavailable)?;
            if !metadata.is_file() || metadata.len() != source.size {
                return Err(unavailable("invalid pinned derived source"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bucket::BucketRecord, object::UploadChunk};
    use bytes::Bytes;
    use std::io::Write;
    use tokio::sync::mpsc;

    async fn setup() -> (Catalog, PathBuf) {
        let path = std::env::temp_dir().join(format!("gcs-mimic-derived-{}", random_id().unwrap()));
        let catalog = Catalog::open(&path).await.unwrap();
        catalog
            .insert_bucket(BucketRecord {
                name: "bucket".into(),
                project: "project".into(),
                location: "US".into(),
                location_type: "multi-region".into(),
                storage_class: "STANDARD".into(),
                labels: Default::default(),
                versioning_enabled: false,
                default_event_based_hold: false,
                metageneration: 1,
                time_created: UNIX_EPOCH,
                updated: UNIX_EPOCH,
            })
            .await
            .unwrap();
        (catalog, path)
    }

    async fn put(catalog: &Catalog, name: &str, bytes: &'static [u8]) -> ObjectRecord {
        let (sender, body) = mpsc::channel(2);
        sender
            .try_send(Ok(UploadChunk::Data(Bytes::from_static(bytes))))
            .unwrap();
        sender.try_send(Ok(UploadChunk::End)).unwrap();
        catalog
            .put_object(
                "bucket",
                name,
                "text/plain".into(),
                Default::default(),
                Default::default(),
                1_000_000,
                body,
            )
            .await
            .unwrap()
    }

    async fn source(catalog: &Catalog, name: &str) -> DerivedSource {
        catalog
            .resolve_derived_source(
                "bucket",
                name,
                None,
                Default::default(),
                &BucketAccess::Unchecked,
            )
            .await
            .unwrap()
    }

    fn destination(name: &str) -> DerivedDestination {
        DerivedDestination {
            bucket: "bucket".into(),
            name: name.into(),
            content_type: "text/plain".into(),
            ..Default::default()
        }
    }

    async fn ready(catalog: &Catalog, name: &str) -> DerivedTransfer {
        let transfer = catalog
            .begin_derived(vec![source(catalog, name).await.whole()], u64::MAX)
            .await
            .unwrap();
        catalog
            .advance_derived(&transfer.id, u64::MAX)
            .await
            .unwrap()
    }

    async fn bytes(catalog: &Catalog, name: &str) -> Vec<u8> {
        let (_, mut reader) = catalog
            .open_object("bucket", name, None, Default::default())
            .await
            .unwrap();
        let mut bytes = Vec::new();
        while let Some(chunk) = reader.next_chunk().await.unwrap() {
            bytes.extend_from_slice(&chunk);
        }
        bytes
    }

    #[tokio::test]
    async fn pinned_ranges_resume_after_source_deletion_restart_and_uncommitted_tail() {
        let (catalog, path) = setup().await;
        let first = put(&catalog, "first", b"abcdef").await;
        put(&catalog, "second", b"12345").await;
        let transfer = catalog
            .begin_derived(
                vec![
                    DerivedSegment {
                        source: source(&catalog, "first").await,
                        offset: 1,
                        length: 4,
                    },
                    DerivedSegment {
                        source: source(&catalog, "second").await,
                        offset: 2,
                        length: 3,
                    },
                ],
                7,
            )
            .await
            .unwrap();
        assert_eq!(
            catalog
                .advance_derived(&transfer.id, 3)
                .await
                .unwrap()
                .offset,
            3
        );
        catalog
            .delete_object(
                "bucket",
                "first",
                Some(first.generation),
                Default::default(),
            )
            .await
            .unwrap();
        put(&catalog, "second", b"changed").await;
        let output = stage_path(&path, &transfer.id).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&output)
            .unwrap()
            .write_all(b"uncommitted")
            .unwrap();
        catalog.close().await.unwrap();
        drop(catalog);
        let catalog = Catalog::open(&path).await.unwrap();
        assert_eq!(fs::metadata(&output).unwrap().len(), 3);
        assert_eq!(
            catalog
                .advance_derived(&transfer.id, 2)
                .await
                .unwrap()
                .offset,
            5
        );
        catalog
            .advance_derived(&transfer.id, u64::MAX)
            .await
            .unwrap();
        let record = catalog
            .publish_derived(&transfer.id, destination("copy"), |_, _| Ok(()))
            .await
            .unwrap();
        assert_eq!(bytes(&catalog, "copy").await, b"bcde345");
        assert_eq!(record.md5, <[u8; 16]>::from(Md5::digest(b"bcde345")));
        assert_eq!(record.crc32c, crc32c::crc32c(b"bcde345"));
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn recovery_restores_renamed_uncommitted_output_and_empty_sources() {
        let (catalog, path) = setup().await;
        put(&catalog, "empty", b"").await;
        let transfer = ready(&catalog, "empty").await;
        objects::publish_blob(
            &stage_path(&path, &transfer.id).unwrap(),
            &objects::blob_path(&path, &transfer.id).unwrap(),
        )
        .unwrap();
        catalog.close().await.unwrap();
        drop(catalog);
        let catalog = Catalog::open(&path).await.unwrap();
        let object = catalog
            .publish_derived(&transfer.id, destination("empty-copy"), |_, _| Ok(()))
            .await
            .unwrap();
        assert_eq!(object.size, 0);
        assert_eq!(object.md5, <[u8; 16]>::from(Md5::digest(b"")));
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn destination_race_has_one_winner_and_failed_publication_cleans_staging() {
        let (catalog, path) = setup().await;
        put(&catalog, "source", b"contents").await;
        let left = ready(&catalog, "source").await;
        let right = ready(&catalog, "source").await;
        let mut target = destination("target");
        target.conditions.if_generation_match = Some(0);
        let (left, right) = tokio::join!(
            catalog.publish_derived(&left.id, target.clone(), |_, _| Ok(())),
            catalog.publish_derived(&right.id, target, |_, _| Ok(())),
        );
        assert!(matches!(
            (&left, &right),
            (Ok(_), Err(ObjectError::PreconditionFailed))
                | (Err(ObjectError::PreconditionFailed), Ok(_))
        ));
        assert_eq!(bytes(&catalog, "target").await, b"contents");
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        let transfer = ready(&catalog, "source").await;
        let mut target = destination("target");
        target.expected.crc32c = Some(0);
        assert_eq!(
            catalog
                .publish_derived(&transfer.id, target, |_, _| Ok(()))
                .await,
            Err(ObjectError::ChecksumMismatch)
        );
        assert_eq!(bytes(&catalog, "target").await, b"contents");
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn callback_source_deletion_rolls_back_with_destination_and_checks_holds() {
        let (catalog, path) = setup().await;
        let original = put(&catalog, "source", b"source").await;
        put(&catalog, "target", b"original").await;
        let transfer = ready(&catalog, "source").await;
        let generation = original.generation;
        let result = catalog
            .publish_derived(&transfer.id, destination("target"), move |connection, _| {
                Catalog::delete_derived_source_in_transaction(
                    connection,
                    "bucket",
                    "source",
                    generation,
                    Default::default(),
                    &BucketAccess::Unchecked,
                )?;
                Err(ObjectError::PreconditionFailed)
            })
            .await;
        assert_eq!(result, Err(ObjectError::PreconditionFailed));
        assert_eq!(bytes(&catalog, "target").await, b"original");
        assert_eq!(bytes(&catalog, "source").await, b"source");
        catalog
            .update_object_metadata(
                "bucket",
                "source",
                None,
                Default::default(),
                ObjectMetadataUpdate {
                    event_based_hold: Some(true),
                    ..Default::default()
                },
                |_| Ok(()),
            )
            .await
            .unwrap();
        let transfer = ready(&catalog, "source").await;
        let result = catalog
            .publish_derived(&transfer.id, destination("target"), move |connection, _| {
                Catalog::delete_derived_source_in_transaction(
                    connection,
                    "bucket",
                    "source",
                    generation,
                    Default::default(),
                    &BucketAccess::Unchecked,
                )
            })
            .await;
        assert_eq!(result, Err(ObjectError::ActiveHold));
        assert_eq!(bytes(&catalog, "target").await, b"original");
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn all_source_conditions_are_checked_before_pinning_and_ranges_are_validated() {
        let (catalog, path) = setup().await;
        let original = put(&catalog, "source", b"source").await;
        for conditions in [
            ObjectConditions {
                if_generation_match: Some(original.generation + 1),
                ..Default::default()
            },
            ObjectConditions {
                if_generation_not_match: Some(original.generation),
                ..Default::default()
            },
            ObjectConditions {
                if_metageneration_match: Some(2),
                ..Default::default()
            },
            ObjectConditions {
                if_metageneration_not_match: Some(1),
                ..Default::default()
            },
        ] {
            assert!(matches!(
                catalog
                    .resolve_derived_source(
                        "bucket",
                        "source",
                        None,
                        conditions,
                        &BucketAccess::Unchecked
                    )
                    .await,
                Err(ObjectError::PreconditionFailed)
            ));
        }
        let denied = BucketAccess::Policy {
            identity: None,
            permission: crate::auth::OBJECTS_GET.into(),
            project: None,
        };
        assert!(matches!(
            catalog
                .resolve_derived_source("bucket", "source", None, Default::default(), &denied)
                .await,
            Err(ObjectError::Forbidden)
        ));
        assert!(
            catalog
                .begin_derived(
                    vec![DerivedSegment {
                        source: source(&catalog, "source").await,
                        offset: 5,
                        length: 2
                    }],
                    100
                )
                .await
                .is_err()
        );
        assert!(matches!(
            catalog
                .begin_derived(vec![source(&catalog, "source").await.whole()], 5)
                .await,
            Err(ObjectError::TooLarge)
        ));
        assert_eq!(fs::read_dir(path.join("staging")).unwrap().count(), 0);
        catalog.close().await.unwrap();
        fs::remove_dir_all(path).unwrap();
    }
}
