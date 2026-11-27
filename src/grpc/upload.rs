use crate::{
    hashing::Hashes,
    object::{
        ChecksumConflict, ExpectedHashes, HashMismatch, ObjectError, ObjectRecord, UploadChunk,
    },
};
use bytes::Bytes;
use tokio::sync::mpsc;
use tonic::{
    Status,
    codegen::tokio_stream::{Stream, StreamExt},
};

#[cfg(test)]
mod tests;

pub(super) const CHUNK_BYTES: usize = 64 * 1024;

/// Slices share the decoded message; its 4 MiB limit and the bounded channel cap memory.
pub(crate) fn chunks(data: &Bytes) -> impl Iterator<Item = Bytes> + '_ {
    (0..data.len())
        .step_by(CHUNK_BYTES)
        .map(|start| data.slice(start..data.len().min(start + CHUNK_BYTES)))
}

type Sender = mpsc::Sender<Result<UploadChunk, std::io::Error>>;

#[derive(Clone, Copy)]
pub(super) enum Resend {
    Reject,
    DiscardOverlap,
}

pub(super) struct IntakePiece {
    pub(crate) offset: u64,
    pub(crate) content: Bytes,
    pub(crate) crc32c: Option<u32>,
    pub(crate) finish: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IntakeError {
    Offset,
    ChunkCrc(CrcMismatch),
    Overflow,
    TooLarge,
    /// `written` bytes cannot make an object of the declared `total`.
    SizeMismatch {
        written: u64,
        total: u64,
    },
    /// The complete object's digest differs from the one the client declared for it.
    ObjectChecksum(HashMismatch),
    ConflictingChecksums,
    ConsumerStopped,
}

pub(super) struct Intake {
    offset: u64,
    expected: ExpectedHashes,
    max_bytes: u64,
    total: Option<u64>,
    resend: Resend,
}

impl Intake {
    pub(super) fn new(
        expected: ExpectedHashes,
        max_bytes: u64,
        total: Option<u64>,
        resend: Resend,
    ) -> Self {
        Self {
            offset: 0,
            expected,
            max_bytes,
            total,
            resend,
        }
    }

    pub(super) fn offset(&self) -> u64 {
        self.offset
    }

    pub(super) fn checksums(&mut self, hashes: ExpectedHashes) -> Result<(), IntakeError> {
        self.expected = merge_hashes(self.expected, hashes)?;
        Ok(())
    }

    /// Checks `piece` against the bytes received so far with [`admit_bytes`] and returns the
    /// bytes it adds, for the caller to stage.
    pub(super) fn admit(&mut self, piece: &IntakePiece) -> Result<Bytes, IntakeError> {
        if matches!(self.resend, Resend::Reject) && piece.offset != self.offset {
            return Err(IntakeError::Offset);
        }
        let (added, written) = admit_bytes(self.offset, piece, self.max_bytes, self.total)?;
        self.offset = written;
        Ok(added)
    }

    pub(super) async fn accept(
        &mut self,
        piece: IntakePiece,
        sender: &Sender,
    ) -> Result<(), IntakeError> {
        let added = self.admit(&piece)?;
        stage(sender, &added).await
    }

    pub(super) async fn finish(self, sender: &Sender) -> Result<(), IntakeError> {
        sender
            .send(Ok(UploadChunk::EndWithHashes(self.expected)))
            .await
            .map_err(|_| IntakeError::ConsumerStopped)
    }
}

/// Content whose CRC32C (`computed`) differs from the one the client declared for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CrcMismatch {
    pub(crate) computed: u32,
    pub(crate) client: u32,
}

impl From<CrcMismatch> for IntakeError {
    fn from(mismatch: CrcMismatch) -> Self {
        Self::ChunkCrc(mismatch)
    }
}

