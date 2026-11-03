//! Hexadecimal codecs shared by protocol-specific validators.
#![deny(clippy::unwrap_used, clippy::expect_used)]

pub(crate) fn byte(pair: &[u8]) -> Option<u8> {
    let [high, low] = pair else { return None };
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

pub(crate) fn decode_hex<const N: usize>(raw: &str) -> Option<[u8; N]> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_length_ascii_hex() {
        assert_eq!(decode_hex("00aBfF"), Some([0, 171, 255]));
        assert_eq!(decode_hex::<0>(""), Some([]));
        for raw in ["0", "000", "0g", "é", "０"] {
            assert_eq!(decode_hex::<1>(raw), None);
        }
    }
}
