//! Storage v2 adapter for Rapid appendable objects: BidiWriteObject with
//! `WriteObjectSpec.appendable` or `AppendObjectSpec`, and reads of unfinalized generations.

use super::{
    Storage, convert,
    object_io::{
        DeclaredChecksums, Flush, Lookup, NegativeOffset, Responses, StepEnd, VerifiedChunk,
        WriteSpec, WriteStep, conditions, next_step, object_status, send,
    },
    pb,
    status::unsupported_for,
};
use crate::{
    auth::{Caller, OBJECTS_CREATE},
    bucket::BucketAccess,
    csek::CustomerKey,
    object::{
        ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord, ObjectTarget,
        generation::{Generation, InvalidGeneration},
    },
    storage::{
        AppendError, AppendWriter, Finality, ObjectReader, ReadableMetadata, ResumableOptions,
    },
};
use tonic::{Status, codegen::tokio_stream::Stream};

pub(super) fn is_append(first: &pb::BidiWriteObjectRequest) -> bool {
    use pb::bidi_write_object_request::FirstMessage;
    match &first.first_message {
        Some(FirstMessage::AppendObjectSpec(_)) => true,
        Some(FirstMessage::WriteObjectSpec(spec)) => spec.appendable == Some(true),
        _ => false,
    }
}

/// What a BidiWriteObject acknowledgement reports in its write status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Acknowledgement {
    Resource,
    PersistedSize,
}

/// How a message ends its part of an appendable write.
enum AppendEnd {
    /// More messages may follow.
    Continue { flush: Flush, lookup: Lookup },
    /// The message finalizes the object, whose CRC32C must match the one declared, if any.
    /// Only a finalizing message may declare one.
    Finish { expected_crc32c: Option<u32> },
}

/// Bytes a message appends at `offset`.
struct Chunk {
    offset: u64,
    content: VerifiedChunk,
}

/// One message of an appendable stream: a [`WriteStep`] that passed the appendable rules.
/// Every check of the message runs before any of its bytes reach the writer.
struct AppendStep {
    /// `None` for a message without data: a takeover only asks for the persisted size, and an
    /// offset means nothing without data.
    chunk: Option<Chunk>,
    end: AppendEnd,
}

impl TryFrom<WriteStep> for AppendStep {
    type Error = Status;

    /// Reads a message whose first message, if any, the caller already took.
    fn try_from(step: WriteStep) -> Result<Self, Status> {
        super::customer_key::reject(step.params.as_ref(), "appendable objects")?;
        if step.first.is_some() {
            return Err(Status::invalid_argument(
                "Only the first message may contain write_object_spec or append_object_spec",
            ));
        }
        let content = step.data.verify()?;
        let end = match step.end {
            StepEnd::Continue { .. } if step.checksums.is_declared() => {
                return Err(Status::invalid_argument(
                    "Appendable objects accept object_checksums only with finish_write=true",
                ));
            }
            StepEnd::Continue { flush, lookup } => AppendEnd::Continue { flush, lookup },
            StepEnd::Finish => {
                let expected = step.checksums.expected()?;
                if expected.md5.is_some() {
                    return Err(Status::invalid_argument(
                        "Appendable objects have no MD5 hash; send only crc32c in object_checksums",
                    ));
                }
                AppendEnd::Finish {
                    expected_crc32c: expected.crc32c,
                }
            }
        };
        let chunk = if content.is_empty() {
            None
        } else {
            let offset = step.offset.position().map_err(|NegativeOffset| {
                Status::out_of_range("write_offset must be nonnegative; resume at persisted_size")
            })?;
            Some(Chunk { offset, content })
        };
        Ok(Self { chunk, end })
    }
}

impl AppendStep {
    /// Checks an opening message against the empty object a creating stream is about to make,
    /// as [`AppendWriter::append`] would check it after the create: the offset first, then the
    /// size limit.
    fn check_new_object(&self, max_bytes: u64) -> Result<(), AppendError> {
        let Some(Chunk { offset, content }) = &self.chunk else {
            return Ok(());
        };
        if *offset > 0 {
            return Err(AppendError::OffsetGap { expected: 0 });
        }
        if u64::try_from(content.len()).map_or(true, |length| length > max_bytes) {
            return Err(AppendError::Object(ObjectError::TooLarge));
        }
        Ok(())
    }
}

/// Unfinalized generations have no MD5 and no finalize time.
pub(super) fn object(
    record: &ObjectRecord,
    finality: Finality,
    hashes: crate::csek::StoredHashes,
) -> Result<pb::Object, Status> {
    let mut object = convert::object(record, hashes)?;
    if finality == Finality::Unfinalized {
        object.finalize_time = None;
        if let Some(checksums) = object.checksums.as_mut() {
            checksums.md5_hash.clear();
        }
    }
    Ok(object)
}

