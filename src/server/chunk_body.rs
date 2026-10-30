//! A response body fed by a producer task.
//!
//! `http_body_util::Channel` ends its body when the sender is gone, but it checks for
//! a queued frame before it checks whether the sender dropped. A producer that sends
//! the last chunk and drops its sender between those two checks loses that chunk, and
//! hyper then closes a response short of its Content-Length. That truncated object
//! downloads under load. An mpsc receiver yields every queued item before it reports
//! closure, and an error travels in order behind the data sent before it.

use bytes::Bytes;
use hyper::body::{Body, Frame};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::mpsc;

pub(crate) type ChunkSender = mpsc::Sender<io::Result<Bytes>>;

/// Holds one chunk ahead of the writer, like the single-slot channel it replaces.
pub(crate) fn channel() -> (ChunkSender, ChunkBody) {
    let (sender, receiver) = mpsc::channel(1);
    (sender, ChunkBody { receiver })
}

pub(crate) struct ChunkBody {
    receiver: mpsc::Receiver<io::Result<Bytes>>,
}

impl Body for ChunkBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        self.receiver
            .poll_recv(cx)
            .map(|item| item.map(|chunk| chunk.map(Frame::data)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    async fn collect(mut body: ChunkBody) -> (Vec<u8>, Option<io::ErrorKind>) {
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            match frame {
                Ok(frame) => bytes.extend_from_slice(&frame.into_data().unwrap()),
                Err(error) => return (bytes, Some(error.kind())),
            }
        }
        (bytes, None)
    }

    #[tokio::test]
    async fn chunks_sent_before_the_sender_drops_all_reach_the_body() {
        let (sender, body) = channel();
        let producer = tokio::spawn(async move {
            for chunk in [&b"first "[..], b"last"] {
                sender.send(Ok(Bytes::from_static(chunk))).await.unwrap();
            }
        });
        assert_eq!(collect(body).await, (b"first last".to_vec(), None));
        producer.await.unwrap();
    }

    #[tokio::test]
    async fn an_error_follows_the_data_sent_before_it() {
        let (sender, body) = channel();
        let producer = tokio::spawn(async move {
            sender
                .send(Ok(Bytes::from_static(b"partial")))
                .await
                .unwrap();
            sender
                .send(Err(io::ErrorKind::UnexpectedEof.into()))
                .await
                .unwrap();
        });
        assert_eq!(
            collect(body).await,
            (b"partial".to_vec(), Some(io::ErrorKind::UnexpectedEof))
        );
        producer.await.unwrap();
    }

    /// Pins the contract `Channel` broke: a chunk queued before the sender drops comes out
    /// before end-of-stream. The sender is gone before the first poll, so the state is
    /// fixed rather than raced. `Channel` lost the chunk only when the drop landed between
    /// two checks inside one poll (about once in 200 000 racing rounds), which no
    /// practical repeat count catches; the regression guard against reintroducing it is
    /// the `disallowed-types` ban on `http_body_util::Channel` in `clippy.toml`.
    #[tokio::test]
    async fn a_chunk_queued_before_the_sender_drops_precedes_end_of_stream() {
        let (sender, mut body) = channel();
        sender
            .try_send(Ok(Bytes::from_static(b"selected")))
            .unwrap();
        drop(sender);
        let frame = body.frame().await.unwrap().unwrap();
        assert_eq!(frame.into_data().unwrap(), Bytes::from_static(b"selected"));
        assert!(body.frame().await.is_none());
    }

    #[tokio::test]
    async fn an_error_queued_before_the_sender_drops_follows_its_data() {
        let (sender, mut body) = channel();
        sender.try_send(Ok(Bytes::from_static(b"partial"))).unwrap();
        let blocked = tokio::spawn(async move {
            sender
                .send(Err(io::ErrorKind::UnexpectedEof.into()))
                .await
                .unwrap();
        });
        let frame = body.frame().await.unwrap().unwrap();
        assert_eq!(frame.into_data().unwrap(), Bytes::from_static(b"partial"));
        // The error is queued and its sender dropped before the next poll.
        blocked.await.unwrap();
        let error = body.frame().await.unwrap().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        assert!(body.frame().await.is_none());
    }
}
