use super::write::{MessagePosition, WriteRpc, check_later_key, message_after_finish};
use super::*;
use crate::grpc::v2::stored;
use crate::storage::{ChunkEnd, ChunkWait, Written};

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
            Ok(()) => Ok(Response::new(pb::CancelResumableWriteResponse {})),
            Err(ResumableError::Gone) => {
                // A completed object is not an in-progress upload; report that distinctly
                // from a cancelled or expired session, matching resumed WriteObject.
                let finalized = self
                    .state
                    .catalog
                    .resumable_session(id, bucket)
                    .await
                    .is_ok_and(|session| session.state == SessionState::Complete);
                Err(if finalized {
                    Status::failed_precondition(
                        "Upload already finalized; delete the object instead of cancelling",
                    )
                } else {
                    session_status(ResumableError::Gone)
                })
            }
            Err(error) => Err(session_status(error)),
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
                r.object_checksums.as_ref(),
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
            .map_err(session_status)?;
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
        let status = if session.state == SessionState::Complete {
            pb::query_write_status_response::WriteStatus::Resource(stored::from_completion(
                session.completion.as_ref(),
            )?)
        } else {
            pb::query_write_status_response::WriteStatus::PersistedSize(
                crate::grpc::convert::signed(session.offset, "upload session offset")?,
            )
        };
        Ok(Response::new(pb::QueryWriteStatusResponse {
            write_status: Some(status),
            ..Default::default()
        }))
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
    /// final checksums merged in. The key stays the one `StartResumableWrite` bound, so a
    /// refused message stages nothing and leaves the session usable.
    async fn message_options(
        &self,
        bucket: &str,
        name: &str,
        mut options: ResumableOptions,
        message: &pb::BidiWriteObjectRequest,
    ) -> Result<ResumableOptions, Status> {
        check_later_key(
            options.metadata.customer_key_sha256,
            message.common_object_request_params.as_ref(),
        )?;
        self.state
            .catalog
            .check_upload(bucket, name, options.conditions, &options.access)
            .await
            .map_err(object_status)?;
        options.expected =
            merge_hashes(options.expected, hashes(message.object_checksums.as_ref())?)?;
        Ok(options)
    }

    pub(super) async fn receive_resumable_write<
        S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
    >(
        &self,
        stream: &mut S,
        mut message: pb::BidiWriteObjectRequest,
        upload: &UploadId,
        caller: Option<Caller<'_>>,
        rpc: WriteRpc<'_>,
    ) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
        use pb::bidi_write_object_response::WriteStatus;
        let (bucket, id) = (upload.bucket(), upload.session());
        let mut position = MessagePosition::First;
        loop {
            check_resumable_message(&message, caller)?;
            let access = self.access(caller, bucket, OBJECTS_CREATE).await?;
            let trailing = read_past_finish(stream, &message, rpc).await;
            let lease = self
                .state
                .catalog
                .lock_resumable_session(id, bucket)
                .await
                .map_err(session_status)?;
            if lease.session.state == SessionState::Complete {
                position.require_resume_start()?;
                let resource = self
                    .completed_resource(lease.session.completion.as_ref(), caller, bucket)
                    .await?;
                drop(lease);
                let result = WriteStatus::Resource(resource);
                rpc.report(&result).await?;
                return Ok(result);
            }
            let mut options = lease.session.options.clone();
            options.access = access;
            let options = self
                .message_options(bucket, &lease.session.name, options, &message)
                .await?;
            let persisted = Persisted {
                offset: lease.session.offset,
                total: lease.session.total,
            };
            let data = take_data(&mut message);
            let ResumablePiece {
                offset,
                written,
                length,
            } = resumable_piece(&message, &data, persisted, position, self.max_object_bytes)?;
            trailing?;
            let (tx, rx) = mpsc::channel(2);
            let (catalog_wait, _wait) = watch::channel(ChunkWait::Transfer);
            let chunk = ResumableChunk {
                start: offset,
                length,
                end: ChunkEnd::Exact {
                    total: persisted.total.or(message.finish_write.then_some(written)),
                    finalize: message.finish_write,
                },
                options,
                catalog_wait,
            };
            let permit = self
                .state
                .catalog
                .admit_object_upload()
                .map_err(object_status)?;
            let producer = async {
                stage(&tx, &data.content).await?;
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
            let (sent, written_record) = tokio::join!(producer, write);
            let written_record = written_record.map_err(session_status)?;
            sent?;
            let result = match written_record {
                Written::Complete(value) => {
                    let resource = self
                        .completed_resource(Some(&value), caller, bucket)
                        .await?;
                    WriteStatus::Resource(resource)
                }
                Written::Open(_) => WriteStatus::PersistedSize(crate::grpc::convert::signed(
                    written,
                    "persisted_size",
                )?),
            };
            // The proto answers finish_write, flush and state_lookup, and a clean close after a
            // message none of them answered.
            let answered = message.finish_write || message.flush || message.state_lookup;
            if answered {
                rpc.report(&result).await?;
            }
            if message.finish_write {
                return Ok(result);
            }
            position = MessagePosition::Later;
            let Some(next) = stream.next().await.transpose()? else {
                if !answered {
                    rpc.report(&result).await?;
                }
                return Ok(result);
            };
            message = next;
        }
    }
}

