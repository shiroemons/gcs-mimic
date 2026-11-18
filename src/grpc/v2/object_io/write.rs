use super::*;
use crate::csek::{CsekError, CustomerKey};
use crate::hashing::Hashes;

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
        let (_, extensions, input) = request.into_parts();
        let admitted = extensions.get();
        let mut position = MessagePosition::First;
        let mut stream = input.map(|result| {
            result.and_then(|r| {
                let message = pb::BidiWriteObjectRequest::from(r);
                if position == MessagePosition::First {
                    super::super::routing::write(admitted, message.first_message.as_ref())?;
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
        let (_, extensions, mut stream) = request.into_parts();
        let first = stream.message().await?.ok_or_else(|| {
            Status::invalid_argument("First message must contain write_object_spec or upload_id")
        })?;
        super::super::routing::write(extensions.get(), first.first_message.as_ref())?;
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

    /// Validates `spec` for `caller`; `checksums` are the ones the request declared beside it.
    pub(in crate::grpc::v2) async fn write_spec(
        &self,
        mut spec: pb::WriteObjectSpec,
        checksums: &DeclaredChecksums,
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
        let expected = merge_hashes(
            DeclaredChecksums::from(resource.checksums.take()).expected()?,
            checksums.expected()?,
        )?;
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
        checksums: &DeclaredChecksums,
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
        let Some(mut first) = next_step(stream).await? else {
            return Err(missing_first_message());
        };
        match first.first.take() {
            Some(FirstMessage::WriteObjectSpec(spec)) => {
                let write = SingleWrite { spec, caller, rpc };
                self.receive_single_write(stream, first, write).await
            }
            Some(FirstMessage::UploadId(upload)) => {
                let session = upload.parse::<UploadId>()?;
                self.receive_resumable_write(stream, first, &session, caller, rpc)
                    .await
            }
            Some(FirstMessage::AppendObjectSpec(_)) => Err(unsupported_for("Rapid append", caller)),
            None => Err(missing_first_message()),
        }
    }

    async fn receive_single_write<
        S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
    >(
        &self,
        stream: &mut S,
        first: WriteStep,
        write: SingleWrite<'_>,
    ) -> Result<pb::BidiWriteObjectResponse, Status> {
        let SingleWrite { spec, caller, rpc } = write;
        let WriteSpec {
            bucket,
            name,
            options,
            total,
        } = self
            .keyed_write_spec(spec, first.params.as_ref(), &first.checksums, caller)
            .await?;
        let mut body = SingleBody {
            intake: Intake::new(
                options.expected,
                self.max_object_bytes,
                total,
                Resend::DiscardOverlap,
            ),
            key: options.metadata.customer_key_sha256,
            persisted: GranulePrefix::default(),
        };
        // Cloud Storage answers the first message's data checksum before the write's conditions,
        // and its offset and size before the data checksum (recorded 2026-11-18), so the
        // message is admitted before the catalog is asked.
        let opening = body.admit(first, MessagePosition::First)?;
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
        let producer = body.produce(stream, opening, tx, rpc);
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

fn missing_first_message() -> Status {
    Status::invalid_argument("First message must contain write_object_spec or upload_id")
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

/// The answer to a message that left `size` bytes persisted with `checksums`.
pub(super) fn persisted_answer(
    size: u64,
    checksums: Option<pb::ObjectChecksums>,
) -> Result<pb::BidiWriteObjectResponse, Status> {
    Ok(pb::BidiWriteObjectResponse {
        write_status: Some(pb::bidi_write_object_response::WriteStatus::PersistedSize(
            crate::grpc::convert::signed(size, "persisted_size")?,
        )),
        persisted_data_checksums: checksums,
        write_handle: None,
    })
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

/// The RPC a write stream arrived on. WriteObject answers once, when the write returns.
/// BidiWriteObject sends each status it reports on its response stream. Both answer at
/// `finish_write` without reading further: Cloud Storage ignored a message that followed it
/// (recorded 2026-11-18).
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

    /// The `persisted_data_checksums` beside a persisted size whose bytes have digests
    /// `hashes`. BidiWriteObject reported the CRC32C alone where QueryWriteStatus reported the
    /// MD5 too (recorded 2026-11-18). No WriteObject answer with a persisted size was recorded;
    /// it keeps both.
    pub(super) fn persisted_checksums(self, hashes: Hashes) -> pb::ObjectChecksums {
        match self {
            Self::WriteObject => pb::ObjectChecksums {
                crc32c: Some(hashes.crc32c),
                md5_hash: hashes.md5.to_vec(),
            },
            Self::Bidi(_) => pb::ObjectChecksums {
                crc32c: Some(hashes.crc32c),
                md5_hash: Vec::new(),
            },
        }
    }
}

/// Whether a message is the first one of its stream. A resumable stream must start exactly at
/// the persisted offset and a non-resumable one at 0; later messages may overlap bytes already
/// received. Cloud Storage's refusal of a first offset past either start was recorded
/// (2026-11-18). A resumable first offset below a persisted size above 0 was not; its refusal
/// is inferred from the proto (see `resumable::Call`).
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

/// The whole granules at the start of a non-resumable write's body, which is what a `flush` or
/// a `state_lookup` reports as persisted. After three bytes Cloud Storage answered 0 with a
/// CRC32C of 0 and no MD5 (recorded 2026-11-18). A body of a granule or more was not recorded;
/// its whole granules are reported, as the resumable path persists them.
#[derive(Default)]
struct GranulePrefix {
    /// Bytes received past the last granule boundary.
    partial: usize,
    /// The CRC32C of every byte received.
    running: u32,
    size: u64,
    crc32c: u32,
}

impl GranulePrefix {
    fn extend(&mut self, mut data: &[u8]) {
        while !data.is_empty() {
            let (head, rest) = data.split_at(data.len().min(WRITE_GRANULE - self.partial));
            self.running = crc32c::crc32c_append(self.running, head);
            self.partial += head.len();
            if self.partial == WRITE_GRANULE {
                self.partial = 0;
                self.size = self.size.saturating_add(WRITE_GRANULE_BYTES);
                self.crc32c = self.running;
            }
            data = rest;
        }
    }

    fn answer(&self) -> Result<pb::BidiWriteObjectResponse, Status> {
        let checksums = pb::ObjectChecksums {
            crc32c: Some(self.crc32c),
            md5_hash: Vec::new(),
        };
        persisted_answer(self.size, Some(checksums))
    }
}

/// The bytes a message adds to a non-resumable write and how the message ends.
struct Admitted {
    added: bytes::Bytes,
    end: StepEnd,
}

/// The body of a non-resumable write as its messages arrive: offsets, sizes and checksums are
/// checked against what was received, and the body ends only after the whole object checks
/// out.
struct SingleBody {
    intake: Intake,
    /// The key the first message chose, which later messages cannot change.
    key: Option<[u8; 32]>,
    persisted: GranulePrefix,
}

impl SingleBody {
    /// Checks one message and returns what it adds. The first message's key and checksums were
    /// read with its spec.
    fn admit(&mut self, step: WriteStep, position: MessagePosition) -> Result<Admitted, Status> {
        if position == MessagePosition::Later {
            check_later_key(self.key, step.params.as_ref())?;
            self.intake
                .checksums(step.checksums.expected()?)
                .map_err(intake_status)?;
        }
        let offset = step.offset.position().map_err(|NegativeOffset| {
            Status::out_of_range("write_offset must be nonnegative; use 0 for the first message")
        })?;
        if position == MessagePosition::First && offset != 0 {
            // Recorded 2026-11-18, ahead of a wrong data checksum in the same message.
            return Err(with_reason(
                Code::OutOfRange,
                format!(
                    "A new upload request must start with write_offset 0, but received '{offset}'."
                ),
                "GRPC_UPLOAD_INVALID_START_OFFSET_NEW_UPLOAD",
            ));
        }
        let added = self
            .intake
            .admit(&step.data.into_piece(offset, step.end))
            .map_err(intake_status)?;
        Ok(Admitted {
            added,
            end: step.end,
        })
    }

    /// Streams the write's messages to the upload consumer, starting with the admitted
    /// `opening` message. A `flush` or `state_lookup` is answered with the persisted prefix;
    /// a later `write_object_spec` is ignored, as Cloud Storage ignores it (recorded
    /// 2026-11-18).
    async fn produce<S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin>(
        mut self,
        stream: &mut S,
        opening: Admitted,
        tx: mpsc::Sender<Result<UploadChunk, std::io::Error>>,
        rpc: WriteRpc<'_>,
    ) -> Result<(), Status> {
        let mut message = opening;
        loop {
            self.persisted.extend(&message.added);
            upload::stage(&tx, &message.added)
                .await
                .map_err(intake_status)?;
            if message.end.finishes() {
                break;
            }
            if message.end.asks_for_status() {
                rpc.report(&self.persisted.answer()?).await?;
            }
            let next = upload::next(stream, &tx)
                .await
                .map_err(intake_status)?
                .transpose()?
                .ok_or_else(|| {
                    // Cloud Storage's wording (recorded 2026-11-18).
                    Status::invalid_argument(
                        "Non-resumable write closed without setting finish_write parameter.",
                    )
                })?;
            message = self.admit(WriteStep::from(next), MessagePosition::Later)?;
        }
        self.intake
            .finish(&tx, EndMarker::Plain)
            .await
            .map_err(intake_status)
    }
}

#[cfg(test)]
mod message_tests;
#[cfg(test)]
mod tests;
