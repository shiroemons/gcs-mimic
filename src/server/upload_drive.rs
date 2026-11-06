//! One bounded producer/catalog protocol; terminal sends never wait ahead of the consumer.

use super::ObjectCtx;
use crate::{
    json::{self, JsonError},
    object::{ExpectedHashes, ObjectError, UploadChunk},
    storage::admin::AdminError,
};
use std::{fmt, future::Future, io};
use tokio::sync::{mpsc, watch};

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

#[derive(Debug)]
struct AdapterRejectedBody;

impl fmt::Display for AdapterRejectedBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the protocol adapter rejected the request body")
    }
}
impl std::error::Error for AdapterRejectedBody {}

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
impl UploadFailure for JsonError {
    fn is_body_abort(&self) -> bool {
        self.kind == json::ErrorKind::IncompleteBody
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
        progress: watch::Receiver<bool>,
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
                super::resumable::await_session_upload(
                    operation,
                    progress,
                    ctx.config.catalog_wait_timeout,
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

#[cfg(test)]
mod tests;
