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
const MD5_STATE_START: usize = 1 + size_of::<u64>();
const CRC32C_START: usize = MD5_STATE_START + MD5_STATE_BYTES;
pub const HASHER_STATE_BYTES: usize = CRC32C_START + size_of::<u32>();

#[derive(Clone, Default)]
pub struct Hasher {
    md5: Md5,
    crc32c: u32,
    bytes: u64,
}

/// Format 1 pins md-5 0.11's ABI; incompatible upgrades require rehashing bodies.
#[derive(Clone)]
pub struct HasherState(Hasher);

fn invalid_state() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid serialized hash state")
}

impl HasherState {
    pub fn bytes(&self) -> u64 {
        self.0.bytes
    }

    pub fn to_bytes(&self) -> [u8; HASHER_STATE_BYTES] {
        let mut bytes = [0; HASHER_STATE_BYTES];
        bytes[0] = STATE_FORMAT;
        bytes[1..MD5_STATE_START].copy_from_slice(&self.0.bytes.to_be_bytes());
        bytes[MD5_STATE_START..CRC32C_START].copy_from_slice(self.0.md5.serialize().as_slice());
        bytes[CRC32C_START..].copy_from_slice(&self.0.crc32c.to_be_bytes());
        bytes
    }

    pub fn from_bytes(input: &[u8]) -> io::Result<Self> {
        let input: &[u8; HASHER_STATE_BYTES] = input.try_into().map_err(|_| invalid_state())?;
        if input[0] != STATE_FORMAT {
            return Err(invalid_state());
        }
        let bytes = u64::from_be_bytes(
            input[1..MD5_STATE_START]
                .try_into()
                .map_err(|_| invalid_state())?,
        );
        let state = SerializedState::<Md5>::try_from(&input[MD5_STATE_START..CRC32C_START])
            .map_err(|_| invalid_state())?;
        // Format 1 has 16 chaining bytes, a little-endian block count, then buffer length.
        let blocks = u64::from_le_bytes(state[16..24].try_into().map_err(|_| invalid_state())?);
        let observed = blocks
            .checked_mul(64)
            .and_then(|n| n.checked_add(u64::from(state[24])))
            .ok_or_else(invalid_state)?;
        if observed != bytes {
            return Err(invalid_state());
        }
        Ok(Self(Hasher {
            md5: Md5::deserialize(&state).map_err(|_| invalid_state())?,
            crc32c: u32::from_be_bytes(
                input[CRC32C_START..]
                    .try_into()
                    .map_err(|_| invalid_state())?,
            ),
            bytes,
        }))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Hashes {
    pub md5: [u8; 16],
    pub crc32c: u32,
}

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
    pub fn update(&mut self, bytes: &[u8]) {
        // Object-size admission bounds callers far below the u64 byte-count limit.
        debug_assert!(self.bytes.checked_add(bytes.len() as u64).is_some());
        self.bytes = self.bytes.wrapping_add(bytes.len() as u64);
        self.md5.update(bytes);
        self.crc32c = crc32c::crc32c_append(self.crc32c, bytes);
    }
    pub fn finish(self) -> Hashes {
        Hashes {
            md5: self.md5.finalize().into(),
            crc32c: self.crc32c,
        }
    }
    pub fn save(&self) -> HasherState {
        HasherState(self.clone())
    }
    pub fn resume(state: HasherState, offset: u64) -> io::Result<Self> {
        if state.bytes() != offset {
            return Err(invalid_state());
        }
        Ok(state.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
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
        assert!(Hasher::resume(parsed.clone(), 65_536).is_err());
        let mut hash = Hasher::resume(parsed, 65_537).unwrap();
        hash.update(&bytes[65_537..]);
        assert_eq!(
            hash.finish(),
            Hashes {
                md5: Md5::digest(bytes).into(),
                crc32c: crc32c::crc32c(&bytes)
            }
        );
        let mut bad = state;
        bad[0] = 2;
        assert!(HasherState::from_bytes(&bad).is_err());
        let mut bad = state;
        bad[8] ^= 1;
        assert!(HasherState::from_bytes(&bad).is_err());
        assert!(HasherState::from_bytes(&state[..100]).is_err());
    }

    #[test]
    fn encode_lower_preserves_leading_zeroes() {
        assert_eq!(encode_lower(&[0, 15, 16, 255]), "000f10ff");
    }
}
