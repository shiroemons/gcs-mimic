//! The `Content-Transfer-Encoding` of a multipart/related media part.

use super::invalid;
use crate::base64::{Alphabet, Padding};
use crate::json::JsonError;
use bytes::Bytes;
use hyper::HeaderMap;

/// How the media part's bytes are encoded on the wire. Cloud Storage decodes a `base64` media
/// part and stores the decoded bytes (recorded 2026-11-13); `binary`, or no header, sends them
/// as they are.
#[derive(Debug, Default)]
pub(super) enum PartEncoding {
    #[default]
    Binary,
    Base64(Base64Part),
}

impl PartEncoding {
    /// The encoding a part's headers declare: `binary`, `base64` or none.
    pub(super) fn of(headers: &HeaderMap) -> Result<Self, JsonError> {
        let Some(value) = headers.get("content-transfer-encoding") else {
            return Ok(Self::Binary);
        };
        let value = value.to_str().map_err(|_| invalid())?.trim();
        if value.eq_ignore_ascii_case("binary") {
            Ok(Self::Binary)
        } else if value.eq_ignore_ascii_case("base64") {
            Ok(Self::Base64(Base64Part::default()))
        } else {
            Err(invalid())
        }
    }

    /// Requires a part other than the media part to be sent as it is.
    pub(super) fn require_binary(self) -> Result<(), JsonError> {
        match self {
            Self::Binary => Ok(()),
            Self::Base64(_) => Err(invalid()),
        }
    }

    /// Decodes one raw chunk of the part. A base64 chunk may yield nothing while it carries an
    /// incomplete quantum to the next chunk.
    pub(super) fn decode(&mut self, raw: Bytes) -> Result<Bytes, JsonError> {
        match self {
            Self::Binary => Ok(raw),
            Self::Base64(part) => part.decode(&raw),
        }
    }

    /// Ends the part: a base64 part must not stop inside a quantum.
    pub(super) fn finish(self) -> Result<(), JsonError> {
        match self {
            Self::Binary => Ok(()),
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
    fn decode(&mut self, raw: &[u8]) -> Result<Bytes, JsonError> {
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

    fn decode_split(encoded: &[u8], at: usize) -> Result<Vec<u8>, JsonError> {
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
    fn declared_encodings_are_binary_or_base64() {
        let mut headers = HeaderMap::new();
        assert!(matches!(
            PartEncoding::of(&headers),
            Ok(PartEncoding::Binary)
        ));
        headers.insert("content-transfer-encoding", "BASE64".parse().unwrap());
        let encoding = PartEncoding::of(&headers).unwrap();
        assert!(matches!(encoding, PartEncoding::Base64(_)));
        assert!(encoding.require_binary().is_err());
        headers.insert("content-transfer-encoding", " binary ".parse().unwrap());
        assert!(PartEncoding::of(&headers).unwrap().require_binary().is_ok());
        headers.insert(
            "content-transfer-encoding",
            "quoted-printable".parse().unwrap(),
        );
        assert!(PartEncoding::of(&headers).is_err());
    }
}
