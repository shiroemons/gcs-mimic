//! Minimal standard-alphabet base64, enough for binary request bodies in case
//! files and for comparing binary response bodies without a dependency.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub(crate) fn decode(text: &str) -> Result<Vec<u8>, String> {
    let trimmed = text.trim_end_matches('=');
    if trimmed.len() % 4 == 1 || text.len() - trimmed.len() > 2 {
        return Err("invalid base64 length".into());
    }
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in trimmed.bytes() {
        let value = ALPHABET
            .iter()
            .position(|a| *a == c)
            .ok_or_else(|| format!("invalid base64 character {:?}", char::from(c)))?;
        acc = (acc << 6) | u32::try_from(value).unwrap_or(0);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).unwrap_or(0));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_padding_length() {
        for sample in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"\x1f\x8b\x08\x00\xff",
        ] {
            assert_eq!(decode(&encode(sample)).unwrap(), sample);
        }
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert!(decode("a").is_err());
        assert!(decode("ab!d").is_err());
    }
}
