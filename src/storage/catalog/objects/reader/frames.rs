//! Polls the selected generation only when a protocol is ready for another frame.

use super::{BlobError, ObjectReader};
use bytes::Bytes;
use std::{
    pin::Pin,
    task::{Context, Poll},
};

#[derive(Clone, Copy)]
pub(crate) enum FrameLimit {
    Http,
    Grpc,
}

type Read = Pin<Box<dyn Future<Output = (ObjectReader, Result<Option<Bytes>, BlobError>)> + Send>>;

pub(crate) struct ObjectFrames(State);

enum State {
    Idle(ObjectReader),
    Reading(Read),
    Finished,
}

impl From<ObjectReader> for ObjectFrames {
    fn from(reader: ObjectReader) -> Self {
        Self(State::Idle(reader))
    }
}

impl ObjectFrames {
    #[cfg(test)]
    pub(crate) fn panicking_read(message: &'static str) -> Self {
        Self(State::Reading(Box::pin(async move { panic!("{message}") })))
    }

    pub(crate) fn is_finished(&self) -> bool {
        match &self.0 {
            State::Idle(reader) => reader.is_finished(),
            State::Reading(_) => false,
            State::Finished => true,
        }
    }

    pub(crate) fn poll_next(
        &mut self,
        cx: &mut Context<'_>,
        limit: FrameLimit,
    ) -> Poll<Option<Result<Bytes, BlobError>>> {
        let mut read = match std::mem::replace(&mut self.0, State::Finished) {
            State::Finished => return Poll::Ready(None),
            State::Reading(read) => read,
            State::Idle(mut reader) => {
                if reader.is_finished() {
                    return Poll::Ready(None);
                }
                if let Some(bytes) = reader.next_frame_at_hand(limit) {
                    self.0 = State::Idle(reader);
                    return Poll::Ready(Some(Ok(bytes)));
                }
                Box::pin(async move {
                    let result = reader.next_frame(limit).await;
                    (reader, result)
                })
            }
        };
        match read.as_mut().poll(cx) {
            Poll::Pending => {
                self.0 = State::Reading(read);
                Poll::Pending
            }
            Poll::Ready((reader, Ok(Some(bytes)))) => {
                self.0 = State::Idle(reader);
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready((_, result)) => Poll::Ready(result.transpose()),
        }
    }
}
