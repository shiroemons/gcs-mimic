use super::{
    ResponseStream, Storage, access_status, convert, pb,
    status::{internal_error, read_failure, unsupported_for, with_reason},
};
use crate::{
    auth::{Caller, OBJECTS_CREATE, OBJECTS_DELETE, OBJECTS_GET, OBJECTS_LIST},
    grpc::{
        upload::{self, EndMarker, Intake, IntakeError, Resend},
        validate::{self, NegativeGeneration},
    },
    json,
    object::{
        ExpectedHashes, HashMismatch, ObjectConditions, ObjectError, ObjectRecord, ObjectTarget,
        UploadChunk, generation::Generation,
    },
    storage::{
        Finality, ObjectUpload, ResumableChunk, ResumableError, ResumableOptions, SessionState,
    },
};
use tokio::sync::{mpsc, watch};
use tonic::codegen::tokio_stream::{Stream, StreamExt, wrappers::ReceiverStream};
use tonic::{Code, Request, Response, Status, Streaming};

mod object_mask;
mod read;
mod resource;
mod resumable;
mod step;
mod upload_id;
mod write;

pub(super) use object_mask::ObjectField;
pub(super) use step::{
    DeclaredChecksums, FirstMessage, Flush, Lookup, MessageError, NegativeOffset, StepEnd,
    VerifiedChunk, WriteStep, next_step,
};
pub(super) use upload_id::UploadId;
pub(super) use write::{Responses, WriteSpec, send};

/// `?` would use JSON's unsupported-setting description instead of the v2 metadata wording.
pub(super) fn object_status(error: ObjectError) -> Status {
    if matches!(error, ObjectError::UnsupportedMetadata(_)) {
        return unsupported_for(&error.to_string(), None);
    }
    error.into()
}

/// Maps the error of an RPC that names one object with Cloud Storage's `<bucket>/<object>`
/// not-found wording; an unsupported setting keeps JSON's description. Every other failure
/// keeps the shared conversion, with the details the JSON error has no place for.
pub(super) fn naming_object<'a>(
    bucket: &'a str,
    name: &'a str,
) -> impl Fn(ObjectError) -> Status + 'a {
    move |error| {
        if matches!(error, ObjectError::ObjectNotFound) {
            return Status::not_found(crate::storage::failure::wording::missing_named_object(
                bucket, name,
            ));
        }
        error.into()
    }
}

/// [`naming_object`] with the Storage v2 wording for unsupported metadata.
pub(super) fn named_object_status<'a>(
    bucket: &'a str,
    name: &'a str,
) -> impl Fn(ObjectError) -> Status + 'a {
    let named = naming_object(bucket, name);
    move |error| {
        if matches!(error, ObjectError::UnsupportedMetadata(_)) {
            object_status(error)
        } else {
            named(error)
        }
    }
}

const UPLOAD_CANCELLED: &str = "Upload has already been cancelled.";

/// The answer to a call that names a session it cannot continue.
///
/// Recorded 2026-11-18: a write to a cancelled session is INVALID_ARGUMENT "Upload has already
/// been cancelled.". Recorded (case grpc-probe-inferred-session-states): WriteObject, with and
/// without `finish_write`, CancelResumableWrite and QueryWriteStatus all answer a session that a
/// size-mismatched finishing message ended with FAILED_PRECONDITION "Upload has already been
/// terminated." and the reason GRPC_UPLOAD_ALREADY_TERMINATED. Not recorded; inferred from those
/// answers: a session the emulator ended for another reason ([`ResumableError::Gone`]) answers
/// the same.
///
/// Recorded (case grpc-probe-inferred-upload-ids): an issued upload ID with one character
/// changed, in the middle or at the end, answers INTERNAL on QueryWriteStatus, WriteObject and
/// CancelResumableWrite, as an ID with a character appended did (recorded 2026-11-18). So an ID
/// that names no session the catalog knows ([`ResumableError::NotFound`]) answers INTERNAL. That
/// includes a session that expired: the catalog answers an expired session as a missing one and
/// its maintenance deletes the expired row, so it cannot tell an ID that once named a session
/// from one that never did.
fn session_status(error: ResumableError) -> Status {
    match error {
        ResumableError::NotFound => internal_error(),
        ResumableError::Cancelled => Status::invalid_argument(UPLOAD_CANCELLED),
        ResumableError::Gone => with_reason(
            Code::FailedPrecondition,
            "Upload has already been terminated.",
            "GRPC_UPLOAD_ALREADY_TERMINATED",
        ),
        ResumableError::Invalid(message) => Status::invalid_argument(message),
        ResumableError::InvalidContentRange | ResumableError::Overrun(_) => {
            Status::invalid_argument(error.to_string())
        }
        ResumableError::Object(error) => object_status(error),
    }
}

