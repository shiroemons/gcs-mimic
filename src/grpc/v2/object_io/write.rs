use super::*;
use crate::csek::{CsekError, CustomerKey};

/// A caller the write task can own: [`Caller`] borrows its identity from the request.
enum TaskCaller {
    Anonymous,
    Principal(crate::auth::Identity),
}

impl From<Caller<'_>> for TaskCaller {
    fn from(caller: Caller<'_>) -> Self {
        match caller {
            Caller::Anonymous => Self::Anonymous,
            Caller::Principal(identity) => Self::Principal(identity.clone()),
        }
    }
}

impl TaskCaller {
    fn as_caller(&self) -> Caller<'_> {
        match self {
            Self::Anonymous => Caller::Anonymous,
            Self::Principal(identity) => Caller::Principal(identity),
        }
    }
}

/// A validated `WriteObjectSpec`: where the object goes and how its upload is checked.
pub(in crate::grpc::v2) struct WriteSpec {
    pub(in crate::grpc::v2) bucket: String,
    pub(in crate::grpc::v2) name: String,
    pub(in crate::grpc::v2) options: ResumableOptions,
    /// `object_size`, when the client declared it.
    pub(in crate::grpc::v2) total: Option<u64>,
}

/// A write stream sent a message after the one that set `finish_write`.
pub(super) fn message_after_finish() -> Status {
    Status::invalid_argument(
        "A message followed finish_write; finish_write must be the final message",
    )
}

/// Checks the `common_object_request_params` of a write message that did not start the write,
/// given the key the starting request chose (`chosen`, its fingerprint).
///
/// Cloud Storage ignores these parameters on a keyed write, even a different key, and encrypts
/// the object with the starting key (recorded 2026-11-15), so they are not parsed. On a keyless
/// write it answered INTERNAL ("We encountered an internal error. Please try again.") and
/// published nothing. The emulator deviates: INTERNAL invites a retry that fails the same way,
/// so it names the client's mistake, a key for an object that is not encrypted, and still
/// publishes nothing.
pub(super) fn check_later_key(
    chosen: Option<[u8; 32]>,
    params: Option<&pb::CommonObjectRequestParams>,
) -> Result<(), Status> {
    if chosen.is_none()
        && params.is_some_and(|params| params != &pb::CommonObjectRequestParams::default())
    {
        return Err(super::super::status::customer_encryption(
            CsekError::ResourceNotEncrypted,
        ));
    }
    Ok(())
}

