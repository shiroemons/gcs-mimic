use super::{
    Catalog, UploadGuard,
    resumable::{
        Appended, Overrun, Progress, ResumableError, ResumableOptions, SessionLease, SessionState,
        TerminalReason, read_session,
    },
    transfer_io::Output,
};
use super::{
    objects::{self, check_publication},
    system_time_micros, unix_micros,
};
use crate::cause::BackendFailure;
use crate::hashing::{Hasher, Hashes};
use crate::object::{Generation, ObjectError, ObjectRecord, ObjectTarget, UploadChunk};
use crate::storage::catalog::{bucket_incarnation, sql};
use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use std::io;
use tokio::sync::{mpsc, oneshot, watch};

/// What a chunk's request is waiting on: its body and file I/O, which the request deadline
/// bounds, or the catalog, which `catalog_wait_timeout_ms` also bounds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ChunkWait {
    #[default]
    Transfer,
    Catalog,
}

/// Where a chunk's body ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChunkEnd {
    /// The body is exactly `length` bytes. `total` is the object's size once known; the chunk
    /// publishes the object when `finalize` is set.
    Exact { total: Option<u64>, finalize: bool },
    /// The body length is unknown: `length` only bounds it, and its EOF fixes the total and
    /// publishes the object.
    Open,
}

impl ChunkEnd {
    /// The total to checkpoint once `end` bytes are durable.
    fn total(self, end: u64) -> Option<u64> {
        match self {
            Self::Exact { total, .. } => total,
            Self::Open => Some(end),
        }
    }

    fn finalizes(self) -> bool {
        match self {
            Self::Exact { finalize, .. } => finalize,
            Self::Open => true,
        }
    }
}

/// Where a chunk left its session.
#[derive(Debug)]
pub(crate) enum Written {
    /// The upload stays open with this much persisted.
    Open(Progress),
    /// The chunk completed the upload: the stored resource, which retries replay.
    Complete(Value),
}

#[cfg(test)]
impl Written {
    pub(crate) fn into_completion(self) -> Option<Value> {
        match self {
            Self::Complete(resource) => Some(resource),
            Self::Open(_) => None,
        }
    }
}

pub(crate) struct ResumableChunk {
    pub(crate) start: u64,
    pub(crate) length: u64,
    pub(crate) end: ChunkEnd,
    pub(crate) options: ResumableOptions,
    pub(crate) catalog_wait: watch::Sender<ChunkWait>,
}

use crate::storage::failure::object_unavailable;

#[cfg(test)]
use super::fault::{self, Step};

#[cfg(test)]
mod tests;

/// A chunk's body, durable in the session's staged file but not yet checkpointed.
struct Received {
    file: Output,
    /// Body bytes, including any that repeat the persisted prefix.
    length: u64,
    /// The session's running digest continued over the bytes appended to the file.
    running_hash: Option<Hasher>,
}

async fn receive_chunk(
    catalog: &Catalog,
    lease: &SessionLease,
    chunk: &ResumableChunk,
    body: &mut mpsc::Receiver<Result<UploadChunk, io::Error>>,
    cancellation: &mut oneshot::Receiver<()>,
) -> Result<Received, ObjectError> {
    let durable = lease.session.offset;
    let mut running_hash = lease.running_hasher()?;
    let mut file = Output::open_readable(
        catalog,
        &lease.session.blob_id,
        durable,
        lease.guard.clone(),
    )
    .await?;
    let closing = catalog.0.shutdown.token.cancelled();
    tokio::pin!(closing);
    let transfer = async {
        let mut received = 0_u64;
        loop {
            if catalog.is_closing() { return Err(object_unavailable("catalog is closing")); }
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
                            if chunk.end == ChunkEnd::Open {
                                ObjectError::TooLarge
                            } else {
                                ObjectError::BodyAborted(
                                    "Chunk length exceeds Content-Range".into(),
                                )
                            }
                        })?;
                    let discard = usize::try_from(durable
                        .saturating_sub(chunk.start + received)
                        .min(bytes.len() as u64)).map_err(object_unavailable)?;
                    if discard < bytes.len() {
                        let appended = bytes.get(discard..).ok_or_else(|| object_unavailable("invalid resumable discard offset"))?;
                        if let Some(hasher) = &mut running_hash {
                            hasher.update(appended);
                        }
                        file.write(appended).await?;
                    }
                    received = next;
                }
                Some(Ok(UploadChunk::End))
                    if chunk.end == ChunkEnd::Open || received == chunk.length =>
                {
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
        file.finish().await.map(|()| received)
    }
    .await;
    let length = match file.settle_result(transfer).await {
        Ok(length) => length,
        Err(error) => match file.rollback(durable).await? {
            objects::Rollback::Truncated | objects::Rollback::Deferred => return Err(error),
        },
    };
    Ok(Received {
        file,
        length,
        running_hash,
    })
}

