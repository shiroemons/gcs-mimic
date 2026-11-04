use crate::{
    hashing::Hasher,
    object::{ExpectedHashes, ObjectError, ObjectRecord, UploadChunk},
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
    ChunkCrc { computed: u32, client: u32 },
    Overflow,
    TooLarge,
    SizeMismatch,
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

    pub(super) async fn accept(
        &mut self,
        piece: IntakePiece,
        sender: &Sender,
    ) -> Result<(), IntakeError> {
        if piece.offset > self.offset
            || (matches!(self.resend, Resend::Reject) && piece.offset != self.offset)
        {
            return Err(IntakeError::Offset);
        }
        verify_crc32c(&piece.content, piece.crc32c)?;
        let discard = usize::try_from(self.offset - piece.offset)
            .unwrap_or(usize::MAX)
            .min(piece.content.len());
        let added = piece.content.slice(discard..);
        self.offset = u64::try_from(added.len())
            .ok()
            .and_then(|length| self.offset.checked_add(length))
            .ok_or(IntakeError::Overflow)?;
        if self.offset > self.max_bytes {
            return Err(IntakeError::TooLarge);
        }
        if self
            .total
            .is_some_and(|total| self.offset > total || (piece.finish && self.offset != total))
        {
            return Err(IntakeError::SizeMismatch);
        }
        self.hasher.update(&added);
        stage(sender, &added).await
    }

    pub(super) async fn finish(self, sender: &Sender, end: EndMarker) -> Result<(), IntakeError> {
        let actual = self.hasher.finish();
        if self.expected.md5.is_some_and(|md5| md5 != actual.md5)
            || self.expected.crc32c.is_some_and(|crc| crc != actual.crc32c)
        {
            return Err(IntakeError::ObjectChecksum);
        }
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

pub(super) fn verify_crc32c(content: &[u8], client: Option<u32>) -> Result<(), IntakeError> {
    if let Some(client) = client {
        let computed = crc32c::crc32c(content);
        if computed != client {
            return Err(IntakeError::ChunkCrc { computed, client });
        }
    }
    Ok(())
}

pub(super) fn merge_hashes(
    first: ExpectedHashes,
    next: ExpectedHashes,
) -> Result<ExpectedHashes, IntakeError> {
    if first.md5.zip(next.md5).is_some_and(|(a, b)| a != b)
        || first.crc32c.zip(next.crc32c).is_some_and(|(a, b)| a != b)
    {
        return Err(IntakeError::ConflictingChecksums);
    }
    Ok(ExpectedHashes {
        md5: first.md5.or(next.md5),
        crc32c: first.crc32c.or(next.crc32c),
    })
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
