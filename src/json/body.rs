use super::JsonError;
use http_body_util::BodyExt;
use hyper::{
    StatusCode,
    body::{Body, Buf},
};

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
            bytes.try_reserve_exact(length).map_err(|_| {
                JsonError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "backendError",
                    "Could not allocate request body buffer",
                )
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
