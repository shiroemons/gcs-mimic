use super::{
    Catalog, UploadGuard,
    resumable::{ResumableError, ResumableOptions, SessionLease, SessionState, read_session},
    transfer_io::Output,
};
use super::{
    objects::{self, authorized_bucket, check_publication, lookup},
    system_time_micros, unix_micros,
};
use crate::hashing::Hashes;
use crate::object::{ObjectError, ObjectRecord, UploadChunk};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
use std::io;
use tokio::sync::{mpsc, oneshot, watch};

pub(crate) struct ResumableChunk {
    pub start: u64,
    pub length: u64,
    pub total: Option<u64>,
    pub finalize: bool,
    /// The body length is unknown: `length` only bounds it, and its EOF sets the total.
    pub open_end: bool,
    pub options: ResumableOptions,
    pub catalog_wait: watch::Sender<bool>,
}

use crate::storage::failure::object_unavailable;

#[cfg(test)]
use super::fault::{self, Step};

async fn receive_chunk(
    catalog: &Catalog,
    lease: &SessionLease,
    chunk: &ResumableChunk,
    body: &mut mpsc::Receiver<Result<UploadChunk, io::Error>>,
    cancellation: &mut oneshot::Receiver<()>,
) -> Result<(Output, u64), ObjectError> {
    #[cfg(test)]
    let data_dir = &catalog.0.data_dir;
    let durable = lease.session.offset;
    let mut file = Output::open_readable(catalog, &lease.session.blob_id, durable).await?;
    let closing = catalog.0.admin_shutdown.notified();
    tokio::pin!(closing);
    closing.as_mut().enable();
    let transfer = async {
        let mut received = 0_u64;
        loop {
            if catalog.0.admission.is_closed() { return Err(object_unavailable("catalog is closing")); }
            let message = tokio::select! {
                biased;
                _ = &mut closing => return Err(object_unavailable("catalog is closing")),
                _ = &mut *cancellation => return Err(ObjectError::BodyAborted("request cancelled".into())),
                message = body.recv() => message,
            };
            match message {
                Some(Ok(UploadChunk::Data(bytes))) => {
                    let next = received
                        .checked_add(bytes.len() as u64)
                        .filter(|n| *n <= chunk.length)
                        .ok_or_else(|| {
                            if chunk.open_end {
                                ObjectError::TooLarge
                            } else {
                                ObjectError::BodyAborted(
                                    "Chunk length exceeds Content-Range".into(),
                                )
                            }
                        })?;
                    let discard = durable
                        .saturating_sub(chunk.start + received)
                        .min(bytes.len() as u64) as usize;
                    if discard < bytes.len() {
                        #[cfg(test)]
                        fault::hit(data_dir, Step::BodyWrite).map_err(object_unavailable)?;
                        file.write(&bytes[discard..]).await?;
                    }
                    received = next;
                }
                Some(Ok(UploadChunk::End)) if chunk.open_end || received == chunk.length => {
                    break;
                }
                Some(Err(error)) => return Err(ObjectError::body_aborted(error)),
                _ => {
                    return Err(ObjectError::BodyAborted(
                        "Chunk did not reach complete EOF".into(),
                    ));
                }
            }
        }
        file.finish(catalog).await.map(|()| received)
    }
    .await;
    let received = match transfer {
        Ok(received) => received,
        Err(error) => {
            file.rollback(catalog, durable).await?;
            return Err(error);
        }
    };
    Ok((file, received))
}

/// Inside the publication transaction: the session is still active and unexpired, and its
/// bucket was not recreated since the session started.
fn check_session_publishable(
    transaction: &rusqlite::Transaction<'_>,
    session: &super::resumable::ResumableSession,
    now: std::time::SystemTime,
) -> Result<(), ResumableError> {
    let stored = transaction
        .prepare_cached("SELECT * FROM resumable_sessions WHERE id = ?1")
        .and_then(|mut statement| statement.query_row([&session.id], read_session))
        .map_err(object_unavailable)?;
    if now >= stored.deadline {
        return Err(ResumableError::NotFound);
    }
    if stored.state != SessionState::Active {
        return Err(ResumableError::Gone);
    }
    let incarnation: Option<String> = transaction
        .prepare_cached("SELECT incarnation FROM bucket_incarnations WHERE bucket = ?1")
        .and_then(|mut statement| statement.query_row([&session.bucket], |r| r.get(0)))
        .optional()
        .map_err(object_unavailable)?;
    if incarnation.as_deref() != Some(&session.incarnation) {
        return Err(ObjectError::Forbidden.into());
    }
    Ok(())
}

