use super::JsonError;
use http_body_util::BodyExt;
use hyper::{
    StatusCode,
    body::{Body, Buf},
};

/// The most one JSON document may occupy in memory: a request body, an expanded gzip body, a
/// multipart metadata part, a form, an initialization entry or a source metadata record.
///
/// Cloud Storage documents no size for these documents, so this is a memory guard that real data
/// does not reach, not a contract. A bucket can legally hold 1,000 lifecycle prefixes and suffixes
/// of up to 1,024 bytes, which is 1 MiB before the rules around them and before JSON escaping
/// multiplies a byte by up to six, and CORS has no documented count at all.
pub(crate) const RESOURCE_BODY_LIMIT: usize = 16 * 1024 * 1024;
pub(crate) const RESOURCE_BODY_LIMIT_TEXT: &str = "16 MiB";

/// Collect at most `cap` bytes; the caller must also bound transport frames and read time.
pub(crate) async fn read_body<B: Body + Unpin>(
    mut body: B,
    cap: usize,
) -> Result<Vec<u8>, JsonError> {
    let too_large = || {
        JsonError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "uploadTooLarge",
            "JSON request body exceeds the configured limit",
        )
    };
    if body.size_hint().lower() > cap as u64 {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    if let Some(length) = body.size_hint().exact() {
        let length = usize::try_from(length).map_err(|_| too_large())?;
        bytes.try_reserve(length).map_err(|_| {
            JsonError::service_unavailable("Could not allocate request body buffer")
        })?;
    }
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| {
            JsonError::new(
                StatusCode::BAD_REQUEST,
                "badRequest",
                "Could not read request body",
            )
        })?;
        if let Ok(mut data) = frame.into_data() {
            let length = data.remaining();
            if length > cap - bytes.len() {
                return Err(too_large());
            }
            bytes.try_reserve(length).map_err(|_| {
                JsonError::service_unavailable("Could not allocate request body buffer")
            })?;
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