/// [`session_status`] for QueryWriteStatus and CancelResumableWrite, which answer a cancelled
/// session with FAILED_PRECONDITION and its reason (recorded 2026-11-18) where a write answers
/// INVALID_ARGUMENT.
fn ended_session_status(error: ResumableError) -> Status {
    match error {
        ResumableError::Cancelled => with_reason(
            Code::FailedPrecondition,
            UPLOAD_CANCELLED,
            "GRPC_UPLOAD_ALREADY_CANCELLED",
        ),
        error @ (ResumableError::NotFound
        | ResumableError::Gone
        | ResumableError::Invalid(_)
        | ResumableError::InvalidContentRange
        | ResumableError::Overrun(_)
        | ResumableError::Object(_)) => session_status(error),
    }
}

pub(super) fn conditions(conditions: ObjectConditions) -> Result<ObjectConditions, Status> {
    validate::lenient_conditions(
        conditions,
        "Preconditions must be nonnegative; use if_generation_match=0 to create only",
    )
}

fn generation(value: i64) -> Result<Option<Generation>, Status> {
    validate::selected_generation(value).map_err(|NegativeGeneration| {
        Status::invalid_argument("generation must be positive, or 0 to select the live object")
    })
}

fn merge_hashes(first: ExpectedHashes, next: ExpectedHashes) -> Result<ExpectedHashes, Status> {
    upload::merge_hashes(first, next).map_err(intake_status)
}

/// The size of one write granule. Cloud Storage persists a resumable upload in whole granules
/// until `finish_write`: three bytes sent without it left `persisted_size` at 0 (recorded
/// 2026-11-18), and one granule left it at 262144 (case grpc-probe-inferred-granules).
const WRITE_GRANULE: usize = 256 * 1024;

/// Cloud Storage's refusal of bytes that cannot make an object of the declared `object_size`
/// (recorded 2026-11-18). It names StartResumableWrite on a non-resumable write as well.
fn size_mismatch(written: u64, total: u64) -> Status {
    with_reason(
        Code::OutOfRange,
        format!(
            "Invalid request. The final size of the upload is {written} byte(s). This does not match the expected size of {total} byte(s), which was specified in the StartResumableWrite request."
        ),
        "GRPC_MISMATCHED_UPLOAD_SIZE",
    )
}

/// The refusal of a complete object whose digest differs from the one declared for it. Recorded
/// for a CRC32C, both values in hexadecimal (case grpc-probe-inferred-session-states, on a
/// resumable write). Not recorded: the MD5 form, which follows the CRC32C one, and the wording
/// on a non-resumable write, which shares it.
fn object_checksum_mismatch(mismatch: HashMismatch) -> Status {
    let (name, computed, client) = match mismatch {
        HashMismatch::Crc32c { expected, computed } => (
            "crc32c",
            format!("{computed:08x}"),
            format!("{expected:08x}"),
        ),
        HashMismatch::Md5 { expected, computed } => (
            "md5",
            crate::hashing::encode_lower(&computed),
            crate::hashing::encode_lower(&expected),
        ),
    };
    Status::invalid_argument(format!(
        "Object {name} mismatch: server computed: '{computed}', client provided: '{client}'."
    ))
}

fn intake_status(error: IntakeError) -> Status {
    match error {
        IntakeError::Offset => Status::out_of_range(
            "write_offset exceeds received bytes; start at 0 and continue at the received byte count",
        ),
        IntakeError::ChunkCrc(mismatch) => MessageError::from(mismatch).into(),
        IntakeError::Overflow => Status::out_of_range("write_offset plus content length overflows"),
        IntakeError::TooLarge => Status::resource_exhausted(
            "Upload exceeds max_object_bytes; increase --max-object-bytes or write a smaller object",
        ),
        IntakeError::SizeMismatch { written, total } => size_mismatch(written, total),
        IntakeError::ObjectChecksum(mismatch) => object_checksum_mismatch(mismatch),
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
        // Cloud Storage deletes an object whatever key the request carries, even a wrong one or
        // one for an unencrypted object (recorded 2026-11-15), so the parameters are not read.
        let (bucket, name) = self.object_name(&r.bucket, &r.object)?;
        let access = self.access(caller, bucket, OBJECTS_DELETE).await?;
        let target =
            ObjectTarget::new(bucket, name, generation(r.generation)?.map(Generation::get));
        self.delete_including_appendable(target, conditions(ObjectConditions::from(&r))?, &access)
            .await
            .map_err(named_object_status(bucket, name))?;
        Ok(Response::new(()))
    }
}

#[cfg(test)]
mod tests;
