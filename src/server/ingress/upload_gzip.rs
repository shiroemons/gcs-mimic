//! Upload transport gzip: Java's direct multipart uploader gzips the MIME envelope, not the
//! stored media, and Cloud Storage also decompresses a gzip-encoded media upload body before
//! storing it (observed 2026-10-31). The stored bytes are the decoded body in both cases.
//!
//! Cloud Storage's front end decodes the body before it authenticates the request (recorded
//! 2026-11-25). An upload body can be far larger than a JSON resource, so it streams to its
//! handler instead of being decoded first: its first bytes are checked before dispatch, so a body
//! that is not gzip answers before any check of the caller, and a failure later in the body that
//! the handler reads replaces the handler's answer through [`UploadDecoder`]. A handler that
//! refuses the caller before it reads the body answers without reading the rest, where Cloud
//! Storage would read the body to its end and could then answer for its decoding instead.

use super::{
    DecodedBody, Decoding, RequestBody, RequestCtx, UploadDecoder, bad_request,
    members::{self, Members},
    without_coding,
};
use crate::{
    api_error::ApiError,
    server::{CHUNK_BYTES, chunk_body},
};
use bytes::{Buf, Bytes};
use http_body_util::BodyExt;
use hyper::{
    Request,
    header::{CONTENT_ENCODING, CONTENT_LENGTH, TRANSFER_ENCODING},
};
use std::{
    io::{self, BufRead, Read},
    sync::Arc,
};

/// ID1, ID2 and CM: enough of a gzip member to tell whether the body is one.
const PREFIX_BYTES: usize = 3;

/// The wire and expanded sizes an upload body may reach.
#[derive(Clone, Copy)]
struct Budget {
    compressed: u64,
    expanded: u64,
}

pub(super) async fn request(
    request: Request<RequestBody>,
    ctx: RequestCtx<'_>,
) -> Result<(Request<RequestBody>, UploadDecoder), ApiError> {
    if !matches!(crate::http_header::sole(request.headers(), CONTENT_ENCODING).value(),
        Ok(Some(encoding)) if encoding.as_bytes().eq_ignore_ascii_case(b"gzip"))
    {
        return Err(bad_request(
            "Upload transport Content-Encoding must be one gzip coding",
        ));
    }
    let (mut parts, mut compressed) = request.into_parts();
    let (prefix, ended) = first_bytes(&mut compressed).await?;
    if prefix.is_empty() {
        return Ok((without_coding(parts, Bytes::new()), UploadDecoder(None)));
    }
    if !members::may_start_member(&prefix) || (ended && prefix.len() < PREFIX_BYTES) {
        return Err(ApiError::undecodable_body());
    }
    parts.headers.remove(CONTENT_ENCODING);
    parts.headers.remove(CONTENT_LENGTH);
    parts.headers.remove(TRANSFER_ENCODING);
    parts.headers.remove("trailer");
    let chunk_body::CheckedChannel {
        sender,
        completion,
        body,
    } = chunk_body::checked_channel("Gzip producer ended before validated EOF");
    let decoding = Arc::new(Decoding::default());
    let decoded = DecodedBody {
        body,
        decoding: Arc::clone(&decoding),
    };
    let outcome = UploadDecoder(Some(Arc::clone(&decoding)));
    let expanded = ctx
        .config
        .max_object_bytes()
        .saturating_add(crate::json::MULTIPART_FRAMING_LIMIT);
    // Stored-gzip media can be incompressible; allow bounded framing/deflate overhead on the wire.
    let budget = Budget {
        compressed: expanded
            .saturating_add(expanded / 8)
            .saturating_add(64 * 1024),
        expanded,
    };
    let decoders = super::decoders_for(&parts, ctx).await;
    crate::task::spawn_supervised("gzip-upload-input", async move {
        let result = tokio::select! {
            result = stream(compressed, prefix, &sender, budget, decoders, &decoding) => result,
            _ = sender.closed() => return,
        };
        match result {
            Ok(()) => completion.mark(),
            Err(error) => {
                let _ = sender.send(Err(error)).await;
            }
        }
    });
    Ok((
        Request::from_parts(parts, RequestBody::Decoded(decoded)),
        outcome,
    ))
}