fn append_status(error: AppendError, caller: Option<Caller<'_>>) -> Status {
    match error {
        AppendError::Object(error) => object_status(error),
        AppendError::Superseded => Status::failed_precondition(
            "Another writer took over this appendable object, or it was finalized or deleted; reopen with append_object_spec and the current generation",
        ),
        AppendError::Finalized => Status::failed_precondition(
            "The appendable object is finalized and accepts no more data; write a new object instead",
        ),
        AppendError::ReplaceUnsupported => unsupported_for(
            "appendable writes that replace an existing object; delete the object first, or set if_generation_match=0 to create only",
            caller,
        ),
        AppendError::OffsetGap { expected } => Status::out_of_range(format!(
            "write_offset leaves a gap after the {expected} bytes received; resume at persisted_size"
        )),
    }
}

fn status_response(
    writer: &AppendWriter,
    acknowledgement: Acknowledgement,
) -> Result<pb::BidiWriteObjectResponse, Status> {
    use pb::bidi_write_object_response::WriteStatus;
    let record = writer.record();
    let (write_status, persisted_data_checksums) = match acknowledgement {
        // The first acknowledgement of a new object carries its generation, which the client
        // needs for AppendObjectSpec on reconnect.
        Acknowledgement::Resource => (
            WriteStatus::Resource(object(
                record,
                Finality::Unfinalized,
                crate::csek::StoredHashes::keyless(record.customer_key_sha256),
            )?),
            None,
        ),
        Acknowledgement::PersistedSize => (
            WriteStatus::PersistedSize(crate::grpc::convert::signed(
                writer.persisted_size(),
                "persisted_size",
            )?),
            Some(pb::ObjectChecksums {
                crc32c: Some(record.crc32c),
                md5_hash: Vec::new(),
            }),
        ),
    };
    Ok(pb::BidiWriteObjectResponse {
        write_status: Some(write_status),
        persisted_data_checksums,
        write_handle: Some(pb::BidiWriteHandle {
            handle: writer.write_handle().to_vec(),
        }),
    })
}

/// The writer an appendable stream opened, and what its first acknowledgement reports.
struct OpenedWriter {
    writer: AppendWriter,
    access: BucketAccess,
    acknowledgement: Acknowledgement,
}

/// What an appendable stream's first message asks to open, with its spec and the caller's
/// permission checked and nothing opened yet.
enum Opening {
    Create {
        bucket: String,
        name: String,
        options: Box<ResumableOptions>,
    },
    TakeOver {
        target: ObjectTarget<i64>,
        write_handle: Option<pb::BidiWriteHandle>,
        conditions: ObjectConditions,
        access: BucketAccess,
    },
}

impl Storage {
    /// Checks the spec of an appendable stream's first message and the caller's permission,
    /// which open nothing.
    async fn prepare_opening(
        &self,
        first: Option<pb::bidi_write_object_request::FirstMessage>,
        caller: Option<Caller<'_>>,
    ) -> Result<Opening, Status> {
        use pb::bidi_write_object_request::FirstMessage;
        match first {
            Some(FirstMessage::WriteObjectSpec(mut spec)) => {
                if spec.object_size.is_some() {
                    return Err(unsupported_for(
                        "appendable object_size; omit it and finalize with finish_write",
                        caller,
                    ));
                }
                spec.appendable = None;
                let WriteSpec {
                    bucket,
                    name,
                    options,
                    total: _,
                } = self
                    .write_spec(spec, &DeclaredChecksums::default(), caller)
                    .await?;
                // The opening message's own object_checksums are checked with the message; the
                // receive loop verifies them at finalization.
                if options.expected != ExpectedHashes::default() {
                    return Err(Status::invalid_argument(
                        "Appendable objects accept checksums only with finish_write; omit resource.checksums and send object_checksums with finish_write=true",
                    ));
                }
                Ok(Opening::Create {
                    bucket,
                    name,
                    options: Box::new(options),
                })
            }
            Some(FirstMessage::AppendObjectSpec(spec)) => {
                let (bucket, name) = self.object_name(&spec.bucket, &spec.object)?;
                let generation =
                    Generation::try_from(spec.generation).map_err(|InvalidGeneration| {
                        Status::invalid_argument(
                            "append_object_spec.generation is required; use the generation returned when the appendable object was created",
                        )
                    })?;
                let access = self.access(caller, bucket, OBJECTS_CREATE).await?;
                let conditions = conditions(ObjectConditions::from(&spec))?;
                Ok(Opening::TakeOver {
                    target: ObjectTarget::new(bucket, name, generation.get()),
                    write_handle: spec.write_handle,
                    conditions,
                    access,
                })
            }
            _ => Err(Status::invalid_argument(
                "First message must contain write_object_spec with appendable=true or append_object_spec",
            )),
        }
    }

    /// Creates or takes over the object `opening` names.
    async fn open_writer(
        &self,
        opening: Opening,
        caller: Option<Caller<'_>>,
    ) -> Result<OpenedWriter, Status> {
        match opening {
            Opening::Create {
                bucket,
                name,
                options,
            } => {
                let access = options.access.clone();
                let writer = self
                    .state
                    .catalog
                    .create_appendable(&bucket, &name, *options)
                    .await
                    .map_err(|e| append_status(e, caller))?;
                Ok(OpenedWriter {
                    writer,
                    access,
                    acknowledgement: Acknowledgement::Resource,
                })
            }
            Opening::TakeOver {
                target,
                write_handle,
                conditions,
                access,
            } => {
                let writer = self
                    .state
                    .catalog
                    .take_over_appendable(
                        target,
                        write_handle.as_ref().map(|h| h.handle.as_slice()),
                        conditions,
                        access.clone(),
                    )
                    .await
                    .map_err(|e| append_status(e, caller))?;
                Ok(OpenedWriter {
                    writer,
                    access,
                    acknowledgement: Acknowledgement::PersistedSize,
                })
            }
        }
    }

