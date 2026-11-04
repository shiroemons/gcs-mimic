use super::{
    ResponseStream, Storage, access_status, convert, pb,
    status::{read_failure, unsupported_for},
};
use crate::{
    auth::{Caller, OBJECTS_CREATE, OBJECTS_DELETE, OBJECTS_GET, OBJECTS_LIST},
    grpc::{
        upload::{self, EndMarker, Intake, IntakeError, IntakePiece, Resend},
        validate::{self, request_conditions},
    },
    json,
    object::{
        ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord, ObjectTarget, UploadChunk,
    },
    storage::{
        Finality, ObjectUpload, ResumableChunk, ResumableError, ResumableOptions, SessionState,
    },
};
use prost::Message;
use tokio::sync::{mpsc, watch};
use tonic::codegen::tokio_stream::{Stream, StreamExt, wrappers::ReceiverStream};
use tonic::{Request, Response, Status, Streaming};

mod read;
mod resource;
mod resumable;
mod write;

/// `?` would use JSON's unsupported-setting description instead of the v2 metadata wording.
pub(super) fn object_status(error: ObjectError) -> Status {
    if matches!(error, ObjectError::UnsupportedMetadata(_)) {
        return unsupported_for(&error.to_string(), None);
    }
    error.into()
}

/// [`object_status`] for an RPC that names one object: Cloud Storage repeats `<bucket>/<object>`
/// in its not-found message over gRPC.
pub(super) fn named_object_status<'a>(
    bucket: &'a str,
    name: &'a str,
) -> impl Fn(ObjectError) -> Status + 'a {
    move |error| {
        if matches!(error, ObjectError::UnsupportedMetadata(_)) {
            return object_status(error);
        }
        json::JsonError::from(error)
            .naming_object(bucket, name)
            .into()
    }
}

fn session_status(error: ResumableError) -> Status {
    match error {
        ResumableError::NotFound | ResumableError::Gone | ResumableError::Cancelled => {
            Status::not_found("Upload session not found; start a new resumable write")
        }
        ResumableError::Invalid(message) => Status::invalid_argument(message),
        ResumableError::InvalidContentRange => Status::invalid_argument(error.to_string()),
        ResumableError::Object(error) => object_status(error),
    }
}

pub(super) fn common(params: Option<&pb::CommonObjectRequestParams>) -> Result<(), Status> {
    validate::common(
        params,
        "customer encryption; omit common_object_request_params",
    )
}

pub(super) fn conditions(conditions: ObjectConditions) -> Result<ObjectConditions, Status> {
    validate::lenient_conditions(
        conditions,
        "Preconditions must be nonnegative; use if_generation_match=0 to create only",
    )
}

fn generation(value: i64) -> Result<Option<i64>, Status> {
    validate::generation(
        value,
        "generation must be positive, or 0 to select the live object",
    )
}

pub(super) fn hashes(value: Option<&pb::ObjectChecksums>) -> Result<ExpectedHashes, Status> {
    let Some(value) = value else {
        return Ok(ExpectedHashes::default());
    };
    Ok(ExpectedHashes {
        crc32c: value.crc32c,
        md5: if value.md5_hash.is_empty() {
            None
        } else {
            Some(
                value.md5_hash.as_slice().try_into().map_err(|_| {
                    Status::invalid_argument("md5_hash must contain exactly 16 bytes")
                })?,
            )
        },
    })
}

fn merge_hashes(first: ExpectedHashes, next: ExpectedHashes) -> Result<ExpectedHashes, Status> {
    validate::merge_hashes(
        first,
        next,
        "Conflicting object_checksums; use the same digest in each message",
    )
}

/// Rejects a data message whose CRC32C differs from its content, with the wording and hex form
/// Cloud Storage uses over gRPC.
pub(super) fn verify_crc32c(data: &pb::ChecksummedData) -> Result<(), Status> {
    upload::verify_crc32c(&data.content, data.crc32c).map_err(intake_status)
}

fn intake_status(error: IntakeError) -> Status {
    match error {
        IntakeError::Offset => Status::out_of_range(
            "write_offset exceeds received bytes; start at 0 and continue at the received byte count",
        ),
        IntakeError::ChunkCrc { computed, client } => Status::invalid_argument(format!(
            "Checksummed data message crc32c mismatch: server computed: '{computed:08x}', client provided '{client:08x}'."
        )),
        IntakeError::Overflow => Status::out_of_range("Object size overflow"),
        IntakeError::TooLarge => Status::resource_exhausted(
            "Upload exceeds max_object_bytes; increase --max-object-bytes or write a smaller object",
        ),
        IntakeError::SizeMismatch => Status::out_of_range(
            "Written bytes do not match object_size; start a new upload with the correct size",
        ),
        IntakeError::ObjectChecksum => Status::invalid_argument(
            "object_checksums mismatch; send checksums for the complete object",
        ),
        IntakeError::ConflictingChecksums => Status::invalid_argument(
            "Conflicting object_checksums; use the same digest in each message",
        ),
        IntakeError::ConsumerStopped => Status::cancelled("Upload consumer stopped"),
    }
}

impl Storage {
    pub(super) async fn delete_object(
        &self,
        request: Request<pb::DeleteObjectRequest>,
        caller: Option<Caller<'_>>,
    ) -> Result<Response<()>, Status> {
        let r = request.into_inner();
        common(r.common_object_request_params.as_ref())?;
        let (bucket, name) = self.object_name(&r.bucket, &r.object)?;
        let access = self.access(caller, bucket, OBJECTS_DELETE).await?;
        let target = ObjectTarget::new(bucket, name, generation(r.generation)?);
        self.delete_including_appendable(target, request_conditions!(conditions, r)?, &access)
            .await
            .map_err(named_object_status(bucket, name))?;
        Ok(Response::new(()))
    }
}

#[cfg(test)]
mod tests;
