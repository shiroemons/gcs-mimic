//! Incremental signed-chunk decoding keeps unverified bytes in the existing staging upload.

use super::*;
use crate::{auth::signature::Streaming, server::Streamed};
use crate::{object::UploadChunk, server::upload_drive::UploadSender};
use http_body_util::BodyExt;
use ring::digest;

#[cfg(test)]
mod tests;

fn malformed() -> Error {
    Error::invalid("Invalid signed chunk framing or decoded length.")
}

pub(super) fn stored_encoding(headers: &HeaderMap) -> Result<Patch<String>, Error> {
    let Some(value) = single(headers, "content-encoding")? else {
        return Ok(Patch::Keep);
    };
    let mut remaining = Vec::new();
    let mut found = false;
    for token in value.split(',').map(str::trim) {
        if token.is_empty()
            || !token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        {
            return Err(malformed());
        }
        if token.eq_ignore_ascii_case("aws-chunked") {
            if found {
                return Err(malformed());
            }
            found = true;
        } else {
            remaining.push(token);
        }
    }
    Ok(Patch::nullable(
        (!remaining.is_empty()).then(|| remaining.join(",")),
    ))
}

/// What the request headers declare about the body.
#[derive(Clone, Copy)]
pub(super) struct Declared {
    pub(super) decoded_length: Option<u64>,
    pub(super) sha256: Option<[u8; 32]>,
    pub(super) length_range: Option<LengthRange>,
}

pub(super) async fn upload(
    body: Incoming,
    sender: &UploadSender,
    ctx: ObjectCtx<'_>,
    streaming: Option<Streaming>,
    declared: Declared,
) -> Result<(), Error> {
    let Declared {
        decoded_length,
        sha256,
        length_range,
    } = declared;
    let Some(streaming) = streaming else {
        if let Some(length) = decoded_length {
            return plain(body, sender, ctx, length, sha256).await;
        }
        return http_framed(body, sender, ctx, sha256, length_range).await;
    };
    let decoded = decoded_length.ok_or_else(malformed)?;
    let overhead = decoded
        .checked_div(8192)
        .and_then(|chunks| chunks.checked_add(2))
        .and_then(|chunks| chunks.checked_mul(102))
        .and_then(|overhead| decoded.checked_add(overhead))
        .ok_or_else(malformed)?;
    let reader = Reader {
        body,
        buffer: Bytes::new(),
        received: 0,
        limit: overhead,
        ctx,
    };
    signed(reader, sender, streaming, decoded).await
}

/// Streams a body delimited only by HTTP framing, Content-Length or chunked. A chunked body
/// declares no size up front, so its length range is checked here.
async fn http_framed(
    body: Incoming,
    sender: &UploadSender,
    ctx: ObjectCtx<'_>,
    sha256: Option<[u8; 32]>,
    length_range: Option<LengthRange>,
) -> Result<(), Error> {
    let limit = ctx.config.max_object_bytes;
    // Stop at the first byte past the range instead of staging the rest of the body.
    let cap = length_range.map_or(limit, |range| range.cap(limit));
    match stream_upload(body, sender, cap, sha256).await {
        Ok(Streamed::Complete(size)) => length_range.map_or(Ok(()), |range| range.check(size)),
        Ok(Streamed::Stopped) => Ok(()),
        Err(ObjectError::TooLarge) if cap < limit => Err(LengthRange::exceeded()),
        Err(error) => Err(storage_error(error, ctx.caller)),
    }
}

async fn plain(
    body: Incoming,
    sender: &UploadSender,
    ctx: ObjectCtx<'_>,
    length: u64,
    expected: Option<[u8; 32]>,
) -> Result<(), Error> {
    let mut reader = Reader {
        body,
        buffer: Bytes::new(),
        received: 0,
        limit: length,
        ctx,
    };
    let mut size = 0;
    let mut hash = digest::Context::new(&digest::SHA256);
    while reader.fill().await? {
        let bytes = reader.buffer.split_to(reader.buffer.len().min(64 * 1024));
        size += bytes.len() as u64;
        hash.update(&bytes);
        if sender.send(Ok(UploadChunk::Data(bytes))).await.is_err() {
            return Ok(());
        }
    }
    if size != length {
        return Err(malformed());
    }
    if expected.is_some_and(|expected| hash.finish().as_ref() != expected) {
        return Err(storage_error(
            ObjectError::ChecksumMismatch,
            reader.ctx.caller,
        ));
    }
    Ok(())
}

