use super::write::{MessagePosition, WriteRpc, check_later_key, finished, persisted_answer};
use super::*;
use crate::grpc::v2::stored;
use crate::hashing::Hashes;
use crate::storage::{ChunkEnd, ChunkWait, SessionLease, Written};

impl Storage {
    /// Shares the JSON session-URI cancellation so staged bytes follow one cleanup journal.
    pub(in crate::grpc::v2) async fn cancel_resumable_write(
        &self,
        request: Request<pb::CancelResumableWriteRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::CancelResumableWriteResponse>, Status> {
        let r = request.into_inner();
        let upload = r.upload_id.parse::<UploadId>()?;
        let (bucket, id) = (upload.bucket(), upload.session());
        // gRPC upload IDs are not bearer capabilities, so cancellation requires the
        // same permission as QueryWriteStatus and resumed writes.
        self.access(caller, bucket, OBJECTS_CREATE).await?;
        match self
            .state
            .catalog
            .cancel_resumable_session(id, bucket)
            .await
        {
            // Cancelling a cancelled upload again succeeds (recorded 2026-11-18).
            Ok(()) | Err(ResumableError::Cancelled) => {
                Ok(Response::new(pb::CancelResumableWriteResponse {}))
            }
            Err(ResumableError::Gone) => {
                // A completed object is not an in-progress upload; report that distinctly
                // from a session the emulator ended.
                let finalized = self
                    .state
                    .catalog
                    .resumable_session(id, bucket)
                    .await
                    .is_ok_and(|session| session.state == SessionState::Complete);
                Err(if finalized {
                    // Recorded 2026-11-18.
                    with_reason(
                        Code::FailedPrecondition,
                        "Upload has already been completed.",
                        "GRPC_UPLOAD_ALREADY_COMPLETED",
                    )
                } else {
                    ended_session_status(ResumableError::Gone)
                })
            }
            Err(error) => Err(ended_session_status(error)),
        }
    }
    pub(in crate::grpc::v2) async fn start_resumable_write(
        &self,
        request: Request<pb::StartResumableWriteRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::StartResumableWriteResponse>, Status> {
        let r = request.into_inner();
        let spec = r
            .write_object_spec
            .ok_or_else(|| Status::invalid_argument("write_object_spec is required"))?;
        // The session binds this request's key; its upload_id messages cannot change it.
        let WriteSpec {
            bucket,
            name,
            options,
            total,
        } = self
            .keyed_write_spec(
                spec,
                r.common_object_request_params.as_ref(),
                &DeclaredChecksums::from(r.object_checksums),
                caller,
            )
            .await?;
        let session = self
            .state
            .catalog
            .create_resumable_session(&bucket, &name, options, total)
            .await
            .map_err(session_status)?;
        Ok(Response::new(pb::StartResumableWriteResponse {
            upload_id: UploadId::issued(&bucket, session.id).to_string(),
        }))
    }
    pub(in crate::grpc::v2) async fn query_write_status(
        &self,
        request: Request<pb::QueryWriteStatusRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::QueryWriteStatusResponse>, Status> {
        let r = request.into_inner();
        // Cloud Storage answers whatever key the request carries, even a wrong one, on an open
        // or a finished session (recorded 2026-11-15), so the parameters are not read.
        let upload = r.upload_id.parse::<UploadId>()?;
        let (bucket, id) = (upload.bucket(), upload.session());
        let access = self.access(caller, bucket, OBJECTS_CREATE).await?;
        let session = self
            .state
            .catalog
            .resumable_session(id, bucket)
            .await
            .map_err(ended_session_status)?;
        let allowed = self
            .state
            .catalog
            .has_bucket_permission(bucket, &access)
            .await
            .map_err(ObjectError::from)
            .map_err(object_status)?;
        if !allowed {
            return Err(object_status(ObjectError::Forbidden));
        }
        let response = if session.state == SessionState::Complete {
            // The finished resource carries the caller's ACL and owner view, as the write's
            // own answer does (recorded 2026-11-18).
            let resource = self
                .completed_resource(session.completion.as_ref(), caller, bucket)
                .await?;
            pb::QueryWriteStatusResponse {
                write_status: Some(pb::query_write_status_response::WriteStatus::Resource(
                    resource,
                )),
                persisted_data_checksums: None,
            }
        } else {
            let progress = session.progress().map_err(session_status)?;
            pb::QueryWriteStatusResponse {
                write_status: Some(pb::query_write_status_response::WriteStatus::PersistedSize(
                    crate::grpc::convert::signed(session.offset, "upload session offset")?,
                )),
                persisted_data_checksums: progress.hashes.map(persisted_checksums),
            }
        };
        Ok(Response::new(response))
    }
}

/// QueryWriteStatus's `persisted_data_checksums` for persisted bytes with digests `hashes`. A
/// session an earlier release staged has no running digests and reports none.
fn persisted_checksums(hashes: Hashes) -> pb::ObjectChecksums {
    pb::ObjectChecksums {
        crc32c: Some(hashes.crc32c),
        md5_hash: hashes.md5.to_vec(),
    }
}

impl Storage {
    /// Completion keeps the publication snapshot and adds only the caller's ACL/owner view.
    async fn completed_resource(
        &self,
        completion: Option<&serde_json::Value>,
        caller: Option<Caller<'_>>,
        bucket: &str,
    ) -> Result<pb::Object, Status> {
        let mut resource = stored::from_completion(completion)?;
        self.add_written_owner_fields(caller, bucket, &mut resource)
            .await?;
        Ok(resource)
    }

    /// Completes the session's `options` (already carrying the caller's current access) for one
    /// message of the open session `bucket`/`name`: the conditions rechecked and the message's
    /// checksums merged in, on whichever message they arrive (Cloud Storage accepted them on a
    /// first and on a middle message, recorded 2026-11-18). The key stays the one
    /// `StartResumableWrite` bound, so a refused message stages nothing and leaves the session
    /// usable.
    async fn message_options(
        &self,
        bucket: &str,
        name: &str,
        mut options: ResumableOptions,
        step: &WriteStep,
    ) -> Result<ResumableOptions, Status> {
        check_later_key(options.metadata.customer_key_sha256, step.params.as_ref())?;
        self.state
            .catalog
            .check_upload(bucket, name, options.conditions, &options.access)
            .await
            .map_err(object_status)?;
        options.expected = merge_hashes(options.expected, step.checksums.expected()?)?;
        Ok(options)
    }

    /// Serves one stream of messages for the session `upload` names, starting with `step`.
    /// A later message that names the `upload_id` again is read like any other, as Cloud
    /// Storage reads it (recorded 2026-11-18).
    pub(super) async fn receive_resumable_write<
        S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
    >(
        &self,
        stream: &mut S,
        mut step: WriteStep,
        upload: &UploadId,
        caller: Option<Caller<'_>>,
        rpc: WriteRpc<'_>,
    ) -> Result<pb::BidiWriteObjectResponse, Status> {
        let (bucket, id) = (upload.bucket(), upload.session());
        let mut call = Call::default();
        loop {
            let access = self.access(caller, bucket, OBJECTS_CREATE).await?;
            let lease = self
                .state
                .catalog
                .lock_resumable_session(id, bucket)
                .await
                .map_err(session_status)?;
            if lease.session.state == SessionState::Complete {
                call.position.require_resume_start()?;
                let resource = self
                    .completed_resource(lease.session.completion.as_ref(), caller, bucket)
                    .await?;
                drop(lease);
                let answer = finished(resource);
                rpc.report(&answer).await?;
                return Ok(answer);
            }
            let mut options = lease.session.options.clone();
            options.access = access;
            let options = self
                .message_options(bucket, &lease.session.name, options, &step)
                .await?;
            let end = step.end;
            let persisted = Persisted {
                offset: lease.session.offset,
                total: lease.session.total,
            };
            let piece = match call.admit(step, persisted, self.max_object_bytes) {
                Ok(piece) => piece,
                Err(Refusal::Message(status)) => return Err(status),
                Err(Refusal::Session(status)) => {
                    self.state
                        .catalog
                        .invalidate_resumable_session(&lease)
                        .await
                        .map_err(session_status)?;
                    return Err(status);
                }
            };
            let answer = match self.write_piece(lease, options, piece).await? {
                Written::Complete(value) => finished(
                    self.completed_resource(Some(&value), caller, bucket)
                        .await?,
                ),
                Written::Open(progress) => persisted_answer(
                    progress.offset,
                    progress
                        .hashes
                        .map(|hashes| rpc.persisted_checksums(hashes)),
                )?,
            };
            // finish_write, flush and state_lookup are answered at once. A clean close is
            // answered too, even right after an answered message: "When the stream is closed, a
            // BidiWriteObjectResponse is always sent to the client, regardless of the value of
            // state_lookup."
            if end.finishes() || end.asks_for_status() {
                rpc.report(&answer).await?;
            }
            if end.finishes() {
                return Ok(answer);
            }
            let Some(next) = next_step(stream).await? else {
                call.close()?;
                rpc.report(&answer).await?;
                return Ok(answer);
            };
            step = next;
        }
    }

    /// Persists `piece` under `lease` and finalizes the upload when the piece ends it.
    async fn write_piece(
        &self,
        lease: SessionLease,
        options: ResumableOptions,
        piece: Piece,
    ) -> Result<Written, Status> {
        let (tx, rx) = mpsc::channel(2);
        let (catalog_wait, _wait) = watch::channel(ChunkWait::Transfer);
        let chunk = ResumableChunk {
            start: lease.session.offset,
            length: piece.length,
            end: piece.end,
            options,
            catalog_wait,
        };
        let permit = self
            .state
            .catalog
            .admit_object_upload()
            .map_err(object_status)?;
        let producer = async {
            for part in &piece.parts {
                upload::stage(&tx, part).await.map_err(intake_status)?;
            }
            tx.send(Ok(UploadChunk::End))
                .await
                .map_err(|_| Status::cancelled("Upload consumer stopped"))
        };
        let write = self.state.catalog.write_resumable_session(
            lease,
            chunk,
            rx,
            permit,
            stored::completion,
        );
        let (sent, written) = tokio::join!(producer, write);
        let written = written.map_err(session_status)?;
        sent?;
        Ok(written)
    }
}

/// What a resumable session had persisted when its lease was taken.
#[derive(Clone, Copy)]
pub(super) struct Persisted {
    pub(super) offset: u64,
    /// The session's `object_size`, when its start declared one.
    pub(super) total: Option<u64>,
}

/// Why a resumable message is refused.
pub(super) enum Refusal {
    /// The session stays as it was and another stream can continue it.
    Message(Status),
    /// The refusal ends the session: after a finishing message whose size did not match the
    /// session's `object_size`, Cloud Storage answered QueryWriteStatus with "Upload has
    /// already been terminated." (recorded 2026-11-18).
    Session(Status),
}

/// The bytes one message makes a session persist.
pub(super) struct Piece {
    /// The bytes in order: the unpersisted tail of earlier messages, then this message's.
    pub(super) parts: [bytes::Bytes; 2],
    pub(super) length: u64,
    end: ChunkEnd,
}

/// What one WriteObject or BidiWriteObject call has sent to a resumable session.
///
/// Cloud Storage persists whole granules until `finish_write`: three bytes sent without it
/// left the persisted size at 0, and closing the stream there failed (recorded 2026-11-18).
/// So the bytes past the last whole granule wait in `tail` for the call's next message, and
/// are dropped with the call.
pub(super) struct Call {
    position: MessagePosition,
    /// Received bytes the session has not persisted; always shorter than a granule.
    tail: bytes::Bytes,
    /// The object offset of the first byte of `tail`: the session's persisted size when the
    /// call's last message was admitted.
    tail_start: u64,
    /// The data bytes the call's messages carried.
    uploaded: u64,
}

impl Default for Call {
    fn default() -> Self {
        Self {
            position: MessagePosition::First,
            tail: bytes::Bytes::new(),
            tail_start: 0,
            uploaded: 0,
        }
    }
}

impl Call {
    /// Checks `step` against what its session `persisted` and returns what the session
    /// persists for it. The order is Cloud Storage's on a non-resumable write, the only one
    /// recorded (2026-11-18): the offset, then the size, then the data checksum.
    pub(super) fn admit(
        &mut self,
        step: WriteStep,
        persisted: Persisted,
        max_object_bytes: u64,
    ) -> Result<Piece, Refusal> {
        let Persisted {
            offset: persisted,
            total,
        } = persisted;
        let offset = step.offset.position().map_err(|NegativeOffset| {
            Refusal::Message(Status::out_of_range(
                "write_offset must be nonnegative; query the persisted offset",
            ))
        })?;
        let overflow = || {
            Refusal::Message(Status::out_of_range(
                "write_offset plus content length overflows",
            ))
        };
        self.drop_persisted_tail(persisted);
        let received = u64::try_from(self.tail.len())
            .ok()
            .and_then(|tail| persisted.checked_add(tail))
            .ok_or_else(overflow)?;
        self.check_offset(offset, received)?;
        // Bytes a later message repeats were received already and are not written twice.
        let repeated = usize::try_from(received - offset)
            .unwrap_or(usize::MAX)
            .min(step.data.len());
        let end = u64::try_from(step.data.len() - repeated)
            .ok()
            .and_then(|added| received.checked_add(added))
            .filter(|end| i64::try_from(*end).is_ok())
            .ok_or_else(overflow)?;
        if end > max_object_bytes {
            return Err(Refusal::Message(Status::resource_exhausted(
                "Upload exceeds max_object_bytes; increase --max-object-bytes or write a smaller object",
            )));
        }
        let finish = step.end.finishes();
        if let Some(total) = total {
            if finish && end != total {
                return Err(Refusal::Session(size_mismatch(end, total)));
            }
            if end > total {
                return Err(Refusal::Message(size_mismatch(end, total)));
            }
        }
        let content = step
            .data
            .verify()
            .map_err(|error| Refusal::Message(error.into()))?
            .into_bytes();
        self.uploaded = self
            .uploaded
            .saturating_add(u64::try_from(content.len()).unwrap_or(u64::MAX));
        self.position = MessagePosition::Later;
        let parts = self.take_whole_granules(&content.slice(repeated..), step.end);
        let length = parts
            .iter()
            .try_fold(0_u64, |length, part| {
                length.checked_add(u64::try_from(part.len()).ok()?)
            })
            .ok_or_else(overflow)?;
        self.tail_start = persisted.checked_add(length).ok_or_else(overflow)?;
        Ok(Piece {
            parts,
            length,
            end: ChunkEnd::Exact {
                total: total.or(finish.then_some(end)),
                finalize: finish,
            },
        })
    }

    /// Drops the waiting bytes another stream persisted since this call's last message, so
    /// that `tail` again starts at the session's `persisted` size and no byte is written twice.
    fn drop_persisted_tail(&mut self, persisted: u64) {
        let stale = usize::try_from(persisted.saturating_sub(self.tail_start))
            .unwrap_or(usize::MAX)
            .min(self.tail.len());
        self.tail = self.tail.slice(stale..);
        self.tail_start = persisted;
    }

    /// A stream's first message must start exactly at the persisted size; a later one may
    /// repeat received bytes but not skip ahead.
    fn check_offset(&self, offset: u64, received: u64) -> Result<(), Refusal> {
        let mismatch = || {
            Status::out_of_range(
                "write_offset does not match persisted data; call QueryWriteStatus and resume at persisted_size",
            )
        };
        let refusal = match self.position {
            // Recorded 2026-11-18.
            MessagePosition::First if offset > received => with_reason(
                Code::OutOfRange,
                format!(
                    "Upload request started at offset '{offset}', which is past expected offset '{received}'."
                ),
                "GRPC_UPLOAD_INVALID_START_OFFSET",
            ),
            MessagePosition::First if offset < received => mismatch(),
            MessagePosition::Later if offset > received => mismatch(),
            MessagePosition::First | MessagePosition::Later => return Ok(()),
        };
        Err(Refusal::Message(refusal))
    }

    /// Adds `added` to the unpersisted tail and takes the bytes to persist now: everything on
    /// a finishing message, otherwise the whole granules.
    fn take_whole_granules(&mut self, added: &bytes::Bytes, end: StepEnd) -> [bytes::Bytes; 2] {
        let kept = match end {
            StepEnd::Finish => 0,
            StepEnd::Continue { .. } => (self.tail.len() + added.len()) % WRITE_GRANULE,
        };
        let Some(cut) = added.len().checked_sub(kept) else {
            // Less than a granule in all: nothing to persist yet.
            let mut tail = bytes::BytesMut::with_capacity(kept);
            tail.extend_from_slice(&self.tail);
            tail.extend_from_slice(added);
            self.tail = tail.freeze();
            return [bytes::Bytes::new(), bytes::Bytes::new()];
        };
        // The new tail is copied so that it does not keep the whole message alive.
        let tail = bytes::Bytes::copy_from_slice(&added.slice(cut..));
        [std::mem::replace(&mut self.tail, tail), added.slice(..cut)]
    }

    /// The client closed the stream without `finish_write`: bytes still waiting for a whole
    /// granule are dropped, and Cloud Storage says so (recorded 2026-11-18).
    pub(super) fn close(&self) -> Result<(), Status> {
        if self.tail.is_empty() {
            return Ok(());
        }
        Err(Status::invalid_argument(format!(
            "Exactly {} bytes were uploaded in this call, which is not a multiple of the {WRITE_GRANULE} byte write granularity, but finish_write was not set. Some trailing data was not written. Call 'QueryWriteStatus' to see exactly how much data has been recorded.",
            self.uploaded
        )))
    }
}
