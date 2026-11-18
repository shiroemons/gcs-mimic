#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Streaming object digests and a versioned, validated checkpoint codec.

use md5::{
    Digest, Md5,
    digest::common::hazmat::{SerializableState, SerializedState},
    digest::common::typenum::Unsigned,
};
use std::io;

const STATE_FORMAT: u8 = 1;
const MD5_STATE_BYTES: usize = <Md5 as SerializableState>::SerializedStateSize::USIZE;
const _: () = assert!(MD5_STATE_BYTES == 88);
/// The envelope: the format, the byte count, md-5's state and the CRC32C.
pub(crate) const HASHER_STATE_BYTES: usize =
    1 + size_of::<u64>() + MD5_STATE_BYTES + size_of::<u32>();

#[derive(Clone, Default)]
pub(crate) struct Hasher {
    md5: Md5,
    crc32c: u32,
    bytes: u64,
}

/// A [`Hasher`] frozen at a byte count, in the versioned envelope a checkpoint persists: the
/// format byte, the big-endian byte count, md-5's serialized state and the big-endian CRC32C.
/// Format 1 pins md-5 0.11's ABI; incompatible upgrades require rehashing bodies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HasherState {
    bytes: u64,
    md5: [u8; MD5_STATE_BYTES],
    crc32c: u32,
}

fn invalid_state() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid serialized hash state")
}

/// The byte count md-5's format-1 state records: 16 chaining bytes, a little-endian block
/// count, then the length of the buffered partial block.
fn md5_byte_count(state: &[u8; MD5_STATE_BYTES]) -> Option<u64> {
    let (_chaining, rest) = state.split_first_chunk::<16>()?;
    let (blocks, rest) = rest.split_first_chunk::<8>()?;
    let buffered = rest.first()?;
    u64::from_le_bytes(*blocks)
        .checked_mul(64)?
        .checked_add(u64::from(*buffered))
}

impl HasherState {
    pub(crate) const fn bytes(&self) -> u64 {
        self.bytes
    }

    pub(crate) fn to_bytes(&self) -> [u8; HASHER_STATE_BYTES] {
        let fields = [STATE_FORMAT]
            .into_iter()
            .chain(self.bytes.to_be_bytes())
            .chain(self.md5)
            .chain(self.crc32c.to_be_bytes());
        let mut envelope = [0; HASHER_STATE_BYTES];
        for (slot, byte) in envelope.iter_mut().zip(fields) {
            *slot = byte;
        }
        envelope
    }

    /// Parses an envelope, refusing one whose MD5 state disagrees with its byte count or does
    /// not decode, so a resumed digest never continues from a corrupt prefix.
    pub(crate) fn from_bytes(input: &[u8]) -> io::Result<Self> {
        let (format, rest) = input.split_first().ok_or_else(invalid_state)?;
        let (bytes, rest) = rest.split_first_chunk().ok_or_else(invalid_state)?;
        let (md5, crc32c) = rest.split_first_chunk().ok_or_else(invalid_state)?;
        let crc32c = <[u8; 4]>::try_from(crc32c).map_err(|_| invalid_state())?;
        let state = Self {
            bytes: u64::from_be_bytes(*bytes),
            md5: *md5,
            crc32c: u32::from_be_bytes(crc32c),
        };
        if *format != STATE_FORMAT || md5_byte_count(&state.md5) != Some(state.bytes) {
            return Err(invalid_state());
        }
        state.md5()?;
        Ok(state)
    }

    fn md5(&self) -> io::Result<Md5> {
        let state =
            SerializedState::<Md5>::try_from(self.md5.as_slice()).map_err(|_| invalid_state())?;
        Md5::deserialize(&state).map_err(|_| invalid_state())
    }

    /// The digests of every byte the state has consumed.
    pub(crate) fn digest(&self) -> io::Result<Hashes> {
        Hasher::resume(self, self.bytes).map(Hasher::finish)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Hashes {
    pub md5: [u8; 16],
    pub crc32c: u32,
}

/// A declared digest that is not the padded standard base64 of exactly the algorithm's width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InvalidDigest;

impl std::fmt::Display for InvalidDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Invalid base64 checksum")
    }
}

impl std::error::Error for InvalidDigest {}

/// The `N` bytes of a digest declared in padded standard base64, as headers and resource
/// fields carry MD5 and CRC32C values.
pub(crate) fn decode_hash<const N: usize>(raw: &str) -> Result<[u8; N], InvalidDigest> {
    if raw.len() != N.div_ceil(3) * 4 {
        return Err(InvalidDigest);
    }
    crate::base64::decode(
        raw,
        crate::base64::Alphabet::Standard,
        crate::base64::Padding::Required,
    )
    .and_then(|bytes| bytes.try_into().ok())
    .ok_or(InvalidDigest)
}

