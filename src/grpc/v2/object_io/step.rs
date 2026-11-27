//! One message of a Storage v2 write stream, decoded once for every write path.
//!
//! The non-resumable, resumable and appendable paths share the fields a message carries and
//! differ only in the rules they apply to them and in their order, so each path reads a
//! [`WriteStep`] instead of the protobuf message. The fields a message can get wrong keep
//! what was sent until the path asks: the offset ([`WriteOffset::position`]), the object
//! checksums ([`DeclaredChecksums::expected`]) and the data, whose declared CRC32C
//! [`DeclaredChunk::verify`] or [`admit_bytes`](crate::grpc::upload::admit_bytes) compares. A
//! path may have to answer another rule first, and no unchecked byte reaches a writer either
//! way. The non-resumable and resumable paths check a message's data and object checksums
//! through one method, [`WriteStep::admit`], so they answer them in the same order.

use super::pb;
use crate::{
    grpc::upload::{self, CrcMismatch, IntakeError, IntakePiece},
    object::ExpectedHashes,
};
use tonic::{
    Status,
    codegen::tokio_stream::{Stream, StreamExt},
};

pub(in crate::grpc::v2) type FirstMessage = pb::bidi_write_object_request::FirstMessage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::grpc::v2) enum Flush {
    No,
    Yes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::grpc::v2) enum Lookup {
    No,
    Yes,
}

/// How a message ends its part of the write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::grpc::v2) enum StepEnd {
    /// More messages may follow. The proto ignores `flush` and `state_lookup` on a message that
    /// sets `finish_write`, so only this variant carries them.
    Continue { flush: Flush, lookup: Lookup },
    /// The message finalizes the object.
    Finish,
}

impl StepEnd {
    pub(in crate::grpc::v2) fn finishes(self) -> bool {
        match self {
            Self::Finish => true,
            Self::Continue { .. } => false,
        }
    }

    /// Whether the message asked for the persisted size.
    pub(in crate::grpc::v2) fn asks_for_status(self) -> bool {
        match self {
            Self::Finish => false,
            Self::Continue { flush, lookup } => flush == Flush::Yes || lookup == Lookup::Yes,
        }
    }
}

/// A message that no write path can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(in crate::grpc::v2) enum MessageError {
    /// `object_checksums.md5_hash` is not 16 bytes long.
    #[error("md5_hash must contain exactly 16 bytes")]
    Md5Length,
    /// The data's CRC32C differs from the one its message declared.
    /// The gRPC response preserves Cloud Storage's wording and hexadecimal form.
    #[error("Checksummed data message crc32c mismatch: server computed: '{:08x}', client provided '{:08x}'.", .0.computed, .0.client)]
    ChunkCrc(CrcMismatch),
}

impl From<CrcMismatch> for MessageError {
    fn from(mismatch: CrcMismatch) -> Self {
        Self::ChunkCrc(mismatch)
    }
}

impl From<MessageError> for Status {
    fn from(error: MessageError) -> Self {
        Status::invalid_argument(error.to_string())
    }
}

/// A `write_offset` below zero, which names no position. Each write path words the refusal
/// with the remedy that fits it.
#[derive(Debug)]
pub(in crate::grpc::v2) struct NegativeOffset;

/// A message's `write_offset` as sent. It is read only where it means something: an appendable
/// message without data ignores it.
#[derive(Clone, Copy, Debug)]
pub(in crate::grpc::v2) struct WriteOffset(i64);

impl WriteOffset {
    pub(in crate::grpc::v2) fn position(self) -> Result<u64, NegativeOffset> {
        u64::try_from(self.0).map_err(|_| NegativeOffset)
    }
}

/// A message's data beside the CRC32C the message declared for it, not yet compared.
pub(in crate::grpc::v2) struct DeclaredChunk {
    content: bytes::Bytes,
    crc32c: Option<u32>,
}

impl DeclaredChunk {
    pub(in crate::grpc::v2) fn verify(self) -> Result<VerifiedChunk, MessageError> {
        upload::verify_crc32c(&self.content, self.crc32c)?;
        Ok(VerifiedChunk(self.content))
    }

    /// Hands the data to [`upload::admit_bytes`], which verifies it after its offset and size
    /// checks.
    pub(in crate::grpc::v2) fn into_piece(self, offset: u64, end: StepEnd) -> IntakePiece {
        IntakePiece {
            offset,
            content: self.content,
            crc32c: self.crc32c,
            finish: end.finishes(),
        }
    }
}

/// Content whose CRC32C matched the one its message declared, if any. Only
/// [`DeclaredChunk::verify`] makes one.
pub(in crate::grpc::v2) struct VerifiedChunk(bytes::Bytes);

impl VerifiedChunk {
    pub(in crate::grpc::v2) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(in crate::grpc::v2) fn len(&self) -> usize {
        self.0.len()
    }

    pub(in crate::grpc::v2) fn into_bytes(self) -> bytes::Bytes {
        self.0
    }
}

/// One decoded message of a write stream.
pub(in crate::grpc::v2) struct WriteStep {
    /// The `write_object_spec`, `upload_id` or `append_object_spec` the message names. The
    /// stream's first message starts the write with it; on a later message the non-resumable
    /// and resumable paths ignore it, as Cloud Storage does (recorded 2026-11-18).
    pub(in crate::grpc::v2) first: Option<FirstMessage>,
    pub(in crate::grpc::v2) params: Option<pb::CommonObjectRequestParams>,
    pub(in crate::grpc::v2) offset: WriteOffset,
    pub(in crate::grpc::v2) data: DeclaredChunk,
    pub(in crate::grpc::v2) checksums: DeclaredChecksums,
    pub(in crate::grpc::v2) end: StepEnd,
}

