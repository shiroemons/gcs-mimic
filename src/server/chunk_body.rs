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

    /// The lost chunk needs a thread switch inside one poll, so this repeats the
    /// producer's send-then-drop on a multi-threaded runtime; `Channel` lost about one
    /// body in 200 000 rounds of this shape on an idle debug build, and more under load.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_last_chunk_survives_a_racing_sender_drop() {
        for _ in 0..20_000 {
            let (sender, body) = channel();
            tokio::spawn(async move {
                let _ = sender.send(Ok(Bytes::from_static(b"selected"))).await;
            });
            assert_eq!(collect(body).await, (b"selected".to_vec(), None));
        }
    }
}
