//! Storage v2 adapter for Rapid appendable objects: BidiWriteObject with
//! `WriteObjectSpec.appendable` or `AppendObjectSpec`, and reads of unfinalized generations.

use super::{
    Storage, convert, names,
    object_io::{common, conditions, hashes, object_status, verify_crc32c},
    pb,
    status::unsupported_for,
};
use crate::{
    auth::{Caller, OBJECTS_CREATE},
    bucket::BucketAccess,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord, ObjectTarget},
    storage::{AppendError, AppendWriter, Finality, ObjectReader},
};
use tokio::sync::mpsc;
use tonic::{
    Status,
    codegen::tokio_stream::{Stream, StreamExt},
};

type Responses = mpsc::Sender<Result<pb::BidiWriteObjectResponse, Status>>;

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

#[derive(PartialEq, Eq)]
enum Flush {
    No,
    Yes,
}

#[derive(PartialEq, Eq)]
enum Report {
    No,
    Yes,
}

/// A message's validations finish before any of its bytes reach the writer.
struct AppendStep {
    data: Option<(u64, bytes::Bytes)>,
    finish: Option<Option<u32>>,
    flush: Flush,
    report: Report,
}

fn append_step(message: &mut pb::BidiWriteObjectRequest) -> Result<AppendStep, Status> {
    common(message.common_object_request_params.as_ref())?;
    if message.first_message.is_some() {
        return Err(Status::invalid_argument(
            "Only the first message may contain write_object_spec or append_object_spec",
        ));
    }
    let data = match message.data.take() {
        Some(pb::bidi_write_object_request::Data::ChecksummedData(data)) => data,
        None => pb::ChecksummedData::default(),
    };
    verify_crc32c(&data)?;
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
    // A takeover only asks for the persisted size; its offset has no meaning without data.
    let data = if data.content.is_empty() {
        None
    } else {
        let offset = u64::try_from(message.write_offset).map_err(|_| {
            Status::out_of_range("write_offset must be nonnegative; resume at persisted_size")
        })?;
        Some((offset, data.content))
    };
    Ok(AppendStep {
        data,
        finish: message.finish_write.then_some(expected.crc32c),
        flush: if message.flush { Flush::Yes } else { Flush::No },
        report: if message.flush || message.state_lookup {
            Report::Yes
        } else {
            Report::No
        },
    })
}

/// Unfinalized generations have no MD5 and no finalize time.
pub(super) fn object(record: &ObjectRecord, finality: Finality) -> Result<pb::Object, Status> {
    let mut object = convert::object(record)?;
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
            WriteStatus::Resource(object(record, Finality::Unfinalized)?),
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

async fn send(tx: &Responses, response: pb::BidiWriteObjectResponse) -> Result<(), Status> {
    tx.send(Ok(response))
        .await
        .map_err(|_| Status::cancelled("Response stream closed"))
}

impl Storage {
    async fn open_writer(
        &self,
        first: &mut pb::BidiWriteObjectRequest,
        caller: Option<Caller<'_>>,
    ) -> Result<(AppendWriter, BucketAccess, Acknowledgement), Status> {
        use pb::bidi_write_object_request::FirstMessage;
        match first.first_message.take() {
            Some(FirstMessage::WriteObjectSpec(mut spec)) => {
                if spec.object_size.is_some() {
                    return Err(unsupported_for(
                        "appendable object_size; omit it and finalize with finish_write",
                        caller,
                    ));
                }
                spec.appendable = None;
                let (bucket, name, options, _) = self.write_spec(spec, None, caller).await?;
                // The proto allows object_checksums on the finish_write message, which may also
                // be the first one; the receive loop verifies them at finalization. Rejecting
                // the others here keeps an invalid request from creating the object.
                if options.expected != ExpectedHashes::default()
                    || (first.object_checksums.is_some() && !first.finish_write)
                {
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
                Ok((writer, access, Acknowledgement::Resource))
            }
            Some(FirstMessage::AppendObjectSpec(spec)) => {
                let (bucket, name) = names::object_name(&spec.bucket, &spec.object)?;
                if spec.generation <= 0 {
                    return Err(Status::invalid_argument(
                        "append_object_spec.generation is required; use the generation returned when the appendable object was created",
                    ));
                }
                let access = self.access(caller, bucket, OBJECTS_CREATE).await?;
                let conditions = conditions(ObjectConditions {
                    if_metageneration_match: spec.if_metageneration_match,
                    if_metageneration_not_match: spec.if_metageneration_not_match,
                    ..Default::default()
                })?;
                let target = ObjectTarget::new(bucket, name, spec.generation);
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
                Ok((writer, access, Acknowledgement::PersistedSize))
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
        common(message.common_object_request_params.as_ref())?;
        let (mut writer, access, mut acknowledgement) =
            self.open_writer(&mut message, caller).await?;
        let mut first = true;
        loop {
            let step = append_step(&mut message)?;
            if let Some((offset, content)) = step.data {
                writer
                    .append(offset, content, self.max_object_bytes)
                    .await
                    .map_err(|e| append_status(e, caller))?;
            }
            if let Some(expected_crc32c) = step.finish {
                let record = writer
                    .finalize(expected_crc32c, access.clone(), self.max_object_bytes)
                    .await
                    .map_err(|e| append_status(e, caller))?;
                return send(
                    tx,
                    pb::BidiWriteObjectResponse {
                        write_status: Some(pb::bidi_write_object_response::WriteStatus::Resource(
                            self.written_object(&record, caller).await?,
                        )),
                        ..Default::default()
                    },
                )
                .await;
            }
            if step.flush == Flush::Yes {
                writer.flush().await.map_err(|e| append_status(e, caller))?;
            }
            if step.report == Report::Yes
                || (first && acknowledgement == Acknowledgement::PersistedSize)
            {
                send(tx, status_response(&writer, acknowledgement)?).await?;
                acknowledgement = Acknowledgement::PersistedSize;
            }
            first = false;
            match stream.next().await.transpose()? {
                Some(next) => message = next,
                None => {
                    // Closing the stream without finish_write leaves the object appendable; the
                    // service still reports the persisted size for a clean close.
                    writer.flush().await.map_err(|e| append_status(e, caller))?;
                    return send(tx, status_response(&writer, acknowledgement)?).await;
                }
            }
        }
    }

    /// Finalized objects come from the catalog; an absent name falls back to an unfinalized
    /// appendable generation. The finality reports which one was selected.
    pub(super) async fn open_readable(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(ObjectRecord, ObjectReader, Finality), ObjectError> {
        self.state
            .catalog
            .open_readable(target, conditions, access)
            .await
    }

    /// Metadata-only counterpart of [`Self::open_readable`]: neither state opens its blob.
    pub(super) async fn get_readable(
        &self,
        target: ObjectTarget,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(ObjectRecord, Finality), ObjectError> {
        self.state
            .catalog
            .get_readable(target, conditions, access)
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
