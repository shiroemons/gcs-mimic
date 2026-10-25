use super::{
    Catalog,
    resumable::{ResumableError, ResumableOptions, SessionLease, SessionState, read_session},
};
use super::{
    objects::{self, authorized_bucket, check_publication, lookup},
    system_time_micros, unix_micros,
};
use crate::hashing::{Hasher, Hashes};
use crate::object::{ObjectError, ObjectRecord, UploadChunk};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use std::io;
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{OwnedSemaphorePermit, mpsc, oneshot, watch},
};

pub(crate) struct ResumableChunk {
    pub start: u64,
    pub length: u64,
    pub total: Option<u64>,
    pub finalize: bool,
    pub options: ResumableOptions,
    pub catalog_wait: watch::Sender<bool>,
}

fn unavailable(error: impl std::fmt::Display) -> ObjectError {
    ObjectError::Unavailable(error.to_string())
}

async fn receive_chunk(
    lease: &SessionLease,
    chunk: &ResumableChunk,
    body: &mut mpsc::Receiver<Result<UploadChunk, io::Error>>,
    cancellation: &mut oneshot::Receiver<()>,
) -> Result<tokio::fs::File, ObjectError> {
    let mut file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lease.staging_path())
        .await
        .map_err(unavailable)?;
    let durable = lease.session.offset;
    if file.metadata().await.map_err(unavailable)?.len() < durable {
        return Err(unavailable("Acknowledged upload prefix is missing"));
    }
    file.set_len(durable).await.map_err(unavailable)?;
    file.seek(io::SeekFrom::Start(durable))
        .await
        .map_err(unavailable)?;
    let transfer = async {
        let mut received = 0_u64;
        loop {
            let message = tokio::select! {
                biased;
                _ = &mut *cancellation => return Err(ObjectError::BodyAborted("request cancelled".into())),
                message = body.recv() => message,
            };
            match message {
                Some(Ok(UploadChunk::Data(bytes))) => {
                    let next = received
                        .checked_add(bytes.len() as u64)
                        .filter(|n| *n <= chunk.length)
                        .ok_or_else(|| {
                            ObjectError::BodyAborted("Chunk length exceeds Content-Range".into())
                        })?;
                    let discard = durable
                        .saturating_sub(chunk.start + received)
                        .min(bytes.len() as u64) as usize;
                    if discard < bytes.len() {
                        file.write_all(&bytes[discard..]).await.map_err(unavailable)?;
                    }
                    received = next;
                }
                Some(Ok(UploadChunk::End)) if received == chunk.length => break,
                Some(Err(error)) => return Err(ObjectError::BodyAborted(error.to_string())),
                _ => {
                    return Err(ObjectError::BodyAborted(
                        "Chunk did not reach complete EOF".into(),
                    ));
                }
            }
        }
        file.sync_all().await.map_err(unavailable)
    }
    .await;
    if let Err(error) = transfer {
        file.set_len(durable).await.map_err(unavailable)?;
        file.sync_all().await.map_err(unavailable)?;
        return Err(error);
    }
    Ok(file)
}