struct Reader<'a> {
    body: Incoming,
    buffer: Bytes,
    received: u64,
    limit: u64,
    ctx: ObjectCtx<'a>,
}

impl Reader<'_> {
    async fn fill(&mut self) -> Result<bool, Error> {
        while self.buffer.is_empty() {
            let Some(frame) = self.body.frame().await else {
                return Ok(false);
            };
            let bytes = frame
                .map_err(|error| storage_error(ObjectError::body_aborted(error), self.ctx.caller))?
                .into_data()
                .map_err(|_| malformed())?;
            self.received = self
                .received
                .checked_add(bytes.len() as u64)
                .filter(|size| *size <= self.limit)
                .ok_or_else(malformed)?;
            self.buffer = bytes;
        }
        Ok(true)
    }

    async fn byte(&mut self) -> Result<u8, Error> {
        if !self.fill().await? {
            return Err(malformed());
        }
        self.buffer
            .split_to(1)
            .first()
            .copied()
            .ok_or_else(malformed)
    }

    async fn crlf(&mut self) -> Result<(), Error> {
        if self.byte().await? != b'\r' || self.byte().await? != b'\n' {
            return Err(malformed());
        }
        Ok(())
    }

    async fn header(&mut self) -> Result<(u64, [u8; 32]), Error> {
        let mut line = Vec::new();
        loop {
            let byte = self.byte().await?;
            if byte == b'\r' {
                if self.byte().await? != b'\n' {
                    return Err(malformed());
                }
                break;
            }
            if line.len() >= 97 {
                return Err(malformed());
            }
            line.push(byte);
        }
        let line = std::str::from_utf8(&line).map_err(|_| malformed())?;
        let (length, signature) = line.split_once(";chunk-signature=").ok_or_else(malformed)?;
        if length.is_empty()
            || length.len() > 16
            || !length.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(malformed());
        }
        let signature = crate::hex::decode_lower::<32>(signature).ok_or_else(malformed)?;
        Ok((
            u64::from_str_radix(length, 16).map_err(|_| malformed())?,
            signature,
        ))
    }
}

async fn signed(
    mut reader: Reader<'_>,
    sender: &UploadSender,
    mut verifier: Streaming,
    decoded: u64,
) -> Result<(), Error> {
    let mut total = 0_u64;
    loop {
        let (length, signature) = reader.header().await?;
        total = total
            .checked_add(length)
            .filter(|size| *size <= decoded)
            .ok_or_else(malformed)?;
        // The linked format requires non-final chunks to be at least 8 KiB, bounding overhead.
        if length > 0 && length < 8192 && total != decoded {
            return Err(malformed());
        }
        let mut remaining = length;
        let mut hash = digest::Context::new(&digest::SHA256);
        while remaining > 0 {
            if !reader.fill().await? {
                return Err(malformed());
            }
            let size = reader
                .buffer
                .len()
                .min(64 * 1024)
                .min(usize::try_from(remaining).unwrap_or(usize::MAX));
            let bytes = reader.buffer.split_to(size);
            remaining -= size as u64;
            hash.update(&bytes);
            if sender.send(Ok(UploadChunk::Data(bytes))).await.is_err() {
                return Ok(());
            }
        }
        reader.crlf().await?;
        verifier
            .verify_chunk(hash.finish().as_ref(), &signature)
            .map_err(|error| super::super::signature_error(error.into()))?;
        if length == 0 {
            if total != decoded || reader.fill().await? {
                return Err(malformed());
            }
            return Ok(());
        }
    }
}
