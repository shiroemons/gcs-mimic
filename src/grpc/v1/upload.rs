use super::{
    Storage, common, conditions, convert, metadata, object_name, pb, projection, unsupported,
};
use crate::object::ObjectKey;
use crate::{
    auth::Caller,
    grpc::upload::{self, Intake, IntakeError, IntakePiece, Resend},
    object::{ExpectedHashes, ObjectConditions, ObjectError, UploadChunk},
    storage::ObjectUpload,
};
use tokio::sync::mpsc;
use tonic::{Status, Streaming};

fn checksums(value: Option<pb::ObjectChecksums>) -> Result<ExpectedHashes, Status> {
    let Some(value) = value else {
        return Ok(Default::default());
    };
    let md5 = match value.md5_hash.as_str() {
        "" => None,
        hash if hash.len() != 32 => {
            return Err(Status::invalid_argument(
                "object_checksums.md5_hash must be 32 hexadecimal characters",
            ));
        }
        hash => Some(
            crate::hex::decode(hash)
                .ok_or_else(|| Status::invalid_argument("Invalid hexadecimal MD5"))?,
        ),
    };
    Ok(ExpectedHashes {
        crc32c: value.crc32c,
        md5,
    })
}

impl Storage {
    pub(super) async fn upload(
        &self,
        mut stream: Streaming<pb::InsertObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<pb::Object, Status> {
        let mut first = stream.message().await?.ok_or_else(|| {
            Status::invalid_argument("InsertObject needs insert_object_spec in its first message")
        })?;
        let Some(pb::insert_object_request::FirstMessage::InsertObjectSpec(mut spec)) =
            first.first_message.take()
        else {
            return Err(unsupported(
                "legacy resumable upload; use insert_object_spec or JSON resumable uploads",
            ));
        };
        projection(spec.projection)?;
        if spec.predefined_acl != 0 {
            return Err(unsupported(
                "legacy insert predefined ACL; omit predefined_acl",
            ));
        }
        let object = spec
            .resource
            .take()
            .ok_or_else(|| Status::invalid_argument("insert_object_spec.resource is required"))?;
        let bucket = object.bucket.clone();
        let name = object.name.clone();
        let content_type = object.content_type.clone();
        object_name(&name)?;
        let metadata = metadata(object, crate::object::WriteMode::Patch)?;
        let conditions = conditions(ObjectConditions::from(&spec))?;
        let (bucket, access) = self
            .key_and_access(caller, &bucket, crate::auth::OBJECTS_CREATE)
            .await?;
        let key = ObjectKey::from_bucket(bucket, &name);
        let permit = self
            .state
            .catalog
            .prepare_object_upload(key, conditions, &access)
            .await?;
        let (sender, receiver) = mpsc::channel(2);
        let write = self.state.catalog.put_object_prechecked(
            ObjectUpload {
                key,
                content_type,
                expected: ExpectedHashes::default(),
                conditions,
                max_bytes: self.max_object_bytes,
                metadata,
                access,
            },
            receiver,
            permit,
        );
        let receive = receive_upload(first, stream, sender, self.max_object_bytes);
        let (sent, written) = tokio::join!(receive, write);
        let record = upload::completed(sent, written, |error| {
            if let ObjectError::HashMismatch(mismatch) = error {
                intake_status(IntakeError::ObjectChecksum(*mismatch))
            } else {
                Status::from(error)
            }
        })?;
        Ok(convert::object(&record)?)
    }
}

/// Streams InsertObject messages into the staging channel, validating offsets and checksums.
async fn receive_upload(
    first: pb::InsertObjectRequest,
    mut stream: Streaming<pb::InsertObjectRequest>,
    sender: mpsc::Sender<Result<UploadChunk, std::io::Error>>,
    max_object_bytes: u64,
) -> Result<(), Status> {
    let mut intake = Intake::new(
        ExpectedHashes::default(),
        max_object_bytes,
        None,
        Resend::Reject,
    );
    let mut message = first;
    loop {
        common(
            message.common_request_params.as_ref(),
            message.common_object_request_params.as_ref(),
        )?;
        if message.first_message.is_some() {
            return Err(Status::invalid_argument(
                "insert_object_spec is only allowed in the first message",
            ));
        }
        let offset = u64::try_from(message.write_offset)
            .ok()
            .filter(|&offset| offset == intake.offset())
            .ok_or_else(|| {
                Status::invalid_argument("write_offset must equal bytes already sent")
            })?;
        if message.object_checksums.is_some() && intake.offset() != 0 && !message.finish_write {
            return Err(Status::invalid_argument(
                "object_checksums belong in the first or final message",
            ));
        }
        let hashes = checksums(message.object_checksums)?;
        intake.checksums(hashes).map_err(intake_status)?;
        let finish = message.finish_write;
        let data = match message.data {
            Some(pb::insert_object_request::Data::ChecksummedData(data)) => data,
            Some(pb::insert_object_request::Data::Reference(_)) => {
                return Err(unsupported(
                    "legacy reference upload data; send checksummed_data",
                ));
            }
            None => pb::ChecksummedData::default(),
        };
        intake
            .accept(
                IntakePiece {
                    offset,
                    content: data.content,
                    crc32c: data.crc32c,
                    finish,
                },
                &sender,
            )
            .await
            .map_err(intake_status)?;
        let next = upload::next(&mut stream, &sender)
            .await
            .map_err(intake_status)?
            .transpose()?;
        // A message after `finish_write` is rejected before its contents are checked, and a
        // stream that ends without one only after its last message was accepted.
        message = match (finish, next) {
            (false, Some(next)) => next,
            (true, None) => break,
            (true, Some(_)) => {
                return Err(Status::invalid_argument(
                    "Messages after finish_write are invalid; close the send stream",
                ));
            }
            (false, None) => {
                return Err(Status::invalid_argument(
                    "Final message requires finish_write=true; resend the complete object",
                ));
            }
        };
    }
    intake.finish(&sender).await.map_err(intake_status)
}

fn intake_status(error: IntakeError) -> Status {
    match error {
        IntakeError::Offset => {
            Status::invalid_argument("write_offset must equal bytes already sent")
        }
        IntakeError::ChunkCrc(_) => {
            Status::invalid_argument("Chunk CRC32C mismatch; recompute checksummed_data.crc32c")
        }
        IntakeError::Overflow | IntakeError::TooLarge => {
            Status::resource_exhausted("Upload exceeds max_object_bytes; reduce the body")
        }
        IntakeError::SizeMismatch { .. } => {
            Status::out_of_range("Written bytes do not match object_size")
        }
        IntakeError::ObjectChecksum(_) => {
            Status::invalid_argument("Object checksum mismatch; recompute object_checksums")
        }
        IntakeError::ConflictingChecksums => {
            Status::invalid_argument("Conflicting object checksums")
        }
        IntakeError::ConsumerStopped => Status::cancelled("Upload staging stopped"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_checksums_reject_signed_hex_pairs() {
        let error = checksums(Some(pb::ObjectChecksums {
            md5_hash: "+0".repeat(16),
            ..Default::default()
        }))
        .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(error.message(), "Invalid hexadecimal MD5");
        assert_eq!(
            checksums(Some(pb::ObjectChecksums {
                md5_hash: "0aAf".repeat(8),
                ..Default::default()
            }))
            .unwrap()
            .md5,
            Some([
                10, 175, 10, 175, 10, 175, 10, 175, 10, 175, 10, 175, 10, 175, 10, 175
            ])
        );
    }
}
