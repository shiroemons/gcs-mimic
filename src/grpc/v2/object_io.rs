use super::{
    ResponseStream, Storage, access_status, convert, pb,
    status::{read_failure, unsupported_for, with_reason},
};
use crate::{
    auth::{Caller, OBJECTS_CREATE, OBJECTS_DELETE, OBJECTS_GET, OBJECTS_LIST},
    grpc::{
        upload::{self, EndMarker, Intake, IntakeError, Resend},
        validate::{self, NegativeGeneration},
    },
    json,
    object::{
        ExpectedHashes, ObjectConditions, ObjectError, ObjectRecord, ObjectTarget, UploadChunk,
        generation::Generation,
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
const UPLOAD_TERMINATED: &str = "Upload has already been terminated.";
const UPLOAD_NOT_FOUND: &str = "Upload session not found; start a new resumable write";

/// The answer to a write that names a session it cannot continue.
///
/// Recorded 2026-11-18: a write to a cancelled session is INVALID_ARGUMENT "Upload has already
/// been cancelled.". Not recorded; inferred from that answer and from the wording QueryWriteStatus
/// was recorded giving a terminated session: a write to a session the emulator ended
/// ([`ResumableError::Gone`]) is INVALID_ARGUMENT "Upload has already been terminated.".
///
/// Not recorded: an upload ID that parses but whose session does not exist, has expired (also
/// while a write holds it) or belongs to another bucket. The nearest recorded answer is the
/// INTERNAL that Cloud Storage gave IDs it could not read, which [`UnknownUploadId::Unreadable`]
/// keeps. Such a session can never be found again, and INTERNAL makes SDKs retry a call that
/// cannot succeed, so the emulator answers NOT_FOUND and names the remedy instead.
///
/// [`UnknownUploadId::Unreadable`]: upload_id::UnknownUploadId::Unreadable
fn session_status(error: ResumableError) -> Status {
    match error {
        ResumableError::NotFound => Status::not_found(UPLOAD_NOT_FOUND),
        ResumableError::Cancelled => Status::invalid_argument(UPLOAD_CANCELLED),
        ResumableError::Gone => Status::invalid_argument(UPLOAD_TERMINATED),
        ResumableError::Invalid(message) => Status::invalid_argument(message),
        ResumableError::InvalidContentRange | ResumableError::Overrun(_) => {
            Status::invalid_argument(error.to_string())
        }
        ResumableError::Object(error) => object_status(error),
    }
}

/// [`session_status`] for QueryWriteStatus and CancelResumableWrite, which answer a session
/// that already ended with FAILED_PRECONDITION and its reason.
///
/// Recorded 2026-11-18: QueryWriteStatus on a cancelled session, and on a session that a
/// finishing message with the wrong size ended. CancelResumableWrite was recorded only on a
/// cancelled and on a completed session, which its caller answers. Not recorded; inferred from
/// QueryWriteStatus's answer: QueryWriteStatus on a session the emulator ended for another
/// reason, such as a complete object that failed its declared checksum, and
/// CancelResumableWrite on any session the emulator ended.
fn ended_session_status(error: ResumableError) -> Status {
    match error {
        ResumableError::Cancelled => with_reason(
            Code::FailedPrecondition,
            UPLOAD_CANCELLED,
            "GRPC_UPLOAD_ALREADY_CANCELLED",
        ),
        ResumableError::Gone => with_reason(
            Code::FailedPrecondition,
            UPLOAD_TERMINATED,
            "GRPC_UPLOAD_ALREADY_TERMINATED",
        ),
        error @ (ResumableError::NotFound
        | ResumableError::Invalid(_)
        | ResumableError::InvalidContentRange
        | ResumableError::Overrun(_)
        | ResumableError::Object(_)) => session_status(error),
    }
}

fn generation(value: i64) -> Result<Option<Generation>, Status> {
    validate::selected_generation(value).map_err(|NegativeGeneration| {
        Status::invalid_argument("generation must be positive, or 0 to select the live object")
    })
}

fn merge_hashes(first: ExpectedHashes, next: ExpectedHashes) -> Result<ExpectedHashes, Status> {
    upload::merge_hashes(first, next).map_err(intake_status)
}

/// The size of one write granule. Cloud Storage persists an upload in whole granules until
/// `finish_write`: three bytes sent without it left `persisted_size` at 0 (recorded
/// 2026-11-18).
const WRITE_GRANULE: usize = 256 * 1024;
const WRITE_GRANULE_BYTES: u64 = 256 * 1024;

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
        // Cloud Storage deletes an object whatever key the request carries, even a wrong one or
        // one for an unencrypted object (recorded 2026-11-15), so the parameters are not read.
        let (bucket, name) = self.object_name(&r.bucket, &r.object)?;
        let access = self.access(caller, bucket, OBJECTS_DELETE).await?;
        let target =
            ObjectTarget::new(bucket, name, generation(r.generation)?.map(Generation::get));
        self.delete_including_appendable(target, ObjectConditions::from(&r), &access)
            .await
            .map_err(named_object_status(bucket, name))?;
        Ok(Response::new(()))
    }
}

#[cfg(test)]
mod tests;
