//! Collecting a small request document into memory under a bound: a JSON resource, an XML
//! manifest, a form or a body that must be empty. Object bodies stream instead.

use http_body_util::BodyExt;
use hyper::body::{Body, Buf};

/// The most one JSON document may occupy in memory: a request body, an expanded gzip body, a
/// multipart metadata part, a form, an initialization entry or a source metadata record.
///
/// Cloud Storage documents no size for these documents, so this is a memory guard that real data
/// does not reach, not a contract. A bucket can legally hold 1,000 lifecycle prefixes and suffixes
/// of up to 1,024 bytes, which is 1 MiB before the rules around them and before JSON escaping
/// multiplies a byte by up to six, and CORS has no documented count at all.
pub(crate) const RESOURCE_BODY_LIMIT: usize = 16 * 1024 * 1024;
pub(crate) const RESOURCE_BODY_LIMIT_TEXT: &str = "16 MiB";

/// Why a body could not be collected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum BodyError {
    /// The body is longer than the caller's bound.
    #[error("JSON request body exceeds the configured limit")]
    TooLarge,
    /// The buffer for the body could not be allocated.
    #[error("Could not allocate request body buffer")]
    OutOfMemory,
    /// The transport failed while the body was read.
    #[error("Could not read request body")]
    Unreadable,
}

/// Collect at most `cap` bytes; the caller must also bound transport frames and read time.
pub(crate) async fn read_body<B: Body + Unpin>(
    mut body: B,
    cap: usize,
) -> Result<Vec<u8>, BodyError> {
    if body.size_hint().lower() > cap as u64 {
        return Err(BodyError::TooLarge);
    }
    let mut bytes = Vec::new();
    if let Some(length) = body.size_hint().exact() {
        let length = usize::try_from(length).map_err(|_| BodyError::TooLarge)?;
        bytes
            .try_reserve(length)
            .map_err(|_| BodyError::OutOfMemory)?;
    }
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| BodyError::Unreadable)?;
        if let Ok(mut data) = frame.into_data() {
            let length = data.remaining();
            if length > cap - bytes.len() {
                return Err(BodyError::TooLarge);
            }
            bytes
                .try_reserve(length)
                .map_err(|_| BodyError::OutOfMemory)?;
            while data.has_remaining() {
                let chunk = data.chunk();
                bytes.extend_from_slice(chunk);
                let length = chunk.len();
                data.advance(length);
            }
        }
    }
    Ok(bytes)
}
