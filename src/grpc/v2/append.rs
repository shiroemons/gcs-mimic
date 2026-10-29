//! Storage v2 adapter for Rapid appendable objects: BidiWriteObject with
//! `WriteObjectSpec.appendable` or `AppendObjectSpec`, and reads of unfinalized generations.

use super::{
    Storage, convert, names,
    object_io::{access_status, common, conditions, hashes, object_status},
    pb,
    status::unsupported_for as unsupported,
};
use crate::{
    auth::{Caller, OBJECTS_CREATE},
    bucket::BucketAccess,
    object::{ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord},
    storage::{AppendError, AppendWriter, ObjectReader},
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

/// Whether a readable generation is a finalized object or a Rapid appendable one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Finality {
    Finalized,
    Unfinalized,
}

/// What a BidiWriteObject acknowledgement reports in its write status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Acknowledgement {
    Resource,
    PersistedSize,
}

/// Unfinalized generations have no MD5 and no finalize time.
pub(super) fn object(record: &ObjectRecord, finality: Finality) -> Result<pb::Object, Status> {
    let mut object = convert::object(record)?;
    if finality == Finality::Unfinalized
        && let Some(checksums) = object.checksums.as_mut()
    {
        checksums.md5_hash.clear();
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
        AppendError::ReplaceUnsupported => unsupported(
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
    let resource = acknowledgement == Acknowledgement::Resource;
    Ok(pb::BidiWriteObjectResponse {
        // The first acknowledgement of a new object carries its generation, which the client
        // needs for AppendObjectSpec on reconnect.
        write_status: Some(if resource {
            WriteStatus::Resource(object(record, Finality::Unfinalized)?)
        } else {
            WriteStatus::PersistedSize(writer.persisted_size() as i64)
        }),
        persisted_data_checksums: (!resource).then_some(pb::ObjectChecksums {
            crc32c: Some(record.crc32c),
            md5_hash: Vec::new(),
        }),
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
    ) -> Result<(AppendWriter, BucketAccess, bool), Status> {
        use pb::bidi_write_object_request::FirstMessage;
        match first.first_message.take() {
            Some(FirstMessage::WriteObjectSpec(mut spec)) => {
                if spec.object_size.is_some() {
                    return Err(unsupported(
                        "appendable object_size; omit it and finalize with finish_write",
                        caller,
                    ));
                }
                spec.appendable = None;
                let (bucket, name, options, _) = self.write_spec(spec, None, caller).await?;
                if options.expected != ExpectedHashes::default() || first.object_checksums.is_some()
                {
                    return Err(Status::invalid_argument(
                        "Appendable objects accept checksums only with finish_write; omit resource.checksums and first-message object_checksums",
                    ));
                }
                let access = options.access.clone();
                let writer = self
                    .state
                    .catalog
                    .create_appendable(&bucket, &name, options)
                    .await
                    .map_err(|e| append_status(e, caller))?;
                Ok((writer, access, false))
            }
            Some(FirstMessage::AppendObjectSpec(spec)) => {
                let (bucket, name) = names::object_name(&spec.bucket, &spec.object)?;
                if spec.generation <= 0 {
                    return Err(Status::invalid_argument(
                        "append_object_spec.generation is required; use the generation returned when the appendable object was created",
                    ));
                }
                let access =
                    crate::server::authorize_bucket(&self.state, caller, OBJECTS_CREATE, bucket)
                        .await
                        .map_err(|e| access_status(e, caller))?;
                let conditions = conditions(
                    None,
                    None,
                    spec.if_metageneration_match,
                    spec.if_metageneration_not_match,
                )?;
                let writer = self
                    .state
                    .catalog
                    .take_over_appendable(
                        bucket,
                        name,
                        spec.generation,
                        spec.write_handle.as_ref().map(|h| h.handle.as_slice()),
                        conditions,
                        access.clone(),
                    )
                    .await
                    .map_err(|e| append_status(e, caller))?;
                Ok((writer, access, true))
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
        let (mut writer, access, takeover) = self.open_writer(&mut message, caller).await?;
        let mut acknowledgement = if takeover {
            Acknowledgement::PersistedSize
        } else {
            Acknowledgement::Resource
        };
        let mut first = true;
        loop {
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
            if data
                .crc32c
                .is_some_and(|crc| crc != crc32c::crc32c(&data.content))
            {
                return Err(Status::invalid_argument(
                    "checksummed_data.crc32c mismatch; resend the correct data and checksum",
                ));
            }
            // A takeover's opening message only asks for the persisted size, so its offset
            // carries no meaning without data.
            if !data.content.is_empty() {
                let offset = u64::try_from(message.write_offset).map_err(|_| {
                    Status::out_of_range(
                        "write_offset must be nonnegative; resume at persisted_size",
                    )
                })?;
                writer
                    .append(offset, data.content, self.max_object_bytes)
                    .await
                    .map_err(|e| append_status(e, caller))?;
            }
            if message.object_checksums.is_some() && !message.finish_write {
                return Err(Status::invalid_argument(
                    "Appendable objects accept object_checksums only with finish_write=true",
                ));
            }
            if message.finish_write {
                let expected = hashes(message.object_checksums.as_ref())?;
                if expected.md5.is_some() {
                    return Err(Status::invalid_argument(
                        "Appendable objects have no MD5 hash; send only crc32c in object_checksums",
                    ));
                }
                let record = writer
                    .finalize(expected.crc32c, access.clone(), self.max_object_bytes)
                    .await
                    .map_err(|e| append_status(e, caller))?;
                return send(
                    tx,
                    pb::BidiWriteObjectResponse {
                        write_status: Some(pb::bidi_write_object_response::WriteStatus::Resource(
                            convert::object(&record)?,
                        )),
                        ..Default::default()
                    },
                )
                .await;
            }
            if message.flush {
                writer.flush().await.map_err(|e| append_status(e, caller))?;
            }
            if message.flush || message.state_lookup || (first && takeover) {
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
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(ObjectRecord, ObjectReader, Finality), ObjectError> {
        let catalog = &self.state.catalog;
        match catalog
            .open_object_authorized(bucket, name, generation, conditions, access)
            .await
        {
            Err(ObjectError::ObjectNotFound) => catalog
                .open_appendable(bucket, name, generation, conditions, access)
                .await
                .map(|(record, reader)| (record, reader, Finality::Unfinalized)),
            other => other.map(|(record, reader)| (record, reader, Finality::Finalized)),
        }
    }

    /// Metadata-only counterpart of [`Self::open_readable`]: finalized objects do not open
    /// their blob.
    pub(super) async fn get_readable(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(ObjectRecord, Finality), ObjectError> {
        let catalog = &self.state.catalog;
        match catalog
            .get_object_authorized(bucket, name, generation, conditions, access)
            .await
        {
            Err(ObjectError::ObjectNotFound) => catalog
                .open_appendable(bucket, name, generation, conditions, access)
                .await
                .map(|(record, _)| (record, Finality::Unfinalized)),
            other => other.map(|record| (record, Finality::Finalized)),
        }
    }

    pub(super) async fn delete_including_appendable(
        &self,
        bucket: &str,
        name: &str,
        generation: Option<i64>,
        conditions: ObjectConditions,
        access: &BucketAccess,
    ) -> Result<(), ObjectError> {
        let catalog = &self.state.catalog;
        match catalog
            .delete_object_authorized(bucket, name, generation, conditions, access)
            .await
        {
            Err(ObjectError::ObjectNotFound) => {
                catalog
                    .delete_appendable(bucket, name, generation, conditions, access)
                    .await
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests;
