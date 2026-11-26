//! The `Content-Transfer-Encoding` of a multipart/related media part.

use super::{MultipartError, invalid};
use crate::base64::{Alphabet, Padding};
use bytes::Bytes;
use hyper::HeaderMap;

/// How the media part's bytes are encoded on the wire. Cloud Storage decodes a `base64` media
/// part and stores the decoded bytes (recorded 2026-11-13).
#[derive(Debug, Default)]
pub(super) enum PartEncoding {
    /// The bytes are stored as sent.
    #[default]
    Identity,
    Base64(Base64Part),
}

/// The encodings stored as sent. Cloud Storage stored a media part declared `7bit`, `8bit` or
/// `quoted-printable` byte for byte, without decoding `=3D` (recorded 2026-11-17, case
/// obj-gaps-uploads).
const IDENTITY_ENCODINGS: [&str; 4] = ["binary", "7bit", "8bit", "quoted-printable"];

/// The encoding a part's headers declare: one of [`IDENTITY_ENCODINGS`], `base64` or none.
impl TryFrom<&HeaderMap> for PartEncoding {
    type Error = MultipartError;

    fn try_from(headers: &HeaderMap) -> Result<Self, MultipartError> {
        let Some(value) = headers.get("content-transfer-encoding") else {
            return Ok(Self::Identity);
        };
        let value = value.to_str().map_err(|_| invalid())?.trim();
        if IDENTITY_ENCODINGS
            .iter()
            .any(|identity| value.eq_ignore_ascii_case(identity))
        {
            Ok(Self::Identity)
        } else if value.eq_ignore_ascii_case("base64") {
            Ok(Self::Base64(Base64Part::default()))
        } else {
            Err(invalid())
        }
    }
}

impl PartEncoding {
    /// Requires a part other than the media part to be sent as it is.
    pub(super) fn require_identity(self) -> Result<(), MultipartError> {
        match self {
            Self::Identity => Ok(()),
            Self::Base64(_) => Err(invalid()),
        }
    }

    /// Decodes one raw chunk of the part. A base64 chunk may yield nothing while it carries an
    /// incomplete quantum to the next chunk.
    pub(super) fn decode(&mut self, raw: Bytes) -> Result<Bytes, MultipartError> {
        match self {
            Self::Identity => Ok(raw),
            Self::Base64(part) => part.decode(&raw),
        }
    }

    /// Ends the part: a base64 part must not stop inside a quantum.
    pub(super) fn finish(self) -> Result<(), MultipartError> {
        match self {
            Self::Identity => Ok(()),
            Self::Base64(part) if part.carry.is_empty() => Ok(()),
            Self::Base64(_) => Err(invalid()),
        }
    }
}

/// A streaming base64 decoder. It keeps fewer than four significant characters between
/// chunks, so its memory stays bounded by the multipart reader's chunk size.
#[derive(Debug, Default)]
pub(super) struct Base64Part {
    carry: Vec<u8>,
    /// A padded quantum ends the encoded data; anything after it is malformed.
    padded: bool,
}

impl Base64Part {
    fn decode(&mut self, raw: &[u8]) -> Result<Bytes, MultipartError> {
        // MIME base64 breaks its text into lines; line breaks and blanks carry no data.
        self.carry.extend(
            raw.iter()
                .copied()
                .filter(|byte| !matches!(byte, b'\r' | b'\n' | b' ' | b'\t')),
        );
        let whole = self.carry.len() / 4 * 4;
        if whole == 0 {
            return Ok(Bytes::new());
        }
        if self.padded {
            return Err(invalid());
        }
        let quanta: Vec<u8> = self.carry.drain(..whole).collect();
        self.padded = quanta.ends_with(b"=");
        let text = std::str::from_utf8(&quanta).map_err(|_| invalid())?;
        crate::base64::decode(text, Alphabet::Standard, Padding::Required)
            .map(Bytes::from)
            .ok_or_else(invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base64() -> PartEncoding {
        PartEncoding::Base64(Base64Part::default())
    }

    fn decode_split(encoded: &[u8], at: usize) -> Result<Vec<u8>, MultipartError> {
        let mut encoding = base64();
        let (head, tail) = encoded.split_at(at);
        let mut decoded = encoding.decode(Bytes::copy_from_slice(head))?.to_vec();
        decoded.extend_from_slice(&encoding.decode(Bytes::copy_from_slice(tail))?);
        encoding.finish()?;
        Ok(decoded)
    }

    #[test]
    fn base64_parts_decode_across_every_chunk_split_and_line_break() {
        for encoded in [&b"aGVsbG8="[..], b"aGVs\r\nbG8=", b"aGVsbG8=\r\n", b""] {
            for at in 0..=encoded.len() {
                let expected: &[u8] = if encoded.is_empty() { b"" } else { b"hello" };
                assert_eq!(decode_split(encoded, at).unwrap(), expected, "{at}");
            }
        }
    }

    #[test]
    fn malformed_base64_parts_fail() {
        for encoded in [&b"aGVsbG8"[..], b"aGU=aGVs", b"aGV*bG8=", b"aGVsbG8==="] {
            for at in 0..=encoded.len() {
                assert!(decode_split(encoded, at).is_err(), "{encoded:?} {at}");
            }
        }
    }

    #[test]
    fn declared_encodings_are_identities_or_base64() {
        let declared = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("content-transfer-encoding", value.parse().unwrap());
            PartEncoding::try_from(&headers)
        };
        assert!(matches!(
            PartEncoding::try_from(&HeaderMap::new()),
            Ok(PartEncoding::Identity)
        ));
        let encoding = declared("BASE64").unwrap();
        assert!(matches!(encoding, PartEncoding::Base64(_)));
        assert!(encoding.require_identity().is_err());
        for identity in [" binary ", "7BIT", "8bit", "Quoted-Printable"] {
            assert!(
                declared(identity).unwrap().require_identity().is_ok(),
                "{identity}"
            );
        }
        for unknown in ["x-uuencode", "binary, base64", ""] {
            assert!(declared(unknown).is_err(), "{unknown:?}");
        }
    }
}