impl Storage {
    pub(in crate::grpc::v2) async fn write_object(
        &self,
        request: Request<Streaming<pb::WriteObjectRequest>>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<pb::WriteObjectResponse>, Status> {
        let (metadata, _, input) = request.into_parts();
        let mut position = MessagePosition::First;
        let mut stream = input.map(|result| {
            result.and_then(|r| {
                let message = pb::BidiWriteObjectRequest::from(r);
                if position == MessagePosition::First {
                    super::super::routing::write(&metadata, message.first_message.as_ref())?;
                    position = MessagePosition::Later;
                }
                Ok(message)
            })
        });
        let answer = self
            .receive_write(&mut stream, caller, WriteRpc::WriteObject)
            .await?;
        Ok(Response::new(pb::WriteObjectResponse {
            write_status: answer.write_status.map(Into::into),
            persisted_data_checksums: answer.persisted_data_checksums,
        }))
    }
    pub(in crate::grpc::v2) async fn bidi_write_object(
        &self,
        request: Request<Streaming<pb::BidiWriteObjectRequest>>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<ResponseStream<pb::BidiWriteObjectResponse>>, Status> {
        let (metadata, _, mut stream) = request.into_parts();
        let first = stream.message().await?.ok_or_else(|| {
            Status::invalid_argument("First message must contain write_object_spec or upload_id")
        })?;
        super::super::routing::write(&metadata, first.first_message.as_ref())?;
        let append = super::super::append::is_append(&first);
        let caller = caller.map(TaskCaller::from);
        let storage = self.clone();
        let (tx, rx) = mpsc::channel(1);
        crate::task::spawn_supervised("grpc-write-object", async move {
            let caller = caller.as_ref().map(TaskCaller::as_caller);
            let result = if append {
                let mut stream = stream;
                storage
                    .receive_append(first, &mut stream, caller, &tx)
                    .await
            } else {
                let mut input = tonic::codegen::tokio_stream::once(Ok(first)).chain(stream);
                storage
                    .receive_write(&mut input, caller, WriteRpc::Bidi(&tx))
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
    ) -> Result<WriteSpec, Status> {
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
        metadata.write = crate::object::WriteMode::Patch;
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
        metadata.content_type = crate::object::Patch::Set(content_type.clone());
        metadata.storage_class = (!storage_class.is_empty())
            .then(|| crate::object::validate_object_storage_class(&storage_class))
            .transpose()
            .map_err(super::super::object_update::input_error)?;
        super::super::object_update::validate(&metadata)?;
        let conditions = conditions(ObjectConditions::from(&spec))?;
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
        Ok(WriteSpec {
            bucket,
            name,
            options,
            total,
        })
    }

    /// [`Self::write_spec`] for a request that starts a non-appendable write: the key in its
    /// `params` encrypts the new object (or binds the resumable session), and a malformed key
    /// fails before anything is staged.
    pub(super) async fn keyed_write_spec(
        &self,
        spec: pb::WriteObjectSpec,
        params: Option<&pb::CommonObjectRequestParams>,
        checksums: Option<&pb::ObjectChecksums>,
        caller: Option<Caller<'_>>,
    ) -> Result<WriteSpec, Status> {
        let key = super::super::customer_key::key(params)?;
        let mut write = self.write_spec(spec, checksums, caller).await?;
        write.options.metadata.customer_key_sha256 = key.map(CustomerKey::sha256);
        Ok(write)
    }

    pub(super) async fn receive_write<
        S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
    >(
        &self,
        stream: &mut S,
        caller: Option<Caller<'_>>,
        rpc: WriteRpc<'_>,
    ) -> Result<pb::BidiWriteObjectResponse, Status> {
        use pb::bidi_write_object_request::FirstMessage;
        let mut message = stream.next().await.transpose()?.ok_or_else(|| {
            Status::invalid_argument("First message must contain write_object_spec or upload_id")
        })?;
        let session = match message.first_message.take() {
            Some(FirstMessage::WriteObjectSpec(spec)) => {
                let write = SingleWrite { spec, caller, rpc };
                return self.receive_single_write(stream, message, write).await;
            }
            Some(FirstMessage::UploadId(upload)) => upload.parse::<UploadId>()?,
            Some(FirstMessage::AppendObjectSpec(_)) => {
                return Err(unsupported_for("Rapid append", caller));
            }
            None => {
                return Err(Status::invalid_argument(
                    "First message must contain write_object_spec or upload_id",
                ));
            }
        };
        self.receive_resumable_write(stream, message, &session, caller, rpc)
            .await
    }

    async fn receive_single_write<
        S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
    >(
        &self,
        stream: &mut S,
        message: pb::BidiWriteObjectRequest,
        write: SingleWrite<'_>,
    ) -> Result<pb::BidiWriteObjectResponse, Status> {
        let SingleWrite { spec, caller, rpc } = write;
        let WriteSpec {
            bucket,
            name,
            options,
            total,
        } = self
            .keyed_write_spec(
                spec,
                message.common_object_request_params.as_ref(),
                message.object_checksums.as_ref(),
                caller,
            )
            .await?;
        #[expect(
            clippy::wildcard_enum_match_arm,
            reason = "every other object error keeps the shared Storage v2 mapping"
        )]
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
            key: options.metadata.customer_key_sha256,
        };
        let producer = produce_single_write(stream, message, limits, tx, caller, rpc);
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
        let resource = self
            .written_object(&record, crate::csek::StoredHashes::Revealed, caller)
            .await?;
        let answer = finished(resource);
        rpc.report(&answer).await?;
        Ok(answer)
    }
}

/// The answer to a write that published `resource`; only a persisted size carries
/// `persisted_data_checksums`.
pub(super) fn finished(resource: pb::Object) -> pb::BidiWriteObjectResponse {
    pb::BidiWriteObjectResponse {
        write_status: Some(pb::bidi_write_object_response::WriteStatus::Resource(
            resource,
        )),
        ..Default::default()
    }
}

pub(in crate::grpc::v2) type Responses = mpsc::Sender<Result<pb::BidiWriteObjectResponse, Status>>;

/// Sends one BidiWriteObject response; the client may have closed its stream.
pub(in crate::grpc::v2) async fn send(
    tx: &Responses,
    response: pb::BidiWriteObjectResponse,
) -> Result<(), Status> {
    tx.send(Ok(response))
        .await
        .map_err(|_| Status::cancelled("Response stream closed"))
}

/// The RPC a write stream arrived on. WriteObject answers once, when the write returns, and
/// rejects a message after `finish_write`. BidiWriteObject sends each status it reports on its
/// response stream and answers at `finish_write` without reading further.
#[derive(Clone, Copy)]
pub(super) enum WriteRpc<'a> {
    WriteObject,
    Bidi(&'a Responses),
}

impl WriteRpc<'_> {
    /// Sends `answer` to a BidiWriteObject client; WriteObject returns it instead.
    pub(super) async fn report(self, answer: &pb::BidiWriteObjectResponse) -> Result<(), Status> {
        match self {
            Self::WriteObject => Ok(()),
            Self::Bidi(tx) => send(tx, answer.clone()).await,
        }
    }

    /// Whether the request stream must end at `finish_write`.
    pub(super) fn ends_at_finish(self) -> bool {
        match self {
            Self::WriteObject => true,
            Self::Bidi(_) => false,
        }
    }
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

/// A non-resumable write and the RPC it arrived on.
struct SingleWrite<'a> {
    spec: pb::WriteObjectSpec,
    caller: Option<Caller<'a>>,
    rpc: WriteRpc<'a>,
}

/// The checksum and size limits a non-resumable write's body must satisfy, and the key its
/// first message chose, which later messages cannot change.
#[derive(Clone, Copy)]
struct SingleWriteLimits {
    expected: ExpectedHashes,
    total: Option<u64>,
    max_object_bytes: u64,
    key: Option<[u8; 32]>,
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
    rpc: WriteRpc<'_>,
) -> Result<(), Status> {
    let mut position = MessagePosition::First;
    let mut intake = Intake::new(
        limits.expected,
        limits.max_object_bytes,
        limits.total,
        Resend::DiscardOverlap,
    );
    loop {
        // The first message's key was parsed with its spec.
        if position == MessagePosition::Later {
            check_later_key(limits.key, message.common_object_request_params.as_ref())?;
        }
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
        let data = take_data(&mut message);
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
    if rpc.ends_at_finish()
        && upload::next(stream, &tx)
            .await
            .map_err(intake_status)?
            .transpose()?
            .is_some()
    {
        return Err(message_after_finish());
    }
    intake
        .finish(&tx, EndMarker::Plain)
        .await
        .map_err(intake_status)
}

#[cfg(test)]
mod message_tests;
#[cfg(test)]
mod tests;
