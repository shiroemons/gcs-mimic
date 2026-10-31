//! Incremental signed-chunk decoding keeps unverified bytes in the existing staging upload.

use super::*;
use crate::{auth::signature::Streaming, server::body_progress};
use http_body_util::BodyExt;
use ring::digest;

#[cfg(test)]
mod tests;

fn malformed() -> Error {
    Error::new(
        StatusCode::BAD_REQUEST,
        "InvalidArgument",
        "Invalid signed chunk framing or decoded length.",
    )
}

pub(super) fn stored_encoding(headers: &HeaderMap) -> Result<Option<Option<String>>, Error> {
    let Some(value) = single(headers, "content-encoding")? else {
        return Ok(None);
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
    Ok(Some((!remaining.is_empty()).then(|| remaining.join(","))))
}

pub(super) async fn upload(
    body: Incoming,
    sender: &mpsc::Sender<Result<UploadChunk, io::Error>>,
    ctx: ObjectCtx<'_>,
    streaming: Option<Streaming>,
    decoded: Option<u64>,
    sha256: Option<[u8; 32]>,
) -> Result<(), Error> {
    let Some(streaming) = streaming else {
        if let Some(length) = decoded {
            return plain(body, sender, ctx, length, sha256).await;
        }
        return stream_upload(
            body,
            sender,
            ctx.config.max_object_bytes,
            ctx.activity,
            sha256,
        )
        .await
        .map_err(|error| storage_error(error, ctx.caller));
    };
    let decoded = decoded.ok_or_else(malformed)?;
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

async fn plain(
    body: Incoming,
    sender: &mpsc::Sender<Result<UploadChunk, io::Error>>,
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
            body_progress(self.ctx.activity);
            self.buffer = bytes;
        }
        Ok(true)
    }

    async fn byte(&mut self) -> Result<u8, Error> {
        if !self.fill().await? {
            return Err(malformed());
        }
        Ok(self.buffer.split_to(1)[0])
    }

    async fn crlf(&mut self) -> Result<(), Error> {
        if self.byte().await? != b'\r' || self.byte().await? != b'\n' {
            return Err(malformed());
        }
        Ok(())
    }

    async fn header(&mut self) -> Result<(u64, Vec<u8>), Error> {
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
            || signature.len() != 64
            || !signature
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(malformed());
        }
        let signature = (0..64)
            .step_by(2)
            .map(|index| {
                u8::from_str_radix(&signature[index..index + 2], 16).map_err(|_| malformed())
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((
            u64::from_str_radix(length, 16).map_err(|_| malformed())?,
            signature,
        ))
    }
}

async fn signed(
    mut reader: Reader<'_>,
    sender: &mpsc::Sender<Result<UploadChunk, io::Error>>,
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
            .await
            .map_err(super::super::signature_error)?;
        if length == 0 {
            if total != decoded || reader.fill().await? {
                return Err(malformed());
            }
            return Ok(());
        }
    }
}
