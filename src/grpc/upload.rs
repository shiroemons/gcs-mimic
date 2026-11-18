use crate::{
    hashing::Hasher,
    object::{ChecksumConflict, ExpectedHashes, ObjectError, ObjectRecord, UploadChunk},
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

pub(super) enum EndMarker {
    Plain,
    WithHashes,
}

pub(super) struct IntakePiece {
    pub offset: u64,
    pub content: Bytes,
    pub crc32c: Option<u32>,
    pub finish: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum IntakeError {
    Offset,
    ChunkCrc {
        computed: u32,
        client: u32,
    },
    Overflow,
    TooLarge,
    /// `written` bytes cannot make an object of the declared `total`.
    SizeMismatch {
        written: u64,
        total: u64,
    },
    ObjectChecksum,
    ConflictingChecksums,
    ConsumerStopped,
}

pub(super) struct Intake {
    offset: u64,
    expected: ExpectedHashes,
    hasher: Hasher,
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
            hasher: Hasher::default(),
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

    /// Checks `piece` against the bytes received so far and returns the bytes it adds, for the
    /// caller to stage. Cloud Storage answers a wrong size before a wrong data checksum
    /// (recorded 2026-11-18), so the checksum is verified last.
    pub(super) fn admit(&mut self, piece: &IntakePiece) -> Result<Bytes, IntakeError> {
        if piece.offset > self.offset
            || (matches!(self.resend, Resend::Reject) && piece.offset != self.offset)
        {
            return Err(IntakeError::Offset);
        }
        let discard = usize::try_from(self.offset - piece.offset)
            .unwrap_or(usize::MAX)
            .min(piece.content.len());
        let added = piece.content.slice(discard..);
        let written = u64::try_from(added.len())
            .ok()
            .and_then(|length| self.offset.checked_add(length))
            .ok_or(IntakeError::Overflow)?;
        if written > self.max_bytes {
            return Err(IntakeError::TooLarge);
        }
        if let Some(total) = self.total
            && (written > total || (piece.finish && written != total))
        {
            return Err(IntakeError::SizeMismatch { written, total });
        }
        verify_crc32c(&piece.content, piece.crc32c)?;
        self.offset = written;
        self.hasher.update(&added);
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

    pub(super) async fn finish(self, sender: &Sender, end: EndMarker) -> Result<(), IntakeError> {
        self.expected
            .verify(&self.hasher.finish())
            .map_err(|_| IntakeError::ObjectChecksum)?;
        let end = match end {
            EndMarker::Plain => UploadChunk::End,
            EndMarker::WithHashes => UploadChunk::EndWithHashes(self.expected),
        };
        sender
            .send(Ok(end))
            .await
            .map_err(|_| IntakeError::ConsumerStopped)
    }
}

/// Content whose CRC32C (`computed`) differs from the one the client declared for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CrcMismatch {
    pub computed: u32,
    pub client: u32,
}

impl From<CrcMismatch> for IntakeError {
    fn from(CrcMismatch { computed, client }: CrcMismatch) -> Self {
        Self::ChunkCrc { computed, client }
    }
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
