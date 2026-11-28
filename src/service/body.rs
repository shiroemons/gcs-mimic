//! The request body every adapter reads and the response body every adapter answers with.

use crate::{
    cause::IoCause,
    service::{
        activity::{BodyRead, ObjectActivity},
        chunk_body::CheckedBody,
    },
};
use http_body_util::{BodyExt, Full, combinators::UnsyncBoxBody};
use hyper::{
    Response, StatusCode,
    body::{Body, Bytes, Frame, Incoming, SizeHint},
};
use std::{
    io,
    pin::Pin,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
};
use tokio::sync::watch;

pub(crate) type ResponseBody = UnsyncBoxBody<Bytes, IoCause>;

pub(crate) fn boxed(response: Response<Full<Bytes>>) -> Response<ResponseBody> {
    response.map(|body| body.map_err(|never| match never {}).boxed_unsync())
}

pub(crate) fn empty_response(status: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response
}

/// Synthetic batch parts and decoded metadata use the same dispatch as network input.
pub(crate) enum RequestBody {
    Transport(TransportBody),
    Buffered(Full<Bytes>),
    Decoded(DecodedBody),
}

/// A request body arriving from the client. Every adapter reads through here, so the
/// connection's watchdog learns from one place when a request is waiting on the client:
/// a stalled body on any route is then bounded by body inactivity, not by the request deadline.
#[must_use = "dropping the body ends transport body-read tracking"]
pub(crate) struct TransportBody<B = Incoming> {
    body: B,
    activity: watch::Sender<ObjectActivity>,
}

impl<B> TransportBody<B> {
    fn report(&self, read: BodyRead) {
        self.activity
            .send_if_modified(|progress| progress.set_body_read(read));
    }
}

impl<B> Drop for TransportBody<B> {
    fn drop(&mut self) {
        self.report(BodyRead::NotPending);
    }
}

impl<B> Body for TransportBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.body).poll_frame(cx);
        match &polled {
            // Only here does the request wait on the client, so only this starts its silence.
            Poll::Pending => this.report(BodyRead::Pending),
            Poll::Ready(Some(Ok(_))) => {
                // A delivered frame is the server's until it asks for the next one, so work
                // between frames, such as a write that waits for the disk, counts against the
                // request deadline instead of the client's silence. Notifying even when the
                // state is unchanged makes every frame renew the limit.
                this.activity.send_modify(|progress| {
                    progress.set_body_read(BodyRead::NotPending);
                });
            }
            Poll::Ready(Some(Err(_)) | None) => this.report(BodyRead::NotPending),
        }
        // xtask-lint: allow(text::io_error_other) -- The request body exposes io::Error and retains the transport error as its payload.
        polled.map(|frame| frame.map(|result| result.map_err(io::Error::other)))
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

impl RequestBody {
    pub(crate) fn transport(body: Incoming, activity: watch::Sender<ObjectActivity>) -> Self {
        Self::Transport(TransportBody { body, activity })
    }
}

impl Body for RequestBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        match self.get_mut() {
            Self::Transport(body) => Pin::new(body).poll_frame(cx),
            Self::Buffered(body) => Pin::new(body)
                .poll_frame(cx)
                .map(|frame| frame.map(|result| result.map_err(|never| match never {}))),
            Self::Decoded(body) => Pin::new(body).poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            Self::Transport(body) => body.is_end_stream(),
            Self::Buffered(body) => body.is_end_stream(),
            Self::Decoded(body) => body.is_end_stream(),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            Self::Transport(body) => body.size_hint(),
            Self::Buffered(body) => body.size_hint(),
            Self::Decoded(body) => body.size_hint(),
        }
    }
}

/// What became of a streamed upload's decoding, shared by its decoder, its body and the ingress
/// that settles the response.
#[derive(Default)]
pub(crate) struct Decoding {
    /// The decoder found that the body does not decode.
    undecodable: OnceLock<()>,
    /// The handler read that failure from the body.
    reported: OnceLock<()>,
}

impl Decoding {
    /// Records that the body does not decode. The decoder records it before it sends the error
    /// that reports it, so the body can tell that error from any other.
    pub(crate) fn mark_undecodable(&self) {
        let _ = self.undecodable.set(());
    }

    pub(crate) fn is_undecodable(&self) -> bool {
        self.undecodable.get().is_some()
    }

    /// Whether the handler read the decode failure from the body.
    pub(crate) fn is_reported(&self) -> bool {
        self.reported.get().is_some()
    }
}

/// The decoded body of a streamed upload.
pub(crate) struct DecodedBody {
    body: CheckedBody,
    decoding: Arc<Decoding>,
}

impl DecodedBody {
    pub(crate) fn new(body: CheckedBody, decoding: Arc<Decoding>) -> Self {
        Self { body, decoding }
    }
}

impl Body for DecodedBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.body).poll_frame(cx);
        // The decoder records the failure before it sends the error that reports it, so an
        // error read after the record is that failure.
        if matches!(polled, Poll::Ready(Some(Err(_)))) && this.decoding.is_undecodable() {
            let _ = this.decoding.reported.set(());
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.body.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}

#[cfg(test)]
mod tests;
