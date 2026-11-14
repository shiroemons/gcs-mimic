use super::write::{EndOfStream, MessagePosition, Reported, Responses, send_status};
use super::*;

fn upload_id(bucket: &str, id: &str) -> String {
    format!("{bucket}/{id}")
}

pub(super) fn parse_upload_id(id: &str) -> Result<(&str, &str), Status> {
    let (bucket, id) = id
        .split_once('/')
        .filter(|(b, i)| !b.is_empty() && i.len() == 64 && i.bytes().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| {
            Status::not_found("Unknown upload_id; use the value returned by StartResumableWrite")
        })?;
    Ok((bucket, id))
}

impl Storage {
    /// Shares the JSON session-URI cancellation so staged bytes follow one cleanup journal.
    pub(in crate::grpc::v2) async fn cancel_resumable_write(
        &self,
        request: Request<pb::CancelResumableWriteRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::CancelResumableWriteResponse>, Status> {
        let r = request.into_inner();
        let (bucket, id) = parse_upload_id(&r.upload_id)?;
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
        common(r.common_object_request_params.as_ref())?;
        let spec = r
            .write_object_spec
            .ok_or_else(|| Status::invalid_argument("write_object_spec is required"))?;
        let (bucket, name, options, total) = self
            .write_spec(spec, r.object_checksums.as_ref(), caller)
            .await?;
        let session = self
            .state
            .catalog
            .create_resumable_session(&bucket, &name, options, total)
            .await
            .map_err(session_status)?;
        Ok(Response::new(pb::StartResumableWriteResponse {
            upload_id: upload_id(&bucket, &session.id.to_string()),
        }))
    }
    pub(in crate::grpc::v2) async fn query_write_status(
        &self,
        request: Request<pb::QueryWriteStatusRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::QueryWriteStatusResponse>, Status> {
        let r = request.into_inner();
        common(r.common_object_request_params.as_ref())?;
        let (bucket, id) = parse_upload_id(&r.upload_id)?;
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
            pb::query_write_status_response::WriteStatus::Resource(decode_completion(
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

fn decode_completion(value: Option<&serde_json::Value>) -> Result<pb::Object, Status> {
    let bytes: Vec<u8> = serde_json::from_value(
        value
            .cloned()
            .ok_or_else(|| Status::internal("Missing upload completion"))?,
    )
    .map_err(|_| Status::internal("Invalid upload completion"))?;
    pb::Object::decode(bytes.as_slice()).map_err(|_| Status::internal("Invalid upload completion"))
}

impl Storage {
    /// Completion keeps the publication snapshot and adds only the caller's ACL/owner view.
    async fn completed_resource(
        &self,
        completion: Option<&serde_json::Value>,
        caller: Option<Caller<'_>>,
        bucket: &str,
    ) -> Result<pb::Object, Status> {
        let mut resource = decode_completion(completion)?;
        self.add_written_owner_fields(caller, bucket, &mut resource)
            .await?;
        Ok(resource)
    }

    pub(super) async fn receive_resumable_write<
        S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
    >(
        &self,
        stream: &mut S,
        mut message: pb::BidiWriteObjectRequest,
        session: &ResumableTarget,
        caller: Option<Caller<'_>>,
        responses: Option<&Responses>,
        end: EndOfStream,
    ) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
        use pb::bidi_write_object_response::WriteStatus;
        let ResumableTarget { bucket, id } = session;
        let mut position = MessagePosition::First;
        let mut result;
        loop {
            check_resumable_message(&message, caller)?;
            let access = self.access(caller, bucket, OBJECTS_CREATE).await?;
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
                send_status(responses, &result).await?;
                return Ok(result);
            }
            let mut options = lease.session.options.clone();
            options.access = access;
            self.state
                .catalog
                .check_upload(
                    bucket,
                    &lease.session.name,
                    options.conditions,
                    &options.access,
                )
                .await
                .map_err(object_status)?;
            options.expected =
                merge_hashes(options.expected, hashes(message.object_checksums.as_ref())?)?;
            let persisted = Persisted {
                offset: lease.session.offset,
                total: lease.session.total,
            };
            let ResumablePiece {
                offset,
                data,
                written,
                length,
            } = resumable_piece(&mut message, persisted, position, self.max_object_bytes)?;
            if message.finish_write
                && end == EndOfStream::Required
                && stream.next().await.transpose()?.is_some()
            {
                return Err(Status::invalid_argument(
                    "A message followed finish_write; finish_write must be the final message",
                ));
            }
            let (tx, rx) = mpsc::channel(2);
            let (catalog_wait, _wait) = watch::channel(false);
            let chunk = ResumableChunk {
                start: offset,
                length,
                total: persisted.total.or(message.finish_write.then_some(written)),
                finalize: message.finish_write,
                open_end: false,
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
            let write =
                self.state
                    .catalog
                    .write_resumable_session(lease, chunk, rx, permit, |record| {
                        let resource = convert::object(record)?;
                        serde_json::to_value(resource.encode_to_vec())
                            .map_err(|_| ObjectError::InvalidMetadata)
                    });
            let (sent, written_record) = tokio::join!(producer, write);
            let completion = written_record.map_err(session_status)?;
            sent?;
            result = if let Some(value) = completion.as_ref() {
                let resource = self.completed_resource(Some(value), caller, bucket).await?;
                WriteStatus::Resource(resource)
            } else {
                WriteStatus::PersistedSize(crate::grpc::convert::signed(written, "persisted_size")?)
            };
            let reported = if message.finish_write || message.flush || message.state_lookup {
                send_status(responses, &result).await?
            } else {
                Reported::No
            };
            if message.finish_write {
                return Ok(result);
            }
            position = MessagePosition::Later;
            let Some(next) = stream.next().await.transpose()? else {
                if reported == Reported::No {
                    send_status(responses, &result).await?;
                }
                break;
            };
            message = next;
        }
        Ok(result)
    }
}

/// The resumable session a write stream continues.
pub(super) struct ResumableTarget {
    pub(super) bucket: String,
    pub(super) id: String,
}

/// What a resumable session had persisted when its lease was taken.
#[derive(Clone, Copy)]
pub(super) struct Persisted {
    pub(super) offset: u64,
    pub(super) total: Option<u64>,
}

pub(super) struct ResumablePiece {
    offset: u64,
    data: pb::ChecksummedData,
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
    common(message.common_object_request_params.as_ref())?;
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
    message: &mut pb::BidiWriteObjectRequest,
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
    let data = match message.data.take() {
        Some(pb::bidi_write_object_request::Data::ChecksummedData(data)) => data,
        None => pb::ChecksummedData::default(),
    };
    verify_crc32c(&data)?;
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
        data,
        written: end,
        length,
    })
}
