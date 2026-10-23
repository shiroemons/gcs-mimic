// Reuse the installed implementation instead of introducing a second polynomial implementation.
pub(crate) use crc32c::crc32c_combine as combine;

#[cfg(test)]
mod tests {
    use super::combine;
    use crc32c::crc32c;

    #[test]
    fn every_split_matches_direct_crc32c() {
        let bytes: Vec<_> = (0..1024).map(|i| (i * 73 + i / 7) as u8).collect();
        for split in 0..=bytes.len() {
            assert_eq!(
                combine(
                    crc32c(&bytes[..split]),
                    crc32c(&bytes[split..]),
                    bytes.len() - split
                ),
                crc32c(&bytes),
                "split {split}"
            );
        }
    }

    #[test]
    fn arbitrary_ordered_components_match_direct_crc32c() {
        let mut seed = 0x87a2_6185_u64;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 32) as u32
        };
        for count in 1..=32 {
            let mut bytes = Vec::new();
            let mut combined = 0;
            for _ in 0..count {
                let length = next() as usize % 4097;
                let component: Vec<_> = (0..length).map(|_| next() as u8).collect();
                combined = combine(combined, crc32c(&component), length);
                bytes.extend_from_slice(&component);
            }
            assert_eq!(combined, crc32c(&bytes), "{count} components");
        }
    }

    #[test]
    fn nested_combination_is_associative_and_preserves_empty_components() {
        let a = b"first";
        let b = b"\0binary\xff";
        let c = b"last";
        let ab = combine(crc32c(a), crc32c(b), b.len());
        let bc = combine(crc32c(b), crc32c(c), c.len());
        assert_eq!(
            combine(ab, crc32c(c), c.len()),
            combine(crc32c(a), bc, b.len() + c.len())
        );
        assert_eq!(combine(ab, 0, 0), ab);
        assert_eq!(combine(0, ab, a.len() + b.len()), ab);
    }
}