fn publication_record(
    connection: &rusqlite::Connection,
    session: &super::resumable::ResumableSession,
    options: ResumableOptions,
    generation: i64,
    defaults: objects::PublicationDefaults,
    hashes: Hashes,
    length: u64,
) -> Result<ObjectRecord, ObjectError> {
    let mut record = ObjectRecord {
        owner: defaults.owner,
        bucket: session.bucket.clone(),
        name: session.name.clone(),
        generation,
        metageneration: 1,
        content_type: options.content_type,
        content_encoding: None,
        content_disposition: None,
        content_language: None,
        cache_control: None,
        metadata: Default::default(),
        size: length,
        md5: hashes.md5,
        md5_availability: Default::default(),
        crc32c: hashes.crc32c,
        component_count: None,
        storage_class: defaults.storage_class,
        time_created: defaults.time,
        updated: defaults.time,
        time_deleted: None,
        soft_delete_time: None,
        hard_delete_time: None,
        event_based_hold: defaults.event_based_hold,
        temporary_hold: false,
        custom_time: None,
        retention_expiration_time: None,
        retention: None,
        customer_key_sha256: None,
    };
    options.metadata.apply(&mut record)?;
    let period: Option<i64> = connection
        .prepare_cached("SELECT json_extract(retention_policy, '$.retention_period') FROM buckets WHERE name=?1")
        .and_then(|mut statement| statement.query_row([&record.bucket], |row| row.get(0)))
        .map_err(object_unavailable)?;
    record.retention_expiration_time =
        period.map(|period| defaults.time + std::time::Duration::from_secs(period as u64));
    Ok(record)
}

