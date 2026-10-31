//! Java's direct multipart uploader gzips the MIME envelope, not the stored media.

use super::{RequestBody, RequestCtx, bad_request};
use crate::{
    json::JsonError,
    server::{ObjectActivity, chunk_body},
};
use bytes::{Buf, Bytes};
use flate2::bufread::MultiGzDecoder;
use http_body_util::BodyExt;
use hyper::{
    Request,
    body::{Body, Frame},
    header::{CONTENT_ENCODING, CONTENT_LENGTH, TRANSFER_ENCODING},
};
use std::{
    io::{self, BufRead, Read},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};
use tokio::sync::watch;

const CHUNK_BYTES: usize = 64 * 1024;

/// The explicit completion flag prevents a failed producer from appearing as clean EOF.
pub(crate) struct DecodedBody {
    body: chunk_body::ChunkBody,
    complete: Arc<AtomicBool>,
    ended: bool,
}

impl Body for DecodedBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        if self.ended {
            return Poll::Ready(None);
        }
        match Pin::new(&mut self.body).poll_frame(cx) {
            Poll::Ready(None) => {
                self.ended = true;
                if self.complete.load(Ordering::Acquire) {
                    Poll::Ready(None)
                } else {
                    Poll::Ready(Some(Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Gzip producer ended before validated EOF",
                    ))))
                }
            }
            frame => frame,
        }
    }
}

pub(super) fn request(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<Request<RequestBody>, JsonError> {
    let mut encodings = request.headers().get_all(CONTENT_ENCODING).iter();
    if encodings
        .next()
        .is_none_or(|value| !value.as_bytes().eq_ignore_ascii_case(b"gzip"))
        || encodings.next().is_some()
    {
        return Err(bad_request(
            "Multipart transport Content-Encoding must be one gzip coding",
        ));
    }
    ctx.state
        .auth
        .authenticate_at(
            request.headers(),
            request.uri().query(),
            ctx.state.clock.as_ref(),
        )
        .map_err(JsonError::from)?;
    let (mut parts, body) = request.into_parts();
    parts.headers.remove(CONTENT_ENCODING);
    parts.headers.remove(CONTENT_LENGTH);
    parts.headers.remove(TRANSFER_ENCODING);
    parts.headers.remove("trailer");
    let (sender, decoded) = chunk_body::channel();
    let complete = Arc::new(AtomicBool::new(false));
    let producer_complete = complete.clone();
    let activity = ctx.activity.clone();
    let expanded_limit = ctx.config.max_object_bytes.saturating_add(256 * 1024);
    // Stored-gzip media can be incompressible; allow bounded framing/deflate overhead on the wire.
    let compressed_limit = expanded_limit
        .saturating_add(expanded_limit / 8)
        .saturating_add(64 * 1024);
    crate::task::spawn_supervised("gzip-multipart-input", async move {
        let result = tokio::select! {
            result = stream(body, &sender, &activity, compressed_limit, expanded_limit) => result,
            _ = sender.closed() => return,
        };
        match result {
            Ok(()) => producer_complete.store(true, Ordering::Release),
            Err(error) => {
                let _ = sender.send(Err(error)).await;
            }
        }
    });
    Ok(Request::from_parts(
        parts,
        RequestBody::Decoded(DecodedBody {
            body: decoded,
            complete,
            ended: false,
        }),
    ))
}

#[derive(Default)]
struct Input {
    bytes: Bytes,
    eof: bool,
}

impl BufRead for Input {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.bytes.is_empty() && !self.eof {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        Ok(&self.bytes)
    }
    fn consume(&mut self, length: usize) {
        self.bytes.advance(length);
    }
}

impl Read for Input {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let bytes = self.fill_buf()?;
        let length = bytes.len().min(output.len());
        output[..length].copy_from_slice(&bytes[..length]);
        self.consume(length);
        Ok(length)
    }
}

async fn stream(
    mut body: RequestBody,
    sender: &chunk_body::ChunkSender,
    activity: &watch::Sender<ObjectActivity>,
    compressed_limit: u64,
    expanded_limit: u64,
) -> io::Result<()> {
    let mut decoder = MultiGzDecoder::new(Input::default());
    let mut pending = Bytes::new();
    let (mut compressed, mut expanded) = (0_u64, 0_u64);
    loop {
        let permit = super::DECODERS.acquire().await.map_err(io::Error::other)?;
        let (next, output) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut output = vec![0; CHUNK_BYTES];
            let result = decoder.read(&mut output).map(|length| {
                output.truncate(length);
                Bytes::from(output)
            });
            (decoder, result)
        })
        .await
        .map_err(io::Error::other)?;
        decoder = next;
        match output {
            Ok(bytes) if bytes.is_empty() => return Ok(()),
            Ok(bytes) => {
                expanded = expanded
                    .checked_add(bytes.len() as u64)
                    .filter(|size| *size <= expanded_limit)
                    .ok_or_else(|| {
                        io::Error::other(
                            "Expanded multipart exceeds max_object_bytes plus framing budget",
                        )
                    })?;
                if sender.send(Ok(bytes)).await.is_err() {
                    return Ok(());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                while pending.is_empty() {
                    match body.frame().await {
                        Some(Ok(frame)) => {
                            pending = frame.into_data().map_err(|_| {
                                io::Error::other("Gzip request trailers are unsupported")
                            })?;
                            compressed = compressed
                                .checked_add(pending.len() as u64)
                                .filter(|size| *size <= compressed_limit)
                                .ok_or_else(|| {
                                    io::Error::other("Compressed multipart exceeds its wire budget")
                                })?;
                            super::super::body_progress(activity);
                        }
                        Some(Err(error)) => return Err(error),
                        None => {
                            decoder.get_mut().eof = true;
                            break;
                        }
                    }
                }
                decoder.get_mut().bytes = pending.split_to(pending.len().min(CHUNK_BYTES));
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::Full;
    use std::io::Write;

    #[tokio::test]
    async fn a_panicking_producer_never_becomes_validated_eof() {
        let (sender, body) = chunk_body::channel();
        let producer = crate::task::spawn_supervised("test-gzip-producer", async move {
            sender
                .send(Ok(Bytes::from_static(b"complete MIME boundary")))
                .await
                .unwrap();
            panic!("injected producer panic");
        });
        let mut body = DecodedBody {
            body,
            complete: Arc::new(AtomicBool::new(false)),
            ended: false,
        };
        assert_eq!(
            body.frame().await.unwrap().unwrap().into_data().unwrap(),
            "complete MIME boundary"
        );
        assert!(producer.await.unwrap_err().is_panic());
        assert_eq!(
            body.frame().await.unwrap().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert!(body.frame().await.is_none());
    }

    #[tokio::test]
    async fn decoder_bounds_compressed_and_expanded_streams() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&vec![b'x'; 200_000]).unwrap();
        let compressed = Bytes::from(encoder.finish().unwrap());
        let (activity, _) = watch::channel(ObjectActivity::default());
        for (input, output) in [(compressed.len() as u64 - 1, 500_000), (500_000, 100)] {
            let (sender, _body) = chunk_body::channel();
            let error = stream(
                RequestBody::Buffered(Full::new(compressed.clone())),
                &sender,
                &activity,
                input,
                output,
            )
            .await
            .unwrap_err();
            assert!(
                error.to_string().contains("budget")
                    || error.to_string().contains("max_object_bytes")
            );
        }
    }
}
