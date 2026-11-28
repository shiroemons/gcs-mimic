//! One bounded producer/catalog protocol; terminal sends never wait ahead of the consumer.

use crate::{
    object::{ExpectedHashes, ObjectError, UploadChunk},
    service::{
        activity::{CatalogWaitLimit, ObjectActivity, begin_catalog_wait},
        body::RequestBody,
        context::ObjectCtx,
    },
    storage::{ChunkWait, admin::AdminError, failure::object_unavailable},
    time_limit::TimeLimit,
};
use http_body_util::BodyExt;
use std::{future::Future, io};
use tokio::sync::{mpsc, watch};

#[cfg(test)]
mod session_wait_tests;
#[cfg(test)]
mod tests;

pub(crate) type UploadSender = mpsc::Sender<Result<UploadChunk, io::Error>>;
pub(crate) type UploadReceiver = mpsc::Receiver<Result<UploadChunk, io::Error>>;

pub(crate) fn upload_channel() -> (UploadSender, UploadReceiver) {
    mpsc::channel(2)
}

pub(crate) enum UploadEnd {
    Eof,
    EofWithHashes(ExpectedHashes),
}

impl From<UploadEnd> for UploadChunk {
    fn from(end: UploadEnd) -> Self {
        match end {
            UploadEnd::Eof => Self::End,
            UploadEnd::EofWithHashes(hashes) => Self::EndWithHashes(hashes),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("the protocol adapter rejected the request body")]
struct AdapterRejectedBody;

pub(crate) trait UploadFailure {
    fn is_body_abort(&self) -> bool;
}
impl UploadFailure for ObjectError {
    fn is_body_abort(&self) -> bool {
        matches!(self, Self::BodyAborted(_))
    }
}
impl UploadFailure for AdminError {
    fn is_body_abort(&self) -> bool {
        matches!(self, Self::BodyAborted(_))
    }
}
impl UploadFailure for crate::storage::ResumableError {
    fn is_body_abort(&self) -> bool {
        matches!(self, Self::Object(error) if error.is_body_abort())
    }
}
impl UploadFailure for crate::storage::xml_multipart::Error {
    fn is_body_abort(&self) -> bool {
        matches!(self, Self::Object(error) if error.is_body_abort())
    }
}

pub(crate) enum CatalogWait<'a> {
    Upload(ObjectCtx<'a>),
    Session {
        ctx: ObjectCtx<'a>,
        progress: watch::Receiver<ChunkWait>,
    },
}

impl CatalogWait<'_> {
    async fn settle<T, E: From<ObjectError>>(
        self,
        operation: impl Future<Output = Result<T, E>>,
    ) -> Result<T, E> {
        match self {
            Self::Upload(ctx) => ctx.await_catalog(operation).await,
            Self::Session { ctx, progress } => {
                await_session_upload(
                    operation,
                    progress,
                    ctx.config.catalog_wait_timeout(),
                    ctx.deadline,
                    ctx.activity,
                )
                .await
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum DriveError<U, P> {
    Upload(U),
    Producer(P),
}
impl<U, P> DriveError<U, P> {
    pub(crate) fn into_error<E: From<U> + From<P>>(self) -> E {
        match self {
            Self::Upload(error) => error.into(),
            Self::Producer(error) => error.into(),
        }
    }
}

pub(crate) async fn drive_upload<T, U, P>(
    wait: CatalogWait<'_>,
    upload: impl Future<Output = Result<T, U>>,
    sender: &UploadSender,
    producer: impl Future<Output = Result<UploadEnd, P>>,
) -> Result<T, DriveError<U, P>>
where
    U: UploadFailure + From<ObjectError>,
{
    tokio::pin!(upload);
    let produced = tokio::select! {
        biased;
        result = &mut upload => return result.map_err(DriveError::Upload),
        produced = producer => produced,
    };
    let (terminal, failure) = match produced {
        Ok(end) => (Ok(end.into()), None),
        Err(error) => (
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                AdapterRejectedBody,
            )),
            Some(error),
        ),
    };
    let result = wait
        .settle(async {
            // The receiver may make progress only when upload is polled, even after production ends.
            let ((), result) = tokio::join!(
                async {
                    let _ = sender.send(terminal).await;
                },
                &mut upload
            );
            result
        })
        .await;
    match (failure, result) {
        (None, result) => result.map_err(DriveError::Upload),
        (Some(_), Err(storage)) if !storage.is_body_abort() => Err(DriveError::Upload(storage)),
        (Some(error), _) => Err(DriveError::Producer(error)),
    }
}

/// Awaits a resumable chunk within the request deadline. Only a wait on the catalog has a
/// budget, and each one starts a new budget; a transfer runs until the request deadline.
async fn await_session_upload<T, E: From<ObjectError>>(
    operation: impl Future<Output = Result<T, E>>,
    mut progress: watch::Receiver<ChunkWait>,
    timeout: TimeLimit,
    deadline: tokio::time::Instant,
    activity: &watch::Sender<ObjectActivity>,
) -> Result<T, E> {
    let _wait = begin_catalog_wait(activity).await;
    tokio::pin!(operation);
    loop {
        let budget = if *progress.borrow_and_update() == ChunkWait::Catalog {
            timeout
        } else {
            TimeLimit::DISABLED
        };
        let limit = CatalogWaitLimit::starting_now(budget, deadline);
        tokio::select! {
            biased;
            result = &mut operation => break result,
            Ok(()) = progress.changed() => {},
            _ = tokio::time::sleep_until(limit.deadline()) => {
                break Err(object_unavailable(limit.expired()).into());
            }
        }
    }
}

/// How `stream_upload` stopped reading a body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Streamed {
    /// The body ended after this many bytes, all forwarded to the catalog.
    Complete(u64),
    /// The catalog stopped accepting the body first and reports its own failure, so the bytes
    /// counted so far say nothing about the body's size.
    Stopped,
}

/// The largest chunk a body producer hands on at once.
pub(crate) const CHUNK_BYTES: usize = 64 * 1024;

/// Streams `body` into the upload channel, at most `max_bytes` of it, and checks the SHA-256 the
/// client declared once the body ends.
pub(crate) async fn stream_upload(
    mut body: RequestBody,
    sender: &UploadSender,
    max_bytes: u64,
    expected_sha256: Option<[u8; 32]>,
) -> Result<Streamed, ObjectError> {
    let mut size = 0_u64;
    let mut sha256 = expected_sha256.map(Sha256Check::new);
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(ObjectError::body_aborted)?;
        let mut bytes = frame
            .into_data()
            .map_err(|_| ObjectError::BodyAborted("Upload trailers are unsupported".into()))?;
        size = size
            .checked_add(bytes.len() as u64)
            .filter(|size| *size <= max_bytes)
            .ok_or(ObjectError::TooLarge)?;
        if let Some(check) = &mut sha256 {
            check.update(&bytes);
        }
        // HTTP/1 frames stay below twice CONNECTION_READ_BUFFER, so passing them on without a
        // copy keeps what the two-slot channel retains bounded; larger frames are still split
        // into 64 KiB chunks.
        while !bytes.is_empty() {
            let part = bytes.split_to(bytes.len().min(CHUNK_BYTES));
            if sender.send(Ok(UploadChunk::Data(part))).await.is_err() {
                // Preserve the catalog's original failure when it stops accepting the body.
                return Ok(Streamed::Stopped);
            }
        }
    }
    sha256.map(Sha256Check::verify).transpose()?;
    Ok(Streamed::Complete(size))
}

/// A SHA-256 the client declared for a body, checked against the bytes as they stream past.
struct Sha256Check {
    hasher: ring::digest::Context,
    expected: [u8; 32],
}

impl Sha256Check {
    fn new(expected: [u8; 32]) -> Self {
        Self {
            hasher: ring::digest::Context::new(&ring::digest::SHA256),
            expected,
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
    }

    /// `ChecksumMismatch` when the streamed bytes hash to another value.
    fn verify(self) -> Result<(), ObjectError> {
        if self.hasher.finish().as_ref() == self.expected {
            Ok(())
        } else {
            Err(ObjectError::ChecksumMismatch)
        }
    }
}