impl Catalog {
    /// The owned task keeps its lease and body slot until filesystem/SQL cleanup has settled.
    pub(crate) async fn write_resumable_session(
        &self,
        mut lease: SessionLease,
        chunk: ResumableChunk,
        mut body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
        permit: OwnedSemaphorePermit,
        render: impl FnOnce(&ObjectRecord) -> Result<Value, ObjectError> + Send + 'static,
    ) -> Result<Option<Value>, ResumableError> {
        let catalog = self.clone();
        let (_request, mut cancellation) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _permit = permit;
            let file = receive_chunk(&lease, &chunk, &mut body, &mut cancellation).await?;
            let offset = lease.session.offset.max(chunk.start + chunk.length);
            chunk.catalog_wait.send_replace(true);
            catalog
                .checkpoint_resumable_session(&mut lease, offset, chunk.total)
                .await?;
            chunk.catalog_wait.send_replace(false);
            if !chunk.finalize {
                return Ok(None);
            }
            let result = catalog
                .finalize_resumable(&lease, chunk.options, file, chunk.catalog_wait, render)
                .await;
            catalog.resolve_resumable_finalization(&lease, result).await
        })
        .await
        .map_err(unavailable)?
    }

    async fn resolve_resumable_finalization(
        &self,
        lease: &SessionLease,
        result: Result<Value, ResumableError>,
    ) -> Result<Option<Value>, ResumableError> {
        let error = match result {
            Ok(value) => return Ok(Some(value)),
            Err(error) => error,
        };
        let id = lease.session.id.clone();
        let committed = self
            .run_upload(move |connection| {
                connection
                    .query_row(
                        "SELECT * FROM resumable_sessions WHERE id = ?1",
                        [id],
                        read_session,
                    )
                    .optional()
                    .map_err(unavailable)
            })
            .await?;
        if let Some(session) = committed
            && session.state == SessionState::Complete
        {
            return Ok(session.completion);
        }
        if matches!(&error, ResumableError::Object(error) if error.is_backend_failure()) {
            let data_dir = self.0.data_dir.clone();
            let blob_id = lease.session.blob_id.clone();
            let offset = lease.session.offset;
            self.run_upload(move |connection| {
                super::resumable_recovery::recover_prefix(connection, &data_dir, &blob_id, offset)
                    .map_err(unavailable)
            })
            .await?;
        } else {
            self.invalidate_resumable_session(lease).await?;
        }
        Err(error)
    }

    pub(crate) async fn invalidate_resumable_session(
        &self,
        lease: &SessionLease,
    ) -> Result<(), ResumableError> {
        let id = lease.session.id.clone();
        let blob_id = lease.session.blob_id.clone();
        let changed = self.run_upload(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(unavailable)?;
            let changed = transaction.execute("UPDATE resumable_sessions SET state = 'CANCELLED' WHERE id = ?1 AND state = 'ACTIVE'", [id]).map_err(unavailable)?;
            if changed == 1 {
                transaction.execute("INSERT OR IGNORE INTO pending_blob_deletions VALUES (?1)", [&blob_id]).map_err(unavailable)?;
            }
            transaction.commit().map_err(unavailable)?;
            Ok(changed)
        }).await?;
        if changed == 1 {
            let data_dir = self.0.data_dir.clone();
            let blob_id = lease.session.blob_id.clone();
            let cleanup = self
                .run_upload(move |connection| {
                    super::resumable_maintenance::clean_session_files(
                        connection, &data_dir, &blob_id,
                    )
                    .map_err(unavailable)
                })
                .await;
            if let Err(error) = cleanup {
                *self
                    .0
                    .maintenance_error
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = Some(error.to_string());
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Warn,
                    "upload_cleanup_deferred",
                    serde_json::json!({"message":format!("invalid upload cleanup failed; maintenance will retry: {error}")}),
                );
            }
        }
        self.0.maintenance_wake.notify_one();
        Ok(())
    }

    async fn finalize_resumable(
        &self,
        lease: &SessionLease,
        options: ResumableOptions,
        mut file: tokio::fs::File,
        catalog_wait: watch::Sender<bool>,
        render: impl FnOnce(&ObjectRecord) -> Result<Value, ObjectError> + Send + 'static,
    ) -> Result<Value, ResumableError> {
        file.seek(io::SeekFrom::Start(0))
            .await
            .map_err(unavailable)?;
        let mut hasher = Hasher::default();
        let mut length = 0_u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            let n = file.read(&mut buffer).await.map_err(unavailable)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            length += n as u64;
        }
        let Hashes { md5, crc32c: crc } = hasher.finish();
        if options.expected.md5.is_some_and(|v| v != md5)
            || options.expected.crc32c.is_some_and(|v| v != crc)
        {
            return Err(ObjectError::ChecksumMismatch.into());
        }
        if Some(length) != lease.session.total || length != lease.session.offset {
            return Err(
                ObjectError::BodyAborted("Final length does not match total".into()).into(),
            );
        }
        file.sync_all().await.map_err(unavailable)?;
        drop(file);
        let source = lease.staging_path();
        let destination = objects::blob_path(&self.0.data_dir, &lease.session.blob_id)?;
        tokio::task::spawn_blocking(move || objects::publish_blob(&source, &destination))
            .await
            .map_err(unavailable)?
            .map_err(unavailable)?;
        let session = lease.session.clone();
        let clock = self.0.clock.clone();
        catalog_wait.send_replace(true);
        let publication =
            move |connection: &mut rusqlite::Connection| -> Result<Value, ResumableError> {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(unavailable)?;
                let stored = transaction
                    .query_row(
                        "SELECT * FROM resumable_sessions WHERE id = ?1",
                        [&session.id],
                        read_session,
                    )
                    .map_err(unavailable)?;
                if clock.now() >= stored.deadline {
                    return Err(ResumableError::NotFound);
                }
                if stored.state != SessionState::Active {
                    return Err(ResumableError::Gone);
                }
                let incarnation: Option<String> = transaction
                    .query_row(
                        "SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1",
                        [&session.bucket],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(unavailable)?;
                if incarnation.as_deref() != Some(&session.incarnation) {
                    return Err(ObjectError::Forbidden.into());
                }
                let (storage_class, allow_overwrite) =
                    authorized_bucket(&transaction, &session.bucket, &options.access)?;
                let (versioning, default_hold) =
                    Catalog::bucket_settings_in_transaction(&transaction, &session.bucket)
                        .map_err(unavailable)?;
                let previous = lookup(&transaction, &session.bucket, &session.name)?;
                check_publication(previous.as_ref(), options.conditions, allow_overwrite)?;
                let now = clock.now();
                now.duration_since(std::time::UNIX_EPOCH)
                    .map_err(unavailable)?;
                let micros = unix_micros(now).map_err(unavailable)?;
                let time = system_time_micros(micros).map_err(unavailable)?;
                let (_, _retired, resource) = objects::publish_in_transaction(
                    &transaction,
                    micros,
                    objects::GenerationAssignment::Automatic,
                    previous.as_ref(),
                    versioning,
                    &session.blob_id,
                    super::change_journal::origin(&options.access),
                    |generation| {
                        let mut record = ObjectRecord {
                            bucket: session.bucket,
                            name: session.name,
                            generation,
                            metageneration: 1,
                            content_type: options.content_type,
                            content_encoding: None,
                            content_disposition: None,
                            content_language: None,
                            cache_control: None,
                            metadata: Default::default(),
                            size: length,
                            md5,
                            crc32c: crc,
                            component_count: None,
                            storage_class,
                            time_created: time,
                            updated: time,
                            time_deleted: None,
                            event_based_hold: default_hold,
                        };
                        options.metadata.apply(&mut record)?;
                        let resource = render(&record)?;
                        Ok((record, resource))
                    },
                )?;
                transaction.execute("UPDATE resumable_sessions SET state = 'COMPLETE', completion = ?1 WHERE id = ?2 AND state = 'ACTIVE'",params![resource.to_string(),session.id]).map_err(unavailable)?;
                transaction.commit().map_err(unavailable)?;
                Ok(resource)
            };
        let result = self
            .run_upload(move |connection| Ok(publication(connection)))
            .await?;
        if result.is_ok() {
            self.reclaim_after_commit().await;
        }
        result
    }
}
