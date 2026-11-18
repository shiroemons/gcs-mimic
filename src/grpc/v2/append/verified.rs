//! Appendable content that passed its message's CRC32C check. The field stays private to this
//! module, so the rest of the adapter can neither build a chunk around unchecked bytes nor
//! reach into one.

use crate::grpc::v2::{object_io::verify_crc32c, pb};
use tonic::Status;

/// Content whose CRC32C matched the one its message declared, if any. Only
/// [`VerifiedChunk::verify`] makes one, so no unchecked byte reaches the writer.
pub(super) struct VerifiedChunk(bytes::Bytes);

impl VerifiedChunk {
    pub(super) fn verify(data: pb::ChecksummedData) -> Result<Self, Status> {
        verify_crc32c(&data)?;
        Ok(Self(data.content))
    }

    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

    pub(super) fn into_bytes(self) -> bytes::Bytes {
        self.0
    }
}