/// The first [`PREFIX_BYTES`] or more of `body`, and whether the body ended within them.
async fn first_bytes(body: &mut RequestBody) -> Result<(Bytes, bool), ApiError> {
    let mut prefix = Bytes::new();
    while prefix.len() < PREFIX_BYTES {
        let Some(frame) = body.frame().await else {
            return Ok((prefix, true));
        };
        let data = frame
            .map_err(|_| bad_request("Could not read compressed request body"))?
            .into_data()
            .map_err(|_| bad_request("Gzip request trailers are unsupported"))?;
        prefix = if prefix.is_empty() {
            data
        } else {
            Bytes::from([prefix.as_ref(), data.as_ref()].concat())
        };
    }
    Ok((prefix, false))
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
        let length = self.fill_buf()?.read(output)?;
        self.consume(length);
        Ok(length)
    }
}

fn within(total: u64, more: usize, limit: u64, refusal: &'static str) -> io::Result<u64> {
    total
        .checked_add(more as u64)
        .filter(|size| *size <= limit)
        .ok_or_else(|| io::Error::other(refusal))
}

const WIRE_REFUSAL: &str = "Compressed upload exceeds its wire budget";

/// Decodes `prefix` and then the rest of `body` into `sender`. A decode failure is recorded in
/// `decoding` before it is returned, so the body that delivers it can tell it apart.
async fn stream(
    mut body: RequestBody,
    prefix: Bytes,
    sender: &chunk_body::ChunkSender,
    budget: Budget,
    decoders: Arc<tokio::sync::Semaphore>,
    decoding: &Decoding,
) -> io::Result<()> {
    let mut decoder = Members::new(Input::default());
    let mut compressed = within(0, prefix.len(), budget.compressed, WIRE_REFUSAL)?;
    let mut pending = prefix;
    let mut expanded = 0_u64;
    loop {
        let permit = decoders
            .clone()
            .acquire_owned()
            .await
            .map_err(io::Error::other)?;
        let (next, output) = crate::task::run_blocking("upload-gzip-decoder", move || {
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
                expanded = within(
                    expanded,
                    bytes.len(),
                    budget.expanded,
                    "Expanded upload exceeds max_object_bytes plus framing budget",
                )?;
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
                            compressed =
                                within(compressed, pending.len(), budget.compressed, WIRE_REFUSAL)?;
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
            // Input only ever blocks, so every other error is a decode failure.
            Err(error) => {
                let _ = decoding.undecodable.set(());
                return Err(error);
            }
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
        let chunk_body::CheckedChannel {
            sender, mut body, ..
        } = chunk_body::checked_channel("producer ended early");
        let producer = crate::task::spawn_supervised("test-gzip-producer", async move {
            sender
                .send(Ok(Bytes::from_static(b"complete MIME boundary")))
                .await
                .unwrap();
            panic!("injected producer panic");
        });
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
        for (input, output) in [(compressed.len() as u64 - 1, 500_000), (500_000, 100)] {
            let (sender, _body) = chunk_body::channel();
            let decoding = Decoding::default();
            let error = stream(
                RequestBody::Buffered(Full::new(compressed.clone())),
                Bytes::new(),
                &sender,
                Budget {
                    compressed: input,
                    expanded: output,
                },
                Arc::new(tokio::sync::Semaphore::new(1)),
                &decoding,
            )
            .await
            .unwrap_err();
            assert!(
                decoding.undecodable.get().is_none(),
                "a budget is not a decode failure"
            );
            assert!(
                error.to_string().contains("budget")
                    || error.to_string().contains("max_object_bytes")
            );
        }
    }
}