impl Catalog {
    /// The owned task keeps its lease and body slot until filesystem/SQL cleanup has settled.
    pub(crate) async fn write_resumable_session(
        &self,
        mut lease: SessionLease,
        chunk: ResumableChunk,
        mut body: mpsc::Receiver<Result<UploadChunk, io::Error>>,
        permit: UploadGuard,
        render: impl FnOnce(&ObjectRecord) -> Result<Value, ObjectError> + Send + 'static,
    ) -> Result<Option<Value>, ResumableError> {
        let catalog = self.clone();
        let (_request, mut cancellation) = oneshot::channel::<()>();
        crate::task::spawn_supervised("resumable-chunk", async move {
            let _permit = permit;
            let (file, received) =
                receive_chunk(&catalog, &lease, &chunk, &mut body, &mut cancellation).await?;
            let end = chunk.start + received;
            // An open-ended body completes the object at its EOF. If it ends inside the
            // persisted prefix, the checkpoint rejects the total and nothing was appended.
            let total = if chunk.open_end {
                Some(end)
            } else {
                chunk.total
            };
            let offset = lease.session.offset.max(end);
            #[cfg(test)]
            fault::hit(&catalog.0.data_dir, Step::Checkpoint).map_err(object_unavailable)?;
            chunk.catalog_wait.send_replace(true);
            let file = catalog
                .checkpoint_resumable_output(
                    &mut lease,
                    offset,
                    total,
                    Some(&chunk.options),
                    Some(file),
                )
                .await?
                .ok_or_else(|| object_unavailable("missing upload checkpoint output"))?;
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
        .map_err(object_unavailable)?
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
                    .prepare_cached("SELECT * FROM resumable_sessions WHERE id = ?1")
                    .and_then(|mut statement| statement.query_row([id], read_session))
                    .optional()
                    .map_err(object_unavailable)
            })
            .await?;
        if let Some(session) = committed
            && session.state == SessionState::Complete
        {
            if let Some(memory) = &self.0.memory {
                memory
                    .remove_draft(&lease.session.blob_id)
                    .map_err(object_unavailable)?;
            }
            return Ok(session.completion);
        }
        if matches!(&error, ResumableError::Object(error) if error.is_backend_failure()) {
            if let Some(memory) = &self.0.memory {
                memory
                    .remove(&lease.session.blob_id)
                    .map_err(object_unavailable)?;
                return Err(error);
            }
            let data_dir = self.0.data_dir.clone();
            let blob_id = lease.session.blob_id.clone();
            let offset = lease.session.offset;
            self.run_upload(move |connection| {
                super::resumable_recovery::recover_prefix(connection, &data_dir, &blob_id, offset)
                    .map_err(object_unavailable)
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
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(object_unavailable)?;
            let changed = transaction.prepare_cached("UPDATE resumable_sessions SET state = 'CANCELLED' WHERE id = ?1 AND state = 'ACTIVE'").and_then(|mut statement| statement.execute([id])).map_err(object_unavailable)?;
            if changed == 1 {
                transaction.prepare_cached("INSERT OR IGNORE INTO pending_blob_deletions VALUES (?1)").and_then(|mut statement| statement.execute([&blob_id])).map_err(object_unavailable)?;
            }
            transaction.commit().map_err(object_unavailable)?;
            Ok(changed)
        }).await?;
        if changed == 1 {
            let data_dir = self.0.data_dir.clone();
            let memory = self.0.memory.clone();
            let blob_id = lease.session.blob_id.clone();
            let cleanup = self
                .run_upload(move |connection| {
                    super::resumable_maintenance::clean_session_storage(
                        connection,
                        &data_dir,
                        memory.as_deref(),
                        &blob_id,
                    )
                    .map_err(object_unavailable)
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
        mut file: Output,
        catalog_wait: watch::Sender<bool>,
        render: impl FnOnce(&ObjectRecord) -> Result<Value, ObjectError> + Send + 'static,
    ) -> Result<Value, ResumableError> {
        let (Hashes { md5, crc32c: crc }, length) = file.hashes().await?;
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
        // Every chunk, the final one included, was made durable by `receive_chunk` before its
        // checkpoint, and hashing only reads the file, so there is nothing left to sync.
        drop(file);
        self.publish_resumable_bytes(lease).await?;
        let session = lease.session.clone();
        let clock = self.0.clock.clone();
        let principals = self.0.principals.get().cloned();
        catalog_wait.send_replace(true);
        let publication =
            move |connection: &mut rusqlite::Connection| -> Result<Value, ResumableError> {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(object_unavailable)?;
                check_session_publishable(&transaction, &session, clock.now())?;
                let (storage_class, delete_permission) =
                    authorized_bucket(&transaction, &session.bucket, &options.access)?;
                let storage_class = super::bucket_autoclass::object_storage_class(
                    &transaction,
                    &session.bucket,
                    options.metadata.storage_class.clone(),
                    storage_class,
                )
                .map_err(object_unavailable)?;
                let (versioning, default_hold) =
                    Catalog::bucket_settings_in_transaction(&transaction, &session.bucket)
                        .map_err(object_unavailable)?;
                let previous = lookup(&transaction, &session.bucket, &session.name)?;
                check_publication(
                    previous.as_ref(),
                    options.conditions,
                    delete_permission,
                    clock.now(),
                )?;
                let now = clock.now();
                now.duration_since(std::time::UNIX_EPOCH)
                    .map_err(object_unavailable)?;
                let micros = unix_micros(now).map_err(object_unavailable)?;
                let time = system_time_micros(micros).map_err(object_unavailable)?;
                let defaults = objects::PublicationDefaults {
                    storage_class,
                    time,
                    event_based_hold: default_hold,
                    owner: Some(super::object_owner::for_publication(
                        &transaction,
                        &session.bucket,
                        &options.access,
                    )?),
                };
                let (_, _retired, resource) = objects::publish_in_transaction(
                    &transaction,
                    previous.as_ref(),
                    versioning,
                    objects::NewVersion {
                        blob_id: &session.blob_id,
                        assignment: objects::GenerationAssignment::Automatic,
                        micros,
                        origin: super::change_journal::origin(&options.access),
                        notification_mode: crate::notification::NotificationMode::for_access(
                            &options.access,
                        ),
                        owner_profile: principals.as_ref(),
                        acl: super::acl::PublicationAcl::InheritDefault,
                    },
                    |generation| {
                        let mut record = publication_record(
                            &transaction,
                            &session,
                            options,
                            generation,
                            defaults,
                            Hashes { md5, crc32c: crc },
                            length,
                        )?;
                        if let Some(owner) = &mut record.owner {
                            super::object_owner::canonicalize(owner, principals.as_ref());
                        }
                        let resource = render(&record)?;
                        Ok((record, resource))
                    },
                )?;
                transaction.prepare_cached("UPDATE resumable_sessions SET state = 'COMPLETE', completion = ?1 WHERE id = ?2 AND state = 'ACTIVE'").and_then(|mut statement| statement.execute(params![resource.to_string(),session.id])).map_err(object_unavailable)?;
                objects::commit_publication(transaction)?;
                Ok(resource)
            };
        let result = self
            .run_upload(move |connection| Ok(publication(connection)))
            .await?;
        if result.is_ok() {
            if let Some(memory) = &self.0.memory {
                memory
                    .remove_draft(&lease.session.blob_id)
                    .map_err(object_unavailable)?;
            }
            self.reclaim_after_commit().await;
        }
        result
    }

    async fn publish_resumable_bytes(&self, lease: &SessionLease) -> Result<(), ResumableError> {
        if let Some(memory) = &self.0.memory {
            let memory = memory.clone();
            let blob_id = lease.session.blob_id.clone();
            self.run_upload(move |_| memory.publish_draft(&blob_id).map_err(object_unavailable))
                .await?;
        } else {
            let destination = objects::blob_path(&self.0.data_dir, &lease.session.blob_id)?;
            self.publish_staged(lease.staging_path(), destination)
                .await?;
        }
        Ok(())
    }
}
