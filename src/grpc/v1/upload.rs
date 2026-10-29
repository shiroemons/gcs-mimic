use super::{
    MetadataWrite, Storage, common, conditions, convert, metadata, object_error, pb, projection,
    unsupported,
};
use crate::{
    auth::Caller,
    hashing::Hasher,
    object::{ExpectedHashes, UploadChunk},
};
use tokio::sync::mpsc;
use tonic::{Status, Streaming};

fn checksums(value: Option<pb::ObjectChecksums>) -> Result<ExpectedHashes, Status> {
    let Some(value) = value else {
        return Ok(Default::default());
    };
    let md5 = if value.md5_hash.is_empty() {
        None
    } else {
        if value.md5_hash.len() != 32 {
            return Err(Status::invalid_argument(
                "object_checksums.md5_hash must be 32 hexadecimal characters",
            ));
        }
        let mut digest = [0; 16];
        for (i, pair) in value
            .md5_hash
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .enumerate()
        {
            let text = std::str::from_utf8(pair)
                .map_err(|_| Status::invalid_argument("Invalid hexadecimal MD5"))?;
            digest[i] = u8::from_str_radix(text, 16)
                .map_err(|_| Status::invalid_argument("Invalid hexadecimal MD5"))?;
        }
        Some(digest)
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
        let spec = match first.first_message.take() {
            Some(pb::insert_object_request::FirstMessage::InsertObjectSpec(spec)) => spec,
            _ => {
                return Err(unsupported(
                    "legacy resumable upload; use insert_object_spec or JSON resumable uploads",
                ));
            }
        };
        projection(spec.projection)?;
        if spec.predefined_acl != 0 {
            return Err(unsupported(
                "legacy insert predefined ACL; omit predefined_acl",
            ));
        }
        let object = spec
            .resource
            .ok_or_else(|| Status::invalid_argument("insert_object_spec.resource is required"))?;
        let bucket = object.bucket.clone();
        let name = object.name.clone();
        let content_type = object.content_type.clone();
        crate::json::validate_object_name(&name).map_err(super::from_json)?;
        let metadata = metadata(object, MetadataWrite::Merge)?;
        let conditions = conditions(
            spec.if_generation_match,
            spec.if_generation_not_match,
            spec.if_metageneration_match,
            spec.if_metageneration_not_match,
        )?;
        let access = self
            .access(caller, &bucket, "storage.objects.create")
            .await?;
        let permit = self
            .state
            .catalog
            .prepare_object_upload(&bucket, &name, conditions, &access)
            .await
            .map_err(object_error)?;
        let (sender, receiver) = mpsc::channel(2);
        let write = self.state.catalog.put_object_prechecked(
            &bucket,
            &name,
            content_type,
            ExpectedHashes::default(),
            conditions,
            self.max_object_bytes,
            receiver,
            permit,
            metadata,
            access,
        );
        let receive = receive_upload(first, stream, sender, self.max_object_bytes);
        let result = tokio::try_join!(async { write.await.map_err(object_error) }, receive)?;
        convert::object(&result.0)
    }
}

/// Streams InsertObject messages into the staging channel, validating offsets and checksums.
async fn receive_upload(
    first: pb::InsertObjectRequest,
    mut stream: Streaming<pb::InsertObjectRequest>,
    sender: mpsc::Sender<Result<UploadChunk, std::io::Error>>,
    max_object_bytes: u64,
) -> Result<(), Status> {
    let mut next = Some(first);
    let mut offset = 0_u64;
    let mut finished = false;
    let mut expected = ExpectedHashes::default();
    let mut hasher = Hasher::default();
    while let Some(message) = next {
        if finished {
            return Err(Status::invalid_argument(
                "Messages after finish_write are invalid; close the send stream",
            ));
        }
        common(
            message.common_request_params.as_ref(),
            message.common_object_request_params.as_ref(),
        )?;
        if message.first_message.is_some() {
            return Err(Status::invalid_argument(
                "insert_object_spec is only allowed in the first message",
            ));
        }
        if message.write_offset < 0 || message.write_offset as u64 != offset {
            return Err(Status::invalid_argument(
                "write_offset must equal bytes already sent",
            ));
        }
        if message.object_checksums.is_some() && offset != 0 && !message.finish_write {
            return Err(Status::invalid_argument(
                "object_checksums belong in the first or final message",
            ));
        }
        let hashes = checksums(message.object_checksums)?;
        if matches!((expected.md5,hashes.md5),(Some(a),Some(b)) if a!=b)
            || matches!((expected.crc32c,hashes.crc32c),(Some(a),Some(b)) if a!=b)
        {
            return Err(Status::invalid_argument("Conflicting object checksums"));
        }
        expected.md5 = hashes.md5.or(expected.md5);
        expected.crc32c = hashes.crc32c.or(expected.crc32c);
        if let Some(data) = message.data {
            let data = match data {
                pb::insert_object_request::Data::ChecksummedData(data) => data,
                _ => {
                    return Err(unsupported(
                        "legacy reference upload data; send checksummed_data",
                    ));
                }
            };
            if data
                .crc32c
                .is_some_and(|crc| crc != crc32c::crc32c(&data.content))
            {
                return Err(Status::invalid_argument(
                    "Chunk CRC32C mismatch; recompute checksummed_data.crc32c",
                ));
            }
            offset = offset
                .checked_add(data.content.len() as u64)
                .filter(|v| *v <= max_object_bytes)
                .ok_or_else(|| {
                    Status::resource_exhausted("Upload exceeds max_object_bytes; reduce the body")
                })?;
            hasher.update(&data.content);
            for part in crate::grpc::upload_chunks(&data.content) {
                sender
                    .send(Ok(UploadChunk::Data(part)))
                    .await
                    .map_err(|_| Status::cancelled("Upload staging stopped"))?;
            }
        }
        finished = message.finish_write;
        next = stream.message().await?;
    }
    if !finished {
        return Err(Status::invalid_argument(
            "Final message requires finish_write=true; resend the complete object",
        ));
    }
    let actual = hasher.finish();
    if expected.md5.is_some_and(|v| v != actual.md5)
        || expected.crc32c.is_some_and(|v| v != actual.crc32c)
    {
        return Err(Status::invalid_argument(
            "Object checksum mismatch; recompute object_checksums",
        ));
    }
    sender
        .send(Ok(UploadChunk::EndWithHashes(expected)))
        .await
        .map_err(|_| Status::cancelled("Upload staging stopped"))?;
    drop(sender);
    Ok::<_, Status>(())
}