    /// Serves one appendable BidiWriteObject stream. Every message's data is written to the
    /// object's file as it arrives; `flush` makes it durable and visible to readers.
    pub(super) async fn receive_append<S>(
        &self,
        mut message: pb::BidiWriteObjectRequest,
        stream: &mut S,
        caller: Option<Caller<'_>>,
        tx: &Responses,
    ) -> Result<(), Status>
    where
        S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
    {
        // The spec and the caller's permission come first, as on every other write path. The
        // opening message is checked next, before the object is created or taken over, so a
        // refusal there changes nothing. A new object starts empty, so a creating stream's
        // opening offset and size limit are checked before the create as well; a takeover's
        // depend on the object it opens, so the writer checks them after the takeover, which
        // stays done.
        let opening = self
            .prepare_opening(message.first_message.take(), caller)
            .await?;
        let mut step = AppendStep::try_from(WriteStep::from(message))?;
        if let Opening::Create { .. } = opening {
            step.check_new_object(self.max_object_bytes)
                .map_err(|e| append_status(e, caller))?;
        }
        let OpenedWriter {
            mut writer,
            access,
            acknowledgement,
        } = self.open_writer(opening, caller).await?;
        // Every open is answered once, even without flush or state_lookup: a takeover learns
        // where to resume, and a creating stream learns its generation. The Python SDK waits
        // for that resource after a spec-only first message before it sends any data.
        let mut opening = Some(acknowledgement);
        loop {
            if let Some(Chunk { offset, content }) = step.chunk {
                writer
                    .append(offset, content.into_bytes(), self.max_object_bytes)
                    .await
                    .map_err(|e| append_status(e, caller))?;
            }
            let (flush, lookup) = match step.end {
                AppendEnd::Continue { flush, lookup } => (flush, lookup),
                AppendEnd::Finish { expected_crc32c } => {
                    return self
                        .finalize_append(writer, expected_crc32c, access, caller, tx)
                        .await;
                }
            };
            if flush == Flush::Yes {
                writer.flush().await.map_err(|e| append_status(e, caller))?;
            }
            let report = opening.take().or_else(|| {
                (flush == Flush::Yes || lookup == Lookup::Yes)
                    .then_some(Acknowledgement::PersistedSize)
            });
            if let Some(acknowledgement) = report {
                send(tx, status_response(&writer, acknowledgement)?).await?;
            }
            match next_step(stream).await? {
                Some(next) => step = AppendStep::try_from(next)?,
                None => {
                    // Closing the stream without finish_write leaves the object appendable; the
                    // service still reports the persisted size for a clean close.
                    writer.flush().await.map_err(|e| append_status(e, caller))?;
                    return send(
                        tx,
                        status_response(&writer, Acknowledgement::PersistedSize)?,
                    )
                    .await;
                }
            }
        }
    }

    /// Publishes the appendable object `writer` holds and answers with the finalized resource.
    async fn finalize_append(
        &self,
        mut writer: AppendWriter,
        expected_crc32c: Option<u32>,
        access: BucketAccess,
        caller: Option<Caller<'_>>,
        tx: &Responses,
    ) -> Result<(), Status> {
        let record = writer
            .finalize(expected_crc32c, access, self.max_object_bytes)
            .await
            .map_err(|e| append_status(e, caller))?;
        let resource = self
            .written_object(
                &record,
                crate::csek::StoredHashes::keyless(record.customer_key_sha256),
                caller,
            )
            .await?;
        send(
            tx,
            pb::BidiWriteObjectResponse {
                write_status: Some(pb::bidi_write_object_response::WriteStatus::Resource(
                    resource,
                )),
                ..Default::default()
            },
        )
        .await
    }

    /// Finalized objects come from the catalog; an absent name falls back to an unfinalized
    /// appendable generation. The finality reports which one was selected.
    pub(super) async fn open_readable(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
        key: Option<CustomerKey>,
    ) -> Result<(ObjectRecord, ObjectReader, Finality), ObjectError> {
        self.state
            .catalog
            .open_readable(target, conditions, access, key)
            .await
    }

    /// Metadata-only counterpart of [`Self::open_readable`]: neither state opens its blob.
    pub(super) async fn get_readable(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
        key: Option<CustomerKey>,
    ) -> Result<ReadableMetadata, ObjectError> {
        self.state
            .catalog
            .get_readable(target, conditions, access, key)
            .await
    }

    pub(super) async fn delete_including_appendable(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        self.state
            .catalog
            .delete_including_appendable(target, conditions, access)
            .await
    }
}

#[cfg(test)]
mod tests;
