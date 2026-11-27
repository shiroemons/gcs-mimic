use super::step::AdmitError;
use super::write::{MessagePosition, WriteRpc, check_later_key, finished, persisted_answer};
use super::*;
use crate::bucket::BucketAccess;
use crate::grpc::v2::{routing::Admitted, stored};
use crate::hashing::Hashes;
use crate::storage::{ChunkEnd, ChunkWait, SessionLease, Written};
use bytes::{Buf, Bytes, BytesMut};

impl Storage {
    /// Authorizes `caller` to write to `bucket`, the bucket an upload ID names, and then
    /// compares the call's routing with it ([`Admitted::compare_upload`], which states the
    /// order). gRPC upload IDs are not bearer capabilities, so QueryWriteStatus,
    /// CancelResumableWrite and a resumed write all require the permission to create objects.
    pub(super) async fn authorize_upload(
        &self,
        caller: Option<Caller<'_>>,
        admitted: &Admitted,
        bucket: &str,
    ) -> Result<BucketAccess, Status> {
        let access = self.access(caller, bucket, OBJECTS_CREATE).await?;
        admitted.compare_upload(bucket)?;
        Ok(access)
    }

    /// Shares the JSON session-URI cancellation so staged bytes follow one cleanup journal.
    pub(in crate::grpc::v2) async fn cancel_resumable_write(
        &self,
        request: Request<pb::CancelResumableWriteRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::CancelResumableWriteResponse>, Status> {
        let (_, extensions, r) = request.into_parts();
        let upload = r.upload_id.parse::<UploadId>()?;
        let (bucket, id) = (upload.bucket(), upload.session());
        self.authorize_upload(caller, Admitted::of(&extensions)?, bucket)
            .await?;
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
                    .is_ok_and(|session| session.state.is_complete());
                Err(if finalized {
                    // Recorded 2026-11-18.
                    with_reason(
                        Code::FailedPrecondition,
                        "Upload has already been completed.",
                        "GRPC_UPLOAD_ALREADY_COMPLETED",
                    )
                } else {
                    // Recorded (case grpc-probe-inferred-session-states); see session_status.
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
            target,
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
            .create_resumable_session(target.key(), options, total)
            .await
            .map_err(session_status)?;
        Ok(Response::new(pb::StartResumableWriteResponse {
            upload_id: UploadId::issued(target.bucket.as_str(), session.id).to_string(),
        }))
    }
    pub(in crate::grpc::v2) async fn query_write_status(
        &self,
        request: Request<pb::QueryWriteStatusRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::QueryWriteStatusResponse>, Status> {
        let (_, extensions, r) = request.into_parts();
        // Cloud Storage answers whatever key the request carries, even a wrong one, on an open
        // or a finished session (recorded 2026-11-15), so the parameters are not read.
        let upload = r.upload_id.parse::<UploadId>()?;
        let (bucket, id) = (upload.bucket(), upload.session());
        let access = self
            .authorize_upload(caller, Admitted::of(&extensions)?, bucket)
            .await?;
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
        let response = if let SessionState::Complete(completion) = &session.state {
            // The finished resource carries the caller's ACL and owner view, as the write's
            // own answer does (recorded 2026-11-18).
            let resource = self
                .completed_resource(stored::from_stored_completion(completion)?, caller, bucket)
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
        mut resource: pb::Object,
        caller: Option<Caller<'_>>,
        bucket: &str,
    ) -> Result<pb::Object, Status> {
        self.add_written_owner_fields(caller, bucket, &mut resource)
            .await?;
        Ok(resource)
    }

    /// Completes the session's `options` (already carrying the caller's current access) for one
    /// admitted message of the open session `lease` holds: the conditions rechecked and, when
    /// the message's `end` finishes the upload, the complete object checked against `expected`,
    /// the checksums the session and its call declared ([`Piece::verify`]). A refused message
    /// stages nothing and leaves the session usable.
    ///
    /// The conditions answer after what the message carries: recorded (case
    /// grpc-probe-inferred-session-states), a wrong data checksum, a first offset past the
    /// persisted size and a finishing size past `object_size` each answer before a failing
    /// condition, the order recorded on a non-resumable write's first message (2026-11-18). The
    /// complete object's checksums answer after the conditions. Not recorded on a resumable
    /// write; on a non-resumable one the conditions are checked before any byte is staged and the
    /// complete object after the last.
    async fn message_options(
        &self,
        lease: &SessionLease,
        mut options: ResumableOptions,
        expected: ExpectedHashes,
        piece: &Piece,
        end: StepEnd,
    ) -> Result<ResumableOptions, Status> {
        let session = &lease.session;
        self.state
            .catalog
            .check_upload(session.key(), options.conditions, &options.access)
            .await
            .map_err(object_status)?;
        if end.finishes() {
            piece.verify(lease, expected)?;
            options.expected = expected;
        }
        Ok(options)
    }

    /// Serves one stream of messages for the session `upload` names, starting with `step`.
    /// A later message that names the `upload_id` again is read like any other, as Cloud
    /// Storage reads it (recorded 2026-11-18). The caller is authorized for the session's bucket
    /// before each message, and before the first the stream's routing is compared with that
    /// bucket ([`Admitted::compare_upload`]).
    ///
    /// Each message is checked in this order: the key `StartResumableWrite` bound, the message
    /// against what the session persisted ([`Call::admit`]: a resumable stream's offset rules,
    /// then the ones it shares with a non-resumable write, [`WriteStep::admit`]: offset, size,
    /// data checksum and object checksums), the object checksums `StartResumableWrite` declared
    /// against the call's ([`Call::expected`]), then the conditions and the complete object
    /// ([`Self::message_options`]). Recorded (case grpc-probe-inferred-session-states): a first
    /// offset past the persisted size answers before a malformed `md5_hash`. Not recorded: the
    /// key's place, first as on a non-resumable write's later message, and a malformed
    /// `md5_hash` against the size, the data checksum and the conditions.
    pub(super) async fn receive_resumable_write<
        S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
    >(
        &self,
        stream: &mut S,
        mut step: WriteStep,
        upload: &UploadId,
        caller: Option<Caller<'_>>,
        rpc: WriteRpc<'_>,
        admitted: &Admitted,
    ) -> Result<pb::BidiWriteObjectResponse, Status> {
        let (bucket, id) = (upload.bucket(), upload.session());
        let mut call = Call::default();
        // The routing is compared once, after the first authorization; each later message is
        // authorized again before it is taken.
        let mut access = self.authorize_upload(caller, admitted, bucket).await?;
        loop {
            let lease = self
                .state
                .catalog
                .lock_resumable_session(id, bucket)
                .await
                .map_err(session_status)?;
            if let SessionState::Complete(completion) = &lease.session.state {
                call.position.require_resume_start()?;
                let resource = self
                    .completed_resource(stored::from_stored_completion(completion)?, caller, bucket)
                    .await?;
                drop(lease);
                let answer = finished(resource);
                rpc.report(&answer).await?;
                return Ok(answer);
            }
            let mut options = lease.session.options.clone();
            options.access = access;
            check_later_key(options.metadata.customer_key_sha256, step.params.as_ref())?;
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
            let expected = call.expected(options.expected)?;
            let options = self
                .message_options(&lease, options, expected, &piece, end)
                .await?;
            let written = self.write_piece(lease, options, &mut call, piece).await?;
            let answer = match written {
                Written::Complete(value) => finished(
                    self.completed_resource(stored::from_completion(Some(&value))?, caller, bucket)
                        .await?,
                ),
                Written::Open(progress) => {
                    persisted_answer(progress.offset, progress.hashes.map(|hashes| hashes.crc32c))?
                }
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
            access = self.access(caller, bucket, OBJECTS_CREATE).await?;
        }
    }

    /// Persists the complete granules and returns any trailing bytes to the call on success.
    /// A finishing piece publishes the object and returns `Written::Complete`.
    async fn write_piece(
        &self,
        lease: SessionLease,
        options: ResumableOptions,
        call: &mut Call,
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
        call.persisted(piece);
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
    /// The refusal ends the session, and QueryWriteStatus then answers "Upload has already been
    /// terminated.": recorded after a finishing message whose size was below the session's
    /// `object_size` (2026-11-18), and after a message without `finish_write` whose bytes went
    /// past it (case grpc-probe-inferred-granules). Not recorded; inferred from those answers:
    /// a finishing message past `object_size` ends the session too. Cloud Storage's refusal of
    /// it was recorded, but not the session's state after it.
    Session(Status),
}

/// The bytes one message makes a session persist.
pub(super) struct Piece {
    /// The bytes in order: the unpersisted tail of earlier messages, then this message's.
    pub(super) parts: [bytes::Bytes; 2],
    /// Still in the incoming frame while the writer owns slices of the tail buffer.
    remainder: Bytes,
    pub(super) length: u64,
    end: ChunkEnd,
}

impl Piece {
    /// Checks the object this finishing piece completes on the session `lease` holds against
    /// `expected` before anything is written, from the session's digest of its persisted bytes
    /// continued over the piece.
    ///
    /// Recorded (case grpc-probe-inferred-session-states): a finishing message whose object
    /// misses a CRC32C an earlier message of its stream declared is refused with
    /// INVALID_ARGUMENT, and the session stays open at its persisted size, without the
    /// message's bytes. Not recorded; inferred from that answer: a checksum the finishing message
    /// or `StartResumableWrite` declared, and an MD5, are refused the same way.
    ///
    /// A session an earlier release staged has no running digest, so nothing is checked here.
    /// The catalog then checks the staged object as it publishes it, and its refusal differs
    /// from the recorded one: it ends the session, and it keeps the catalog's wording ("Provided
    /// CRC32C ... doesn't match calculated CRC32C ...") instead of "Object crc32c mismatch".
    fn verify(&self, lease: &SessionLease, expected: ExpectedHashes) -> Result<(), Status> {
        if expected == ExpectedHashes::default() {
            return Ok(());
        }
        let Some(hashes) = lease.digest_with(&self.parts).map_err(session_status)? else {
            return Ok(());
        };
        upload::verify_object(&expected, &hashes).map_err(intake_status)
    }
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
    tail: BytesMut,
    /// The object offset of the first byte of `tail`: the session's persisted size when the
    /// call's last message was admitted.
    tail_start: u64,
    /// The data bytes the call's messages carried.
    uploaded: u64,
    /// The `object_checksums` the call's messages declared, which hold for the call alone:
    /// recorded (case grpc-probe-inferred-session-states), a stream that declared a CRC32C
    /// without finishing left the session to accept a later stream's object that the CRC32C
    /// does not match.
    declared: ExpectedHashes,
}

impl Default for Call {
    fn default() -> Self {
        Self {
            position: MessagePosition::First,
            tail: BytesMut::new(),
            tail_start: 0,
            uploaded: 0,
            declared: ExpectedHashes::default(),
        }
    }
}

impl Call {
    /// What the complete object must match: the checksums the call's messages declared, on
    /// whichever message they arrived (Cloud Storage accepted them on a first and on a middle
    /// message, recorded 2026-11-18), and the `session`'s, which `StartResumableWrite` declared
    /// and which hold for every stream of the session.
    pub(super) fn expected(&self, session: ExpectedHashes) -> Result<ExpectedHashes, Status> {
        merge_hashes(session, self.declared)
    }

    /// Checks `step` against what its session `persisted` and returns what the session
    /// persists for it: the offset a stream starts at here, then the rules a resumable write
    /// shares with a non-resumable one ([`WriteStep::admit`]), which also add the message's
    /// object checksums to the call's.
    ///
    /// A finishing message whose bytes all lie inside the persisted ones adds nothing, as
    /// [`upload::admit_bytes`] skips repeated bytes. Not recorded; the emulator's choice, which
    /// follows from that skip: without an `object_size` the object is finalized at the persisted
    /// size, and with one that differs from the persisted size the message misses it and ends
    /// the session, as any finishing message short of `object_size` does (see
    /// [`Refusal::Session`]).
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
        let overflow = || Refusal::Message(intake_status(IntakeError::Overflow));
        self.drop_persisted_tail(persisted);
        let received = u64::try_from(self.tail.len())
            .ok()
            .and_then(|tail| persisted.checked_add(tail))
            .ok_or_else(overflow)?;
        self.check_first_offset(offset, received)?;
        let admitted = step
            .admit(offset, self.declared, |piece| {
                let (added, written) =
                    upload::admit_bytes(received, piece, max_object_bytes, total)?;
                Ok((added, written, piece.content.len()))
            })
            .map_err(refusal)?;
        let (added, written, sent) = admitted.bytes;
        let end = admitted.end;
        self.declared = admitted.declared;
        self.uploaded = self
            .uploaded
            .saturating_add(u64::try_from(sent).unwrap_or(u64::MAX));
        self.position = MessagePosition::Later;
        let (parts, remainder) = self.take_whole_granules(added, end);
        let length = parts
            .iter()
            .try_fold(0_u64, |length, part| {
                length.checked_add(u64::try_from(part.len()).ok()?)
            })
            .ok_or_else(overflow)?;
        self.tail_start = persisted.checked_add(length).ok_or_else(overflow)?;
        let finish = end.finishes();
        Ok(Piece {
            parts,
            remainder,
            length,
            end: ChunkEnd::Exact {
                total: total.or_else(|| finish.then_some(written)),
                finalize: finish,
            },
        })
    }

    /// Drops the waiting bytes another stream persisted since this call's last message, so
    /// that `tail` again starts at the session's `persisted` size and no byte is written twice.
    /// Like repeated bytes ([`upload::admit_bytes`]), they are not compared with the persisted
    /// ones, which stay.
    fn drop_persisted_tail(&mut self, persisted: u64) {
        let stale = usize::try_from(persisted.saturating_sub(self.tail_start))
            .unwrap_or(usize::MAX)
            .min(self.tail.len());
        self.tail.advance(stale);
        self.tail_start = persisted;
    }

    /// A stream's first message may not start past the persisted size, and is refused in its
    /// own wording. Like every later message, it may repeat persisted bytes, which are skipped
    /// without being compared with the persisted ones ([`upload::admit_bytes`]).
    ///
    /// Recorded 2026-11-18: a first offset past the persisted size, with nothing persisted.
    /// Recorded (case grpc-probe-inferred-granules): with one granule persisted, a first offset
    /// past it is refused the same way, and one at 0 or inside it is accepted without changing
    /// the persisted size, as the WriteObject description lets a client resend persisted bytes.
    fn check_first_offset(&self, offset: u64, received: u64) -> Result<(), Refusal> {
        match self.position {
            MessagePosition::First if offset > received => Err(Refusal::Message(with_reason(
                Code::OutOfRange,
                format!(
                    "Upload request started at offset '{offset}', which is past expected offset '{received}'."
                ),
                "GRPC_UPLOAD_INVALID_START_OFFSET",
            ))),
            MessagePosition::First | MessagePosition::Later => Ok(()),
        }
    }

    /// Adds `added` to the unpersisted tail and takes the bytes to persist now: everything on
    /// a finishing message, otherwise the whole granules.
    fn take_whole_granules(&mut self, mut added: Bytes, end: StepEnd) -> ([Bytes; 2], Bytes) {
        let length = self.tail.len() + added.len();
        let kept = match end {
            StepEnd::Finish => 0,
            StepEnd::Continue { .. } if length < WRITE_GRANULE => {
                self.append_tail(&added);
                return ([Bytes::new(), Bytes::new()], Bytes::new());
            }
            StepEnd::Continue { .. } => length % WRITE_GRANULE,
        };
        let prefix = added.split_to(added.len() - kept);
        let prior = if self.tail.is_empty() {
            Bytes::new()
        } else {
            self.tail.split().freeze()
        };
        ([prior, prefix], added)
    }

    pub(super) fn persisted(&mut self, piece: Piece) {
        drop(piece.parts);
        self.append_tail(&piece.remainder);
    }

    fn append_tail(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.tail.is_empty() {
            // Allocate on the first partial granule, then reclaim it after the catalog write.
            self.tail.reserve(WRITE_GRANULE);
        }
        self.tail.extend_from_slice(bytes);
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

/// How a resumable message that [`WriteStep::admit`] refuses leaves its session. Bytes that
/// miss `object_size`, past it or short of it at `finish_write`, end it (see
/// [`Refusal::Session`]). A later message that skips past the received bytes is told to resume
/// where the session stands, the remedy that fits a resumable write; every other refusal keeps
/// the wording all write paths share and leaves the session usable, as a refused data checksum
/// left it open at its persisted size (recorded 2026-11-18).
fn refusal(error: AdmitError) -> Refusal {
    match error {
        AdmitError::Intake(error @ IntakeError::SizeMismatch { .. }) => {
            Refusal::Session(intake_status(error))
        }
        AdmitError::Intake(IntakeError::Offset) => Refusal::Message(Status::out_of_range(
            "write_offset does not match persisted data; call QueryWriteStatus and resume at persisted_size",
        )),
        error @ (AdmitError::Intake(
            IntakeError::ChunkCrc(_)
            | IntakeError::Overflow
            | IntakeError::TooLarge
            | IntakeError::ObjectChecksum(_)
            | IntakeError::ConflictingChecksums
            | IntakeError::ConsumerStopped,
        )
        | AdmitError::Message(_)) => Refusal::Message(error.into()),
    }
}
