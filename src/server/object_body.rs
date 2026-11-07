//! A response body that reads its object only when the connection can send more.
//!
//! A producer task with a channel between it and the connection cost a task switch per chunk, and
//! a wakeup of another worker whenever the scheduler chose to share the load, which is expensive
//! inside a VM. It also held a chunk in the channel beside the one being written. Pulling instead
//! keeps one frame in flight, and bytes already in memory or the page cache need no switch at all.
//! A client that stops reading stops the polling, so nothing is read ahead of it.

use super::{ObjectActivity, record_progress};
use crate::storage::ObjectReader;
use bytes::Bytes;
use hyper::body::{Body, Frame};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::watch;

type Read = Pin<Box<dyn Future<Output = (ObjectReader, io::Result<Option<Bytes>>)> + Send>>;

enum State {
    Idle(ObjectReader),
    /// The bytes were not at hand, so a blocking read owns the reader until it returns it.
    Reading(Read),
    Finished,
}

pub(super) struct ObjectBody {
    state: State,
    activity: watch::Sender<ObjectActivity>,
}

impl ObjectBody {
    pub(super) fn new(reader: ObjectReader, activity: watch::Sender<ObjectActivity>) -> Self {
        Self {
            state: State::Idle(reader),
            activity,
        }
    }

    fn data_frame(&self, bytes: Bytes) -> Option<io::Result<Frame<Bytes>>> {
        record_progress(&self.activity);
        Some(Ok(Frame::data(bytes)))
    }
}

/// hyper reports only that the body was aborted, so this names the read failure.
fn report_read_failure(error: &io::Error) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        "object_body_read_failed",
        serde_json::json!({
            "message": "object data read failed after the response started; the client receives a truncated body",
            "cause": error.kind().to_string(),
        }),
    );
}

impl Body for ObjectBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        let mut read = match std::mem::replace(&mut this.state, State::Finished) {
            State::Finished => return Poll::Ready(None),
            State::Reading(read) => read,
            State::Idle(mut reader) => {
                if reader.is_finished() {
                    return Poll::Ready(None);
                }
                if let Some(bytes) = reader.next_frame_at_hand() {
                    this.state = State::Idle(reader);
                    return Poll::Ready(this.data_frame(bytes));
                }
                Box::pin(async move {
                    let result = reader.next_frame().await;
                    (reader, result)
                })
            }
        };
        match read.as_mut().poll(cx) {
            Poll::Pending => {
                this.state = State::Reading(read);
                Poll::Pending
            }
            Poll::Ready((reader, Ok(Some(bytes)))) => {
                this.state = State::Idle(reader);
                Poll::Ready(this.data_frame(bytes))
            }
            Poll::Ready((_, Ok(None))) => Poll::Ready(None),
            Poll::Ready((_, Err(error))) => {
                report_read_failure(&error);
                Poll::Ready(Some(Err(error)))
            }
        }
    }

    /// Lets hyper finish a response in the same write as its last frame.
    fn is_end_stream(&self) -> bool {
        match &self.state {
            State::Idle(reader) => reader.is_finished(),
            State::Reading(_) => false,
            State::Finished => true,
        }
    }
}

#[cfg(test)]
mod tests;