/// An `object_checksums` as sent beside a spec or in a message. It is read where its write
/// path checks it: `StartResumableWrite` reads its own with the spec, and every message of a
/// non-resumable or resumable write reads it after its data ([`WriteStep::admit`]) and before
/// the write's conditions.
#[derive(Default)]
pub(in crate::grpc::v2) struct DeclaredChecksums(Option<pb::ObjectChecksums>);

impl From<Option<pb::ObjectChecksums>> for DeclaredChecksums {
    fn from(checksums: Option<pb::ObjectChecksums>) -> Self {
        Self(checksums)
    }
}

impl DeclaredChecksums {
    /// Whether the message sent the field at all, even an empty one.
    pub(in crate::grpc::v2) fn is_declared(&self) -> bool {
        self.0.is_some()
    }

    /// What the field expects of the complete object; nothing when it is absent.
    pub(in crate::grpc::v2) fn expected(&self) -> Result<ExpectedHashes, MessageError> {
        Ok(self
            .0
            .as_ref()
            .map(expected_hashes)
            .transpose()?
            .unwrap_or_default())
    }
}

impl From<pb::BidiWriteObjectRequest> for WriteStep {
    fn from(message: pb::BidiWriteObjectRequest) -> Self {
        let pb::ChecksummedData { content, crc32c } = match message.data {
            Some(pb::bidi_write_object_request::Data::ChecksummedData(data)) => data,
            None => pb::ChecksummedData::default(),
        };
        let end = if message.finish_write {
            StepEnd::Finish
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
        Self {
            first: message.first_message,
            params: message.common_object_request_params,
            offset: WriteOffset(message.write_offset),
            data: DeclaredChunk { content, crc32c },
            checksums: DeclaredChecksums(message.object_checksums),
            end,
        }
    }
}

/// Why [`WriteStep::admit`] refuses a message.
#[derive(Debug)]
pub(in crate::grpc::v2) enum AdmitError {
    /// Its data breaks a rule [`upload::admit_bytes`] applies, or its object checksums name a
    /// digest that differs from one declared before.
    Intake(IntakeError),
    /// Its object checksums cannot be read.
    Message(MessageError),
}

impl From<IntakeError> for AdmitError {
    fn from(error: IntakeError) -> Self {
        Self::Intake(error)
    }
}

impl From<MessageError> for AdmitError {
    fn from(error: MessageError) -> Self {
        Self::Message(error)
    }
}

/// A message [`WriteStep::admit`] accepted.
pub(in crate::grpc::v2) struct AdmittedStep<T> {
    /// What the write path's check of the message's data returned.
    pub(in crate::grpc::v2) bytes: T,
    /// The object checksums declared for the complete object, this message's included.
    pub(in crate::grpc::v2) declared: ExpectedHashes,
    pub(in crate::grpc::v2) end: StepEnd,
}

impl WriteStep {
    /// Checks this message, whose `offset` its write path's own rules accepted, by the rules
    /// the non-resumable and resumable paths share, in this order:
    ///
    /// 1. Its data, which `check_bytes` checks against what the write received with
    ///    [`upload::admit_bytes`]: the offset, the size, then the data's CRC32C. A
    ///    non-resumable write checks it through its intake, which also takes the bytes; a
    ///    resumable one against its session's persisted size and the bytes still waiting.
    /// 2. Its object checksums, which must be readable and agree with the ones `declared`
    ///    before it.
    ///
    /// Recorded (case grpc-probe-inferred-session-states), on a resumable stream's first
    /// message: a first offset past the persisted size answers before a malformed `md5_hash`.
    /// Not recorded; inferred from that answer: every message of either path, a non-resumable
    /// write's first message included, reads its object checksums after its data. The
    /// non-resumable recordings fix the order of the first message's offset, size and data
    /// checksum (cases grpc-probe-write-streams and grpc-probe-inferred-session-states), but
    /// none of them sent object checksums.
    pub(in crate::grpc::v2) fn admit<T>(
        self,
        offset: u64,
        declared: ExpectedHashes,
        check_bytes: impl FnOnce(&IntakePiece) -> Result<T, IntakeError>,
    ) -> Result<AdmittedStep<T>, AdmitError> {
        let end = self.end;
        let bytes = check_bytes(&self.data.into_piece(offset, end))?;
        let declared = upload::merge_hashes(declared, self.checksums.expected()?)?;
        Ok(AdmittedStep {
            bytes,
            declared,
            end,
        })
    }
}

fn expected_hashes(value: &pb::ObjectChecksums) -> Result<ExpectedHashes, MessageError> {
    let md5 = if value.md5_hash.is_empty() {
        None
    } else {
        Some(
            value
                .md5_hash
                .as_slice()
                .try_into()
                .map_err(|_| MessageError::Md5Length)?,
        )
    };
    Ok(ExpectedHashes {
        crc32c: value.crc32c,
        md5,
    })
}

/// Reads and decodes the next message of a write stream; `None` once the client closed it.
pub(in crate::grpc::v2) async fn next_step<S>(stream: &mut S) -> Result<Option<WriteStep>, Status>
where
    S: Stream<Item = Result<pb::BidiWriteObjectRequest, Status>> + Unpin,
{
    let Some(message) = stream.next().await.transpose()? else {
        return Ok(None);
    };
    Ok(Some(WriteStep::from(message)))
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let error = MessageError::Md5Length;
    assert_eq!(error.to_string(), "md5_hash must contain exactly 16 bytes");
    assert!(std::error::Error::source(&error).is_none());
}
