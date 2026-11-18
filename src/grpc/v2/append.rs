//! Storage v2 adapter for Rapid appendable objects: BidiWriteObject with
//! `WriteObjectSpec.appendable` or `AppendObjectSpec`, and reads of unfinalized generations.

use super::{
    Storage, convert,
    object_io::{
        Responses, WriteSpec, conditions, hashes, object_status, send, take_data, verify_crc32c,
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
    storage::{AppendError, AppendWriter, Finality, ObjectReader, ReadableMetadata},
};
use tonic::{
    Status,
    codegen::tokio_stream::{Stream, StreamExt},
};

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flush {
    No,
    Yes,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lookup {
    No,
    Yes,
}

/// How a message ends its part of the write.
enum StepEnd {
    /// More messages may follow. The proto ignores `flush` and `state_lookup` on a message that
    /// sets `finish_write`, so only this variant carries them.
    Continue { flush: Flush, lookup: Lookup },
    /// The message finalizes the object, whose CRC32C must match the one declared, if any.
    Finish { expected_crc32c: Option<u32> },
}

/// Content whose CRC32C matched the one its message declared, if any. Only
/// [`VerifiedChunk::verify`] makes one, so no unchecked byte reaches the writer.
struct VerifiedChunk(bytes::Bytes);

impl VerifiedChunk {
    fn verify(data: pb::ChecksummedData) -> Result<Self, Status> {
        verify_crc32c(&data)?;
        Ok(Self(data.content))
    }

    fn into_bytes(self) -> bytes::Bytes {
        self.0
    }
}

/// Bytes a message appends at `offset`.
struct Chunk {
    offset: u64,
    content: VerifiedChunk,
}

/// One decoded message of an appendable stream. Every check of the message runs before any of
/// its bytes reach the writer.
struct WriteStep {
    /// `None` for a message without data: a takeover only asks for the persisted size, and an
    /// offset means nothing without data.
    chunk: Option<Chunk>,
    end: StepEnd,
}

impl WriteStep {
    /// Decodes a message whose first message, if any, the caller already took.
    fn decode(message: &mut pb::BidiWriteObjectRequest) -> Result<Self, Status> {
        super::customer_key::reject(
            message.common_object_request_params.as_ref(),
            "appendable objects",
        )?;
        if message.first_message.is_some() {
            return Err(Status::invalid_argument(
                "Only the first message may contain write_object_spec or append_object_spec",
            ));
        }
        let content = VerifiedChunk::verify(take_data(message))?;
        if message.object_checksums.is_some() && !message.finish_write {
            return Err(Status::invalid_argument(
                "Appendable objects accept object_checksums only with finish_write=true",
            ));
        }
        let expected = hashes(message.object_checksums.as_ref())?;
        if expected.md5.is_some() {
            return Err(Status::invalid_argument(
                "Appendable objects have no MD5 hash; send only crc32c in object_checksums",
            ));
        }
        let chunk = if content.0.is_empty() {
            None
        } else {
            let offset = u64::try_from(message.write_offset).map_err(|_| {
                Status::out_of_range("write_offset must be nonnegative; resume at persisted_size")
            })?;
            Some(Chunk { offset, content })
        };
        let end = if message.finish_write {
            StepEnd::Finish {
                expected_crc32c: expected.crc32c,
            }
        } else {
            StepEnd::Continue {
                flush: if message.flush { Flush::Yes } else { Flush::No },
                lookup: if message.state_lookup {
                    Lookup::Yes
                } else {
                    Lookup::No
                },
            }
        };
        Ok(Self { chunk, end })
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

impl Storage {
    async fn open_writer(
        &self,
        first: Option<pb::bidi_write_object_request::FirstMessage>,
        caller: Option<Caller<'_>>,
    ) -> Result<OpenedWriter, Status> {
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
                } = self.write_spec(spec, None, caller).await?;
                // The opening message's own object_checksums were checked before this; the
                // receive loop verifies them at finalization.
                if options.expected != ExpectedHashes::default() {
                    return Err(Status::invalid_argument(
                        "Appendable objects accept checksums only with finish_write; omit resource.checksums and send object_checksums with finish_write=true",
                    ));
                }
                let access = options.access.clone();
                let writer = self
                    .state
                    .catalog
                    .create_appendable(&bucket, &name, options)
                    .await
                    .map_err(|e| append_status(e, caller))?;
                Ok(OpenedWriter {
                    writer,
                    access,
                    acknowledgement: Acknowledgement::Resource,
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
                let target = ObjectTarget::new(bucket, name, generation.get());
                let writer = self
                    .state
                    .catalog
                    .take_over_appendable(
                        target,
                        spec.write_handle.as_ref().map(|h| h.handle.as_slice()),
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
            _ => Err(Status::invalid_argument(
                "First message must contain write_object_spec with appendable=true or append_object_spec",
            )),
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
        let first = message.first_message.take();
        // The opening message's own fields are checked before it creates or takes over the
        // object, so a refusal there changes nothing. Its offset and the size limit depend on
        // the opened object, so the writer checks them after the open, which stays done.
        let mut step = WriteStep::decode(&mut message)?;
        let OpenedWriter {
            mut writer,
            access,
            acknowledgement,
        } = self.open_writer(first, caller).await?;
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
                StepEnd::Continue { flush, lookup } => (flush, lookup),
                StepEnd::Finish { expected_crc32c } => {
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
            match stream.next().await.transpose()? {
                Some(mut next) => step = WriteStep::decode(&mut next)?,
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
