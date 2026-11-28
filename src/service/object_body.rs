//! A response body that reads its object only when the connection can send more.
//!
//! A producer task with a channel between it and the connection cost a task switch per chunk, and
//! a wakeup of another worker whenever the scheduler chose to share the load, which is expensive
//! inside a VM. It also held a chunk in the channel beside the one being written. Pulling instead
//! keeps one frame in flight, and bytes already in memory or the page cache need no switch at all.
//! A client that stops reading stops the polling, so nothing is read ahead of it.

use crate::cause::IoCause;
use crate::service::activity::{ObjectActivity, record_progress};
use crate::service::body::ResponseBody;
use crate::storage::{FrameLimit, ObjectFrames, ObjectReader};
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::body::{Body, Frame};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::watch;

pub(crate) struct ObjectBody {
    frames: ObjectFrames,
    activity: watch::Sender<ObjectActivity>,
}

/// The response body of a media read, which sends `reader`'s bytes and records each frame as
/// the request's progress.
pub(crate) fn object_body(
    reader: ObjectReader,
    activity: &watch::Sender<ObjectActivity>,
) -> ResponseBody {
    ObjectBody::new(reader, activity.clone()).boxed_unsync()
}

impl ObjectBody {
    pub(crate) fn new(reader: ObjectReader, activity: watch::Sender<ObjectActivity>) -> Self {
        Self {
            frames: reader.into(),
            activity,
        }
    }

    fn data_frame(&self, bytes: Bytes) -> Frame<Bytes> {
        record_progress(&self.activity);
        Frame::data(bytes)
    }
}

/// hyper reports only that the body was aborted, so this names the read failure.
fn report_read_failure(error: &IoCause) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        crate::diagnostics::Event::ObjectBodyReadFailed,
        serde_json::json!({
            "message": "object data read failed after the response started; the client receives a truncated body",
            "cause": error.kind().to_string(),
        }),
    );
}

impl Body for ObjectBody {
    type Data = Bytes;
    type Error = IoCause;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        this.frames.poll_next(cx, FrameLimit::Http).map(|item| {
            item.map(|result| {
                result
                    .map_err(IoCause::from)
                    .inspect_err(report_read_failure)
                    .map(|bytes| this.data_frame(bytes))
            })
        })
    }

    /// Lets hyper finish a response in the same write as its last frame.
    fn is_end_stream(&self) -> bool {
        self.frames.is_finished()
    }
}

#[cfg(test)]
mod tests;
