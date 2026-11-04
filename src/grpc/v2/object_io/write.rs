use super::resumable::{ResumableTarget, parse_upload_id};
use super::*;

impl Storage {
    pub(in crate::grpc::v2) async fn write_object(
        &self,
        request: Request<Streaming<pb::WriteObjectRequest>>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::WriteObjectResponse>, Status> {
        let metadata = request.metadata().clone();
        let mut position = MessagePosition::First;
        let mut stream = request.into_inner().map(|result| {
            result.and_then(|r| {
                let message = pb::BidiWriteObjectRequest {
                    write_offset: r.write_offset,
                    object_checksums: r.object_checksums,
                    finish_write: r.finish_write,
                    common_object_request_params: r.common_object_request_params,
                    first_message: r.first_message.map(|f| match f {
                        pb::write_object_request::FirstMessage::UploadId(id) => {
                            pb::bidi_write_object_request::FirstMessage::UploadId(id)
                        }
                        pb::write_object_request::FirstMessage::WriteObjectSpec(spec) => {
                            pb::bidi_write_object_request::FirstMessage::WriteObjectSpec(spec)
                        }
                    }),
                    data: r
                        .data
                        .map(|pb::write_object_request::Data::ChecksummedData(data)| {
                            pb::bidi_write_object_request::Data::ChecksummedData(data)
                        }),
                    ..Default::default()
                };
                if position == MessagePosition::First {
                    super::super::routing::write(&metadata, message.first_message.as_ref())?;
                    position = MessagePosition::Later;
                }
                Ok(message)
            })
        });
        let result = self
            .receive_write(&mut stream, caller, None, EndOfStream::Required)
            .await?;
        Ok(Response::new(pb::WriteObjectResponse {
            write_status: Some(match result {
                pb::bidi_write_object_response::WriteStatus::PersistedSize(n) => {
                    pb::write_object_response::WriteStatus::PersistedSize(n)
                }
                pb::bidi_write_object_response::WriteStatus::Resource(r) => {
                    pb::write_object_response::WriteStatus::Resource(r)
                }
            }),
            ..Default::default()
        }))
    }
    pub(in crate::grpc::v2) async fn bidi_write_object(
        &self,
        request: Request<Streaming<pb::BidiWriteObjectRequest>>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<ResponseStream<pb::BidiWriteObjectResponse>>, Status> {
        let metadata = request.metadata().clone();
        let mut stream = request.into_inner();
        let first = stream.message().await?.ok_or_else(|| {
            Status::invalid_argument("First message must contain write_object_spec or upload_id")
        })?;
        super::super::routing::write(&metadata, first.first_message.as_ref())?;
        let append = super::super::append::is_append(&first);
        let strict = caller.is_some();
        let identity = caller.and_then(|c| match c {
            Caller::Principal(i) => Some(i.clone()),
            Caller::Anonymous => None,
        });
        let storage = Storage {
            state: self.state.clone(),
            max_object_bytes: self.max_object_bytes,
            compat_profile: self.compat_profile,
        };
        let (tx, rx) = mpsc::channel(1);
        crate::task::spawn_supervised("grpc-write-object", async move {
            let caller = strict.then(|| {
                identity
                    .as_ref()
                    .map_or(Caller::Anonymous, Caller::Principal)
            });
            let result = if append {
                let mut stream = stream;
                storage
                    .receive_append(first, &mut stream, caller, &tx)
                    .await
            } else {
                let mut input = tonic::codegen::tokio_stream::once(Ok(first)).chain(stream);
                storage
                    .receive_write(&mut input, caller, Some(&tx), EndOfStream::NotChecked)
                    .await
                    .map(|_| ())
            };
            // Terminal stream errors bypass the RPC wrapper's anonymous mapping.
            if let Err(error) = result {
                let _ = tx
                    .send(Err(super::super::status::anonymous(error, caller)))
                    .await;
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    pub(in crate::grpc::v2) async fn write_spec(
        &self,
        mut spec: pb::WriteObjectSpec,
        checksums: Option<&pb::ObjectChecksums>,
        caller: Option<Caller<'_>>,
    ) -> Result<(String, String, ResumableOptions, Option<u64>), Status> {
        if spec.appendable == Some(true) || !spec.predefined_acl.is_empty() {
            return Err(unsupported_for(
                "appendable/predefined_acl; omit these fields",
                caller,
            ));
        }
        let mut resource = spec
            .resource
            .take()
            .ok_or_else(|| Status::invalid_argument("write_object_spec.resource is required"))?;
        if resource.encoded_len() > 8192 {
            return Err(Status::resource_exhausted(
                "Object metadata exceeds the supported 8 KiB limit; reduce metadata",
            ));
        }
        let (bucket, name) = self.object_name(&resource.bucket, &resource.name)?;
        let bucket = bucket.to_owned();
        let name = name.to_owned();
        let access = self.access(caller, &bucket, OBJECTS_CREATE).await?;
        let content_type = if resource.content_type.is_empty() {
            "application/octet-stream".to_owned()
        } else {
            std::mem::take(&mut resource.content_type)
        };
        let mut metadata = super::super::object_update::for_write(&resource);
        metadata.replace = false;
        let storage_class = std::mem::take(&mut resource.storage_class);
        resource.content_encoding.clear();
        resource.content_disposition.clear();
        resource.content_language.clear();
        resource.cache_control.clear();
        resource.metadata.clear();
        resource.event_based_hold = None;
        resource.custom_time = None;
        resource.temporary_hold = false;
        resource.name.clear();
        resource.bucket.clear();
        let expected = merge_hashes(hashes(resource.checksums.as_ref())?, hashes(checksums)?)?;
        resource.checksums = None;
        if resource != pb::Object::default() {
            return Err(unsupported_for(
                "write_object_spec.resource fields; use content_type, fixed/custom metadata and holds",
                caller,
            ));
        }
        metadata.content_type = Some(Some(content_type.clone()));
        metadata.storage_class = (!storage_class.is_empty())
            .then(|| crate::object::validate_object_storage_class(&storage_class))
            .transpose()
            .map_err(super::super::object_update::input_error)?;
        super::super::object_update::validate(&metadata)?;
        let conditions = request_conditions!(conditions, spec)?;
        let total = spec
            .object_size
            .map(|n| {
                u64::try_from(n)
                    .map_err(|_| Status::invalid_argument("object_size must be nonnegative"))
            })
            .transpose()?;
        if total.is_some_and(|n| n > self.max_object_bytes) {
            return Err(Status::resource_exhausted(
                "object_size exceeds max_object_bytes; increase --max-object-bytes or write a smaller object",
            ));
        }
        let mut options = ResumableOptions::new(access);
        options.content_type = content_type;
        options.metadata = metadata;
        options.expected = expected;
        options.conditions = conditions;
        Ok((bucket, name, options, total))
    }

    pub(super) async fn receive_write<
        S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
    >(
        &self,
        stream: &mut S,
        caller: Option<Caller<'_>>,
        responses: Option<&Responses>,
        end: EndOfStream,
    ) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
        use pb::bidi_write_object_request::FirstMessage;
        let mut message = stream.next().await.transpose()?.ok_or_else(|| {
            Status::invalid_argument("First message must contain write_object_spec or upload_id")
        })?;
        common(message.common_object_request_params.as_ref())?;
        let session = match message.first_message.take() {
            Some(FirstMessage::WriteObjectSpec(spec)) => {
                let write = SingleWrite {
                    spec,
                    caller,
                    responses,
                    end,
                };
                return self.receive_single_write(stream, message, write).await;
            }
            Some(FirstMessage::UploadId(upload)) => {
                let (bucket, id) = parse_upload_id(&upload)?;
                ResumableTarget {
                    bucket: bucket.to_owned(),
                    id: id.to_owned(),
                }
            }
            Some(FirstMessage::AppendObjectSpec(_)) => {
                return Err(unsupported_for("Rapid append", caller));
            }
            None => {
                return Err(Status::invalid_argument(
                    "First message must contain write_object_spec or upload_id",
                ));
            }
        };
        self.receive_resumable_write(stream, message, &session, caller, responses, end)
            .await
    }

    async fn receive_single_write<
        S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
    >(
        &self,
        stream: &mut S,
        message: pb::BidiWriteObjectRequest,
        write: SingleWrite<'_>,
    ) -> Result<pb::bidi_write_object_response::WriteStatus, Status> {
        let SingleWrite {
            spec,
            caller,
            responses,
            end,
        } = write;
        let (bucket, name, options, total) = self
            .write_spec(spec, message.object_checksums.as_ref(), caller)
            .await?;
        let permit = self
            .state
            .catalog
            .prepare_object_upload(&bucket, &name, options.conditions, &options.access)
            .await
            .map_err(|error| match error {
                // Over WriteObject Cloud Storage words a missing bucket differently from reads.
                ObjectError::BucketNotFound => {
                    Status::not_found(format!("Bucket 'projects/_/buckets/{bucket}' not found"))
                }
                error => object_status(error),
            })?;
        let (tx, rx) = mpsc::channel(2);
        let limits = SingleWriteLimits {
            expected: options.expected,
            total,
            max_object_bytes: self.max_object_bytes,
        };
        let producer = produce_single_write(stream, message, limits, tx, caller, end);
        let write = self.state.catalog.put_object_prechecked(
            ObjectUpload {
                bucket: &bucket,
                name: &name,
                content_type: options.content_type.clone(),
                expected: options.expected,
                conditions: options.conditions,
                max_bytes: self.max_object_bytes,
                metadata: options.metadata.clone(),
                access: options.access.clone(),
            },
            rx,
            permit,
        );
        let (sent, written) = tokio::join!(producer, write);
        let record = upload::completed(sent, written, object_status)?;
        let resource = self.written_object(&record, caller).await?;
        let result = pb::bidi_write_object_response::WriteStatus::Resource(resource);
        send_status(responses, &result).await?;
        Ok(result)
    }
}

pub(super) type Responses = mpsc::Sender<Result<pb::BidiWriteObjectResponse, Status>>;

/// Whether a write must end its request stream at `finish_write`. WriteObject rejects a later
/// message; BidiWriteObject answers at `finish_write` without waiting for the stream to close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EndOfStream {
    Required,
    NotChecked,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Reported {
    Yes,
    No,
}

/// Whether a resumable message is the first one of its stream, which must resume exactly at
/// the persisted offset; later messages may overlap bytes already persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MessagePosition {
    First,
    Later,
}

impl MessagePosition {
    pub(super) fn require_resume_start(self) -> Result<(), Status> {
        match self {
            Self::First => Ok(()),
            Self::Later => Err(Status::failed_precondition(
                "Upload finalized by another stream; start a new upload",
            )),
        }
    }
}

/// A non-resumable write and where its replies go.
struct SingleWrite<'a> {
    spec: pb::WriteObjectSpec,
    caller: Option<Caller<'a>>,
    responses: Option<&'a Responses>,
    end: EndOfStream,
}

/// The checksum and size limits a non-resumable write's body must satisfy.
#[derive(Clone, Copy)]
struct SingleWriteLimits {
    expected: ExpectedHashes,
    total: Option<u64>,
    max_object_bytes: u64,
}

pub(super) async fn send_status(
    responses: Option<&Responses>,
    status: &pb::bidi_write_object_response::WriteStatus,
) -> Result<Reported, Status> {
    let Some(tx) = responses else {
        return Ok(Reported::No);
    };
    tx.send(Ok(pb::BidiWriteObjectResponse {
        write_status: Some(status.clone()),
        ..Default::default()
    }))
    .await
    .map_err(|_| Status::cancelled("Response stream closed"))?;
    Ok(Reported::Yes)
}

/// Streams a non-resumable write's messages to the upload consumer, validating offsets,
/// checksums and sizes, and ends the body only after the whole object checks out.
async fn produce_single_write<
    S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
>(
    stream: &mut S,
    mut message: pb::BidiWriteObjectRequest,
    limits: SingleWriteLimits,
    tx: mpsc::Sender<Result<UploadChunk, std::io::Error>>,
    caller: Option<Caller<'_>>,
    end: EndOfStream,
) -> Result<(), Status> {
    let mut position = MessagePosition::First;
    let mut intake = Intake::new(
        limits.expected,
        limits.max_object_bytes,
        limits.total,
        Resend::DiscardOverlap,
    );
    loop {
        common(message.common_object_request_params.as_ref())?;
        if message.first_message.is_some() {
            return Err(Status::invalid_argument(
                "Only the first message may contain write_object_spec",
            ));
        }
        if (message.flush || message.state_lookup) && !message.finish_write {
            return Err(unsupported_for(
                "non-resumable flush/state_lookup; use StartResumableWrite and upload_id",
                caller,
            ));
        }
        if message.object_checksums.is_some()
            && position == MessagePosition::Later
            && !message.finish_write
        {
            return Err(Status::invalid_argument(
                "object_checksums is allowed only in the first or final message",
            ));
        }
        let checksums = hashes(message.object_checksums.as_ref())?;
        intake.checksums(checksums).map_err(intake_status)?;
        let start = u64::try_from(message.write_offset).map_err(|_| {
            Status::out_of_range("write_offset must be nonnegative; use 0 for the first message")
        })?;
        let data = match message.data.take() {
            Some(pb::bidi_write_object_request::Data::ChecksummedData(data)) => data,
            None => pb::ChecksummedData::default(),
        };
        intake
            .accept(
                IntakePiece {
                    offset: start,
                    content: data.content,
                    crc32c: data.crc32c,
                    finish: message.finish_write,
                },
                &tx,
            )
            .await
            .map_err(intake_status)?;
        if message.finish_write {
            break;
        }
        message = upload::next(stream, &tx)
            .await
            .map_err(intake_status)?
            .transpose()?
            .ok_or_else(|| {
                Status::invalid_argument(
                    "Non-resumable write requires finish_write=true on its final message",
                )
            })?;
        position = MessagePosition::Later;
    }
    if end == EndOfStream::Required
        && upload::next(stream, &tx)
            .await
            .map_err(intake_status)?
            .transpose()?
            .is_some()
    {
        return Err(Status::invalid_argument(
            "A message followed finish_write; finish_write must be the final message",
        ));
    }
    intake
        .finish(&tx, EndMarker::Plain)
        .await
        .map_err(intake_status)
}