/// Inside the publication transaction: the session is still active and unexpired, and its
/// bucket was not recreated since the session started.
fn check_session_publishable(
    transaction: &sql::WriteTx<'_>,
    session: &super::resumable::ResumableSession,
    now: std::time::SystemTime,
) -> Result<(), ResumableError> {
    let stored = sql::row(
        transaction,
        super::resumable::SELECT_SESSION,
        [&session.id],
        read_session,
    )
    .map_err(object_unavailable)?;
    if now >= stored.deadline {
        return Err(ResumableError::NotFound);
    }
    if stored.state != SessionState::Active {
        return Err(ResumableError::Gone);
    }
    let incarnation = bucket_incarnation(transaction, &session.bucket)
        .optional()
        .map_err(object_unavailable)?;
    if incarnation != Some(session.incarnation) {
        return Err(ObjectError::Forbidden.into());
    }
    Ok(())
}

fn publication_record(
    connection: &rusqlite::Connection,
    session: &super::resumable::ResumableSession,
    options: ResumableOptions,
    generation: Generation,
    defaults: objects::PublicationDefaults,
    hashes: Hashes,
    length: u64,
) -> Result<ObjectRecord, ObjectError> {
    let time = defaults.time;
    let mut record = defaults.record(
        ObjectTarget::new(&session.bucket, &session.name, generation),
        options.content_type,
        length,
        hashes,
    );
    options.metadata.reject_past_retention(time)?;
    options.metadata.apply(&mut record)?;
    record.retention_expiration_time = objects::retention_expiration(connection, &record)?;
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
    ) -> Result<Written, ResumableError> {
        let catalog = self.clone();
        let (_request, mut cancellation) = oneshot::channel::<()>();
        crate::task::spawn_supervised("resumable-chunk", async move {
            let _permit = permit;
            let Received {
                file,
                length,
                running_hash,
            } = receive_chunk(&catalog, &lease, &chunk, &mut body, &mut cancellation).await?;
            let end = chunk.start + length;
            // An open-ended body completes the object at its EOF. If it ends inside the
            // persisted prefix, the checkpoint rejects the total and nothing was appended.
            let total = chunk.end.total(end);
            let offset = lease.session.offset.max(end);
            #[cfg(test)]
            fault::hit(&catalog.0.storage.data_dir, Step::Checkpoint)
                .map_err(object_unavailable)?;
            chunk.catalog_wait.send_replace(ChunkWait::Catalog);
            let appended = Appended {
                output: file,
                running_hash: running_hash.as_ref().map(Hasher::save),
            };
            let file = catalog
                .checkpoint_resumable_output(
                    &mut lease,
                    offset,
                    total,
                    Some(&chunk.options),
                    Some(appended),
                )
                .await?
                .ok_or_else(|| object_unavailable("missing upload checkpoint output"))?;
            chunk.catalog_wait.send_replace(ChunkWait::Transfer);
            if !chunk.end.finalizes() {
                return lease.session.progress().map(Written::Open);
            }
            // Completion digests the staged file itself rather than trusting the running
            // digest: the stored checksums must describe the bytes being published.
            let result = catalog
                .finalize_resumable(&lease, chunk.options, file, chunk.catalog_wait, render)
                .await;
            catalog
                .resolve_resumable_finalization(&lease, result)
                .await
                .map(Written::Complete)
        })
        .await
        .map_err(object_unavailable)?
    }

    async fn resolve_resumable_finalization(
        &self,
        lease: &SessionLease,
        result: Result<Value, ResumableError>,
    ) -> Result<Value, ResumableError> {
        let error = match result {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let id = lease.session.id;
        let committed = self
            .run_upload(move |connection| {
                sql::row(
                    connection,
                    super::resumable::SELECT_SESSION,
                    [id],
                    read_session,
                )
                .optional()
                .map_err(object_unavailable)
            })
            .await?;
        if let Some(session) = committed
            && let SessionState::Complete(completion) = session.state
        {
            self.0
                .storage
                .backend
                .finish_draft(&lease.session.blob_id)
                .map_err(object_unavailable)?;
            return completion
                .into_value()
                .ok_or_else(|| object_unavailable(BackendFailure::MissingCompletion).into());
        }
        if matches!(&error, ResumableError::Object(error) if error.is_backend_failure()) {
            if self.0.storage.backend.is_memory() {
                self.0
                    .storage
                    .backend
                    .remove(&lease.session.blob_id)
                    .map_err(object_unavailable)?;
                return Err(error);
            }
            let data_dir = self.0.storage.data_dir.clone();
            let blob_id = lease.session.blob_id;
            let offset = lease.session.offset;
            self.run_upload(move |connection| {
                super::resumable_recovery::check_unreferenced(connection, &blob_id)
                    .map_err(object_unavailable)
            })
            .await?;
            crate::task::run_blocking("resumable-recovery", move || {
                super::resumable_recovery::recover_file_prefix(&data_dir, &blob_id, offset)
            })
            .await
            .map_err(object_unavailable)?
            .map_err(object_unavailable)?;
        } else {
            self.invalidate_resumable_session(lease).await?;
        }
        Err(error)
    }

    pub(crate) async fn invalidate_resumable_session(
        &self,
        lease: &SessionLease,
    ) -> Result<(), ResumableError> {
        self.fail_resumable_session(lease, TerminalReason::Aborted)
            .await
    }

    pub(crate) async fn reject_resumable_content_range(
        &self,
        lease: &SessionLease,
    ) -> Result<(), ResumableError> {
        self.fail_resumable_session(lease, TerminalReason::ContentRange)
            .await
    }

    /// Ends the session on a chunk past its total; later requests repeat `overrun`.
    pub(crate) async fn reject_resumable_overrun(
        &self,
        lease: &SessionLease,
        overrun: Overrun,
    ) -> Result<(), ResumableError> {
        self.fail_resumable_session(lease, TerminalReason::Overrun(overrun))
            .await
    }

    async fn fail_resumable_session(
        &self,
        lease: &SessionLease,
        reason: TerminalReason,
    ) -> Result<(), ResumableError> {
        let wake = self.0.maintenance.wake.clone();
        let id = lease.session.id;
        let blob_id = lease.session.blob_id;
        #[cfg(test)]
        let data_dir = self.0.storage.data_dir.clone();
        let changed = self.run_upload::<_, ObjectError>(move |connection| {
            let transaction = sql::immediate(connection)?;
            let changed = sql::exec(&transaction, "UPDATE resumable_sessions SET state = 'CANCELLED', options = json_set(options, '$.terminal_error', json(?2)) WHERE id = ?1 AND state = 'ACTIVE'", params![id, sql::Json(reason)])?;
            if changed == 1 {
                objects::journal_deletion(&transaction, &blob_id)?;
            }
            #[cfg(test)]
            fault::hit(&data_dir, Step::SessionFailureCommit).map_err(object_unavailable)?;
            super::resumable_maintenance::commit_cancellation(transaction, changed, &wake)?;
            #[cfg(test)]
            fault::hit(&data_dir, Step::SessionFailureCommitted).map_err(object_unavailable)?;
            Ok(changed)
        }).await?;
        if changed == 1 {
            let blob_id = lease.session.blob_id;
            let cleanup = self.clean_session_storage(&lease.guard, blob_id).await;
            if let Err(error) = cleanup {
                crate::diagnostics::emit(
                    crate::diagnostics::LogLevel::Warn,
                    crate::diagnostics::Event::UploadCleanupDeferred,
                    serde_json::json!({"message":format!("invalid upload cleanup failed; maintenance will retry: {error}")}),
                );
                self.record_maintenance_error(Some(crate::cause::BackendCause::from_error(error)));
            }
        }
        Ok(())
    }

    async fn finalize_resumable(
        &self,
        lease: &SessionLease,
        options: ResumableOptions,
        file: Output,
        catalog_wait: watch::Sender<ChunkWait>,
        render: impl FnOnce(&ObjectRecord) -> Result<Value, ObjectError> + Send + 'static,
    ) -> Result<Value, ResumableError> {
        let (hashes, length) = file.into_hashes().await?;
        options
            .expected
            .verify(&hashes)
            .map_err(ObjectError::from)?;
        if Some(length) != lease.session.total || length != lease.session.offset {
            return Err(
                ObjectError::BodyAborted("Final length does not match total".into()).into(),
            );
        }
        // Every chunk, the final one included, was made durable by `receive_chunk` before its
        // checkpoint, and hashing only reads the file, so there is nothing left to sync.
        self.0
            .storage
            .backend
            .publish_resumable(self, lease.session.blob_id)
            .await?;
        let session = lease.session.clone();
        let clock = self.0.policy.clock.clone();
        let principals = self.0.policy.principals.clone();
        catalog_wait.send_replace(ChunkWait::Catalog);
        let publication =
            move |connection: &mut rusqlite::Connection| -> Result<(Value, objects::Retired), ResumableError> {
                let transaction = sql::immediate(connection).map_err(object_unavailable)?;
                check_session_publishable(&transaction, &session, clock.now())?;
                let authority = objects::BucketAuthority::load(&transaction, &session.bucket, &options.access)?;
                let bucket = authority.publication?;
                let storage_class = bucket.storage_class(options.metadata.storage_class)?;
                let previous = bucket.lookup(&transaction, session.key())?;
                check_publication(
                    previous.as_ref(),
                    options.conditions,
                    authority.overwrite,
                    clock.now(),
                )?;
                let now = clock.now();
                now.duration_since(std::time::UNIX_EPOCH)
                    .map_err(object_unavailable)?;
                let micros = unix_micros(now)?;
                let time = system_time_micros(micros).map_err(object_unavailable)?;
                let defaults = objects::PublicationDefaults {
                    storage_class,
                    time,
                    event_based_hold: bucket.default_event_hold,
                    owner: Some(bucket.owner(&options.access, principals.as_ref())?),
                };
                let objects::PublishedVersion {
                    retired,
                    prepared: resource,
                    ..
                } = objects::publish_in_transaction(
                    &transaction,
                    previous.as_ref(),
                    &bucket,
                    objects::NewVersion {
                        blob_id: session.blob_id,
                        assignment: objects::GenerationAssignment::Automatic,
                        micros,
                        notification_mode: crate::notification::NotificationMode::for_access(
                            &options.access,
                        ),
                        owner_profile: principals.as_ref(),
                        acl: super::acl::PublicationAcl::requested(&options.metadata.acl),
                    },
                    |generation| {
                        let mut record = publication_record(
                            &transaction,
                            &session,
                            options,
                            generation,
                            defaults,
                            hashes,
                            length,
                        )?;
                        if let Some(owner) = &mut record.owner {
                            super::object_owner::canonicalize(owner, principals.as_ref());
                        }
                        let resource = render(&record)?;
                        Ok((record, resource))
                    },
                )?;
                sql::exec(&transaction, "UPDATE resumable_sessions SET state = 'COMPLETE', completion = ?1 WHERE id = ?2 AND state = 'ACTIVE'", params![resource.to_string(),session.id]).map_err(object_unavailable)?;
                objects::commit_publication(transaction)?;
                Ok((resource, retired))
            };
        let (resource, retired) = self
            .run_upload::<_, ObjectError>(move |connection| Ok(publication(connection)))
            .await??;
        self.schedule_expiry(retired);
        self.0
            .storage
            .backend
            .finish_draft(&lease.session.blob_id)
            .map_err(object_unavailable)?;
        self.reclaim_if(retired.into()).await;
        Ok(resource)
    }
}
