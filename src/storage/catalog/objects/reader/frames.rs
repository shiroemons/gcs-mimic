//! Polls the selected generation only when a protocol is ready for another frame.

use super::ObjectReader;
use bytes::Bytes;
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

#[derive(Clone, Copy)]
pub(crate) enum FrameLimit {
    Http,
    Grpc,
}

type Read = Pin<Box<dyn Future<Output = (ObjectReader, io::Result<Option<Bytes>>)> + Send>>;

pub(crate) enum ObjectFrames {
    Idle(ObjectReader),
    Reading(Read),
    Finished,
}

impl ObjectFrames {
    pub(crate) fn is_finished(&self) -> bool {
        match self {
            Self::Idle(reader) => reader.is_finished(),
            Self::Reading(_) => false,
            Self::Finished => true,
        }
    }

    pub(crate) fn poll_next(
        &mut self,
        cx: &mut Context<'_>,
        limit: FrameLimit,
    ) -> Poll<Option<io::Result<Bytes>>> {
        let mut read = match std::mem::replace(self, Self::Finished) {
            Self::Finished => return Poll::Ready(None),
            Self::Reading(read) => read,
            Self::Idle(mut reader) => {
                if reader.is_finished() {
                    return Poll::Ready(None);
                }
                if let Some(bytes) = reader.next_frame_at_hand(limit) {
                    *self = Self::Idle(reader);
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
                *self = Self::Reading(read);
                Poll::Pending
            }
            Poll::Ready((reader, Ok(Some(bytes)))) => {
                *self = Self::Idle(reader);
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready((_, result)) => Poll::Ready(result.transpose()),
        }
    }
}
