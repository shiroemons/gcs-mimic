//! Hexadecimal codecs shared by protocol-specific validators.
#![deny(clippy::unwrap_used, clippy::expect_used)]

pub(crate) fn byte([high, low]: &[u8; 2]) -> Option<u8> {
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }
    Some(digit(*high)? << 4 | digit(*low)?)
}

pub(crate) fn decode<const N: usize>(raw: &str) -> Option<[u8; N]> {
    let (pairs, remainder) = raw.as_bytes().as_chunks::<2>();
    if pairs.len() != N || !remainder.is_empty() {
        return None;
    }
    let mut output = [0; N];
    for (output, pair) in output.iter_mut().zip(pairs) {
        *output = byte(pair)?;
    }
    Some(output)
}

/// The canonical lowercase form of exactly `N` bytes. Opaque identifiers and signatures that a
/// client echoes back must match their stored text byte for byte, so `A`–`F` are rejected here
/// while `decode` keeps accepting them for protocol fields that allow either case.
pub(crate) fn decode_lower<const N: usize>(raw: &str) -> Option<[u8; N]> {
    if raw.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    decode(raw)
}

/// Whether `raw` is `len` lowercase hexadecimal digits, for revisions the catalog keeps as text.
pub(crate) fn is_lower(raw: &str, len: usize) -> bool {
    raw.len() == len
        && raw
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_length_ascii_hex() {
        assert_eq!(decode("00aBfF"), Some([0, 171, 255]));
        assert_eq!(decode::<0>(""), Some([]));
        for raw in ["0", "000", "0g", "é", "０"] {
            assert_eq!(decode::<1>(raw), None);
        }
    }

    #[test]
    fn lowercase_hex_rejects_uppercase_lengths_and_non_ascii() {
        assert_eq!(decode_lower("00abff"), Some([0, 171, 255]));
        for raw in ["00aBfF", "00ab", "00abff00", "é", "０"] {
            assert_eq!(decode_lower::<3>(raw), None, "{raw}");
        }
        assert!(is_lower("00abff", 6));
        for (raw, len) in [
            ("00abff", 4),
            ("00aBff", 6),
            ("00abfg", 6),
            ("é", 2),
            ("０", 3),
        ] {
            assert!(!is_lower(raw, len), "{raw}");
        }
    }
}