/// Reads past `message` when it ends a stream that must end there, and returns the failure
/// that implies. The loop reads it before taking the session lease, so a client that never
/// half-closes cannot hold the session, and reports it after the message's own checks, which
/// take precedence.
async fn read_past_finish<S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin>(
    stream: &mut S,
    message: &pb::BidiWriteObjectRequest,
    rpc: WriteRpc<'_>,
) -> Result<(), Status> {
    if message.finish_write && rpc.ends_at_finish() && stream.next().await.transpose()?.is_some() {
        return Err(message_after_finish());
    }
    Ok(())
}

/// What a resumable session had persisted when its lease was taken.
#[derive(Clone, Copy)]
pub(super) struct Persisted {
    pub(super) offset: u64,
    pub(super) total: Option<u64>,
}

pub(super) struct ResumablePiece {
    offset: u64,
    pub(super) written: u64,
    length: u64,
}

/// Sends `content` to the upload consumer in bounded chunks.
async fn stage(
    tx: &mpsc::Sender<Result<UploadChunk, std::io::Error>>,
    content: &bytes::Bytes,
) -> Result<(), Status> {
    upload::stage(tx, content).await.map_err(intake_status)
}

fn check_resumable_message(
    message: &pb::BidiWriteObjectRequest,
    caller: Option<Caller<'_>>,
) -> Result<(), Status> {
    if message.first_message.is_some() {
        return Err(Status::invalid_argument(
            "Only the first message may contain write_object_spec or upload_id",
        ));
    }
    if message.object_checksums.is_some() && !message.finish_write {
        return Err(unsupported_for(
            "object_checksums before resumable finish; set StartResumableWrite.object_checksums or send them with finish_write=true",
            caller,
        ));
    }
    Ok(())
}

/// Validates one resumable message's offset, data checksum and size against the persisted
/// session, retaining the checked length for the catalog write.
pub(super) fn resumable_piece(
    message: &pb::BidiWriteObjectRequest,
    data: &pb::ChecksummedData,
    persisted: Persisted,
    position: MessagePosition,
    max_object_bytes: u64,
) -> Result<ResumablePiece, Status> {
    let offset = u64::try_from(message.write_offset).map_err(|_| {
        Status::out_of_range("write_offset must be nonnegative; query the persisted offset")
    })?;
    if (position == MessagePosition::First && offset != persisted.offset)
        || offset > persisted.offset
    {
        return Err(Status::out_of_range(
            "write_offset does not match persisted data; call QueryWriteStatus and resume at persisted_size",
        ));
    }
    verify_crc32c(data)?;
    let (length, end) = u64::try_from(data.content.len())
        .ok()
        .and_then(|length| offset.checked_add(length).map(|end| (length, end)))
        .filter(|(_, end)| i64::try_from(*end).is_ok())
        .ok_or_else(|| Status::out_of_range("write_offset plus content length overflows"))?;
    let end = end.max(persisted.offset);
    if end > max_object_bytes {
        return Err(Status::resource_exhausted(
            "Upload exceeds max_object_bytes; increase --max-object-bytes or write a smaller object",
        ));
    }
    if persisted
        .total
        .is_some_and(|total| end > total || (message.finish_write && end != total))
    {
        return Err(Status::out_of_range(
            "Written bytes do not match object_size; start a new upload with the correct size",
        ));
    }
    Ok(ResumablePiece {
        offset,
        written: end,
        length,
    })
}