/// Checks `piece` against the `received` bytes of an upload and returns the bytes it adds, with
/// the received size once they are added. Every write path that receives checksummed data in
/// messages applies these rules through this one function, in this order:
///
/// 1. The piece may repeat received bytes, which are dropped, but may not start past them.
///    Recorded (case grpc-probe-inferred-granules): a resumed stream's first message at 0 or
///    inside the persisted bytes is accepted and leaves the persisted size as it was; the bytes
///    it repeated matched the persisted ones. Not recorded; the emulator's choice: the
///    repeated bytes are not compared with the received ones. They count only in the data's
///    CRC32C (rule 5), so a piece that repeats different bytes is accepted and the received
///    bytes win, on a resumable and on a non-resumable write alike.
/// 2. The new size must fit a signed 64-bit offset, as every size on the wire does.
/// 3. The size may not pass `max` (`--max-object-bytes`). The limit is the emulator's own, so
///    Cloud Storage has no answer to record; checking it before the declared size and the data
///    checksum is the emulator's choice.
/// 4. With a declared `total` (`object_size`), the bytes may not pass it, and a finishing piece
///    must reach it exactly.
/// 5. The data's declared CRC32C. Cloud Storage answered a wrong `object_size` before a wrong
///    data checksum (recorded 2026-11-18), so the checksum is verified last.
pub(super) fn admit_bytes(
    received: u64,
    piece: &IntakePiece,
    max: u64,
    total: Option<u64>,
) -> Result<(Bytes, u64), IntakeError> {
    if piece.offset > received {
        return Err(IntakeError::Offset);
    }
    let repeated = usize::try_from(received - piece.offset)
        .unwrap_or(usize::MAX)
        .min(piece.content.len());
    let added = piece.content.slice(repeated..);
    let written = u64::try_from(added.len())
        .ok()
        .and_then(|length| received.checked_add(length))
        .filter(|written| i64::try_from(*written).is_ok())
        .ok_or(IntakeError::Overflow)?;
    if written > max {
        return Err(IntakeError::TooLarge);
    }
    if let Some(total) = total
        && (written > total || (piece.finish && written != total))
    {
        return Err(IntakeError::SizeMismatch { written, total });
    }
    verify_crc32c(&piece.content, piece.crc32c)?;
    Ok((added, written))
}

/// Checks the `computed` digests of a complete object against the checksums its client declared
/// for it (`expected`).
pub(super) fn verify_object(
    expected: &ExpectedHashes,
    computed: &Hashes,
) -> Result<(), IntakeError> {
    expected
        .verify(computed)
        .map_err(IntakeError::ObjectChecksum)
}

pub(super) fn verify_crc32c(content: &[u8], client: Option<u32>) -> Result<(), CrcMismatch> {
    if let Some(client) = client {
        let computed = crc32c::crc32c(content);
        if computed != client {
            return Err(CrcMismatch { computed, client });
        }
    }
    Ok(())
}

pub(super) fn merge_hashes(
    first: ExpectedHashes,
    next: ExpectedHashes,
) -> Result<ExpectedHashes, IntakeError> {
    first
        .merge(next)
        .map_err(|ChecksumConflict(_)| IntakeError::ConflictingChecksums)
}

pub(super) async fn stage(sender: &Sender, data: &Bytes) -> Result<(), IntakeError> {
    for part in chunks(data) {
        sender
            .send(Ok(UploadChunk::Data(part)))
            .await
            .map_err(|_| IntakeError::ConsumerStopped)?;
    }
    Ok(())
}

/// A failed consumer must wake a producer waiting for another client message.
pub(super) async fn next<S: Stream + Unpin>(
    stream: &mut S,
    sender: &Sender,
) -> Result<Option<S::Item>, IntakeError> {
    tokio::select! {
        biased;
        () = sender.closed() => Err(IntakeError::ConsumerStopped),
        message = stream.next() => Ok(message),
    }
}

/// Await both futures before selecting the root storage failure or the producer's input error.
pub(super) fn completed(
    sent: Result<(), Status>,
    written: Result<ObjectRecord, ObjectError>,
    status: fn(ObjectError) -> Status,
) -> Result<ObjectRecord, Status> {
    match written {
        Ok(record) => {
            sent?;
            Ok(record)
        }
        Err(error) => {
            if matches!(error, ObjectError::BodyAborted(_)) {
                sent?;
            }
            Err(status(error))
        }
    }
}
