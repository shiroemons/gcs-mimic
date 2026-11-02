//! Base64 codecs with the acceptance rules of the individual wire formats.

#[derive(Clone, Copy)]
pub(crate) enum Alphabet {
    Standard,
    UrlSafe,
    Either,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Padding {
    Required,
    Optional,
    Forbidden,
    /// XML POST policies historically accept nonzero unused bits, but require padding.
    RequiredUncheckedBits,
}

pub(crate) fn decode(raw: &str, alphabet: Alphabet, padding: Padding) -> Option<Vec<u8>> {
    let data = raw.trim_end_matches('=');
    let pads = raw.len() - data.len();
    if data.len() % 4 == 1
        || pads > 2
        || (pads > 0 && (!raw.len().is_multiple_of(4) || padding == Padding::Forbidden))
        || (matches!(padding, Padding::Required | Padding::RequiredUncheckedBits)
            && !raw.len().is_multiple_of(4))
    {
        return None;
    }
    let mut output = Vec::with_capacity(data.len() * 3 / 4);
    let mut bits = 0u32;
    let mut count = 0;
    for byte in data.bytes() {
        let digit = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' if matches!(alphabet, Alphabet::Standard | Alphabet::Either) => 62,
            b'/' if matches!(alphabet, Alphabet::Standard | Alphabet::Either) => 63,
            b'-' if matches!(alphabet, Alphabet::UrlSafe | Alphabet::Either) => 62,
            b'_' if matches!(alphabet, Alphabet::UrlSafe | Alphabet::Either) => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(digit);
        count += 6;
        if count >= 8 {
            count -= 8;
            output.push((bits >> count) as u8);
        }
    }
    (padding == Padding::RequiredUncheckedBits || bits & ((1 << count) - 1) == 0).then_some(output)
}

pub(crate) fn encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        result.push(ALPHABET[(a >> 2) as usize] as char);
        result.push(ALPHABET[((a & 3) << 4 | b >> 4) as usize] as char);
        result.push(if chunk.len() > 1 {
            ALPHABET[((b & 15) << 2 | c >> 6) as usize] as char
        } else {
            '='
        });
        result.push(if chunk.len() > 2 {
            ALPHABET[(c & 63) as usize] as char
        } else {
            '='
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_vectors_and_boundary_lengths() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(plain.as_bytes()), encoded);
            assert_eq!(
                decode(encoded, Alphabet::Standard, Padding::Required).unwrap(),
                plain.as_bytes()
            );
            let unpadded = encoded.trim_end_matches('=');
            for padding in [Padding::Optional, Padding::Forbidden] {
                assert_eq!(
                    decode(unpadded, Alphabet::Standard, padding).unwrap(),
                    plain.as_bytes()
                );
            }
        }
        for length in 0..=257 {
            let plain: Vec<_> = (0..length).map(|index| index as u8).collect();
            assert_eq!(
                decode(&encode(&plain), Alphabet::Standard, Padding::Required).unwrap(),
                plain
            );
        }
    }

    #[test]
    fn alphabets_are_explicit_and_mixed_only_when_requested() {
        for (raw, alphabet) in [
            ("+/8=", Alphabet::Standard),
            ("-_8=", Alphabet::UrlSafe),
            ("+_8=", Alphabet::Either),
            ("-/8=", Alphabet::Either),
        ] {
            assert_eq!(
                decode(raw, alphabet, Padding::Required).unwrap(),
                [251, 255]
            );
        }
        assert!(decode("+/8=", Alphabet::UrlSafe, Padding::Required).is_none());
        assert!(decode("-_8=", Alphabet::Standard, Padding::Required).is_none());
    }

    #[test]
    fn padding_and_unused_bits_follow_the_selected_contract() {
        assert!(decode("Zg", Alphabet::Standard, Padding::Required).is_none());
        assert!(decode("Zg==", Alphabet::Standard, Padding::Forbidden).is_none());
        assert!(decode("Zh==", Alphabet::Standard, Padding::Required).is_none());
        assert!(decode("Zm9=", Alphabet::Standard, Padding::Optional).is_none());
        assert_eq!(
            decode("Zh==", Alphabet::Standard, Padding::RequiredUncheckedBits).unwrap(),
            b"f"
        );
        assert_eq!(
            decode("Zm9=", Alphabet::Standard, Padding::RequiredUncheckedBits).unwrap(),
            b"fo"
        );
        for raw in [
            "A", "=", "====", "Zg=", "Zg===", "Z=g=", "Zg==AAAA", "Z g==", "Zg==\n", "é",
        ] {
            for padding in [
                Padding::Required,
                Padding::Optional,
                Padding::Forbidden,
                Padding::RequiredUncheckedBits,
            ] {
                assert!(decode(raw, Alphabet::Either, padding).is_none(), "{raw}");
            }
        }
    }
}