#[expect(
    clippy::indexing_slicing,
    reason = "each hex alphabet index is masked or shifted to four bits"
)]
pub(crate) fn encode_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(HEX[(byte >> 4) as usize]));
        text.push(char::from(HEX[(byte & 15) as usize]));
    }
    text
}

impl Hasher {
    // Derived transfers predate the versioned envelope and persist these fields separately.
    pub(crate) fn from_md5_state(state: &[u8], crc32c: u32, bytes: u64) -> io::Result<Self> {
        let state = SerializedState::<Md5>::try_from(state).map_err(|_| invalid_state())?;
        Ok(Self {
            md5: Md5::deserialize(&state).map_err(|_| invalid_state())?,
            crc32c,
            bytes,
        })
    }

    pub(crate) fn md5_state(&self) -> ([u8; MD5_STATE_BYTES], u32) {
        (self.md5.serialize().into(), self.crc32c)
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        // Object-size admission bounds callers far below the u64 byte-count limit.
        debug_assert!(self.bytes.checked_add(bytes.len() as u64).is_some());
        self.bytes = self.bytes.wrapping_add(bytes.len() as u64);
        self.md5.update(bytes);
        self.crc32c = crc32c::crc32c_append(self.crc32c, bytes);
    }
    pub(crate) fn finish(self) -> Hashes {
        Hashes {
            md5: self.md5.finalize().into(),
            crc32c: self.crc32c,
        }
    }
    pub(crate) fn save(&self) -> HasherState {
        HasherState {
            bytes: self.bytes,
            md5: self.md5.serialize().into(),
            crc32c: self.crc32c,
        }
    }

    /// Continues the digests of a checkpoint whose bytes end at `offset`.
    pub(crate) fn resume(state: &HasherState, offset: u64) -> io::Result<Self> {
        if state.bytes != offset {
            return Err(invalid_state());
        }
        Ok(Self {
            md5: state.md5()?,
            crc32c: state.crc32c,
            bytes: state.bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checkpoint_resumes_both_digests() {
        let bytes = [17; 131_073];
        let mut hash = Hasher::default();
        hash.update(&bytes[..65_537]);
        let state = hash.save().to_bytes();
        let mut expected = [0u8; HASHER_STATE_BYTES];
        expected[..35].copy_from_slice(&[
            1, 0, 0, 0, 0, 0, 1, 0, 1, 228, 41, 161, 232, 38, 22, 221, 141, 86, 54, 56, 29, 64,
            100, 99, 185, 0, 4, 0, 0, 0, 0, 0, 0, 1, 17,
        ]);
        expected[97..].copy_from_slice(&[198, 142, 15, 147]);
        assert_eq!(state, expected);
        let parsed = HasherState::from_bytes(&state).unwrap();
        assert_eq!(parsed.bytes(), 65_537);
        assert_eq!(
            parsed.digest().unwrap(),
            Hashes {
                md5: Md5::digest(&bytes[..65_537]).into(),
                crc32c: crc32c::crc32c(&bytes[..65_537])
            }
        );
        assert!(Hasher::resume(&parsed, 65_536).is_err());
        let mut hash = Hasher::resume(&parsed, 65_537).unwrap();
        hash.update(&bytes[65_537..]);
        assert_eq!(
            hash.finish(),
            Hashes {
                md5: Md5::digest(bytes).into(),
                crc32c: crc32c::crc32c(&bytes)
            }
        );
        assert_eq!(
            HasherState::from_bytes(&[]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let mut bad = state;
        bad[0] = 2;
        assert!(HasherState::from_bytes(&bad).is_err());
        let mut bad = state;
        bad[8] ^= 1;
        assert!(HasherState::from_bytes(&bad).is_err());
        assert!(HasherState::from_bytes(&state[..100]).is_err());
        assert!(HasherState::from_bytes(&[state.as_slice(), &[0]].concat()).is_err());
    }

    #[test]
    fn encode_lower_preserves_leading_zeroes() {
        assert_eq!(encode_lower(&[0, 15, 16, 255]), "000f10ff");
    }

    #[test]
    fn derived_checkpoint_preserves_raw_md5_bytes_and_resumes() {
        for offset in [0, 1, 63, 64, 65, 65_537] {
            let bytes = vec![17; offset];
            let mut previous = Md5::new();
            previous.update(&bytes);
            let state = previous.serialize();
            let crc = crc32c::crc32c(&bytes);
            let mut hash = Hasher::from_md5_state(state.as_slice(), crc, offset as u64).unwrap();
            assert_eq!(hash.md5_state(), (state.into(), crc));
            hash.update(b"tail");
            previous.update(b"tail");
            assert_eq!(
                hash.finish(),
                Hashes {
                    md5: previous.finalize().into(),
                    crc32c: crc32c::crc32c_append(crc, b"tail"),
                }
            );
        }
        assert!(Hasher::from_md5_state(&[0; 87], 0, 0).is_err());
        assert!(Hasher::from_md5_state(&[0; 89], 0, 0).is_err());
    }
}
