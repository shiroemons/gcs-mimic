//! Integer representations retained by the protobuf adapters.

/// Preserve the former wire representation even for a stored value above `i64::MAX`.
pub(super) fn signed(value: u64) -> i64 {
    value.cast_signed()
}

/// Keep the low 32 bits, including the legacy representation of oversized component counts.
pub(super) fn component_count(value: Option<i64>) -> i32 {
    let [a, b, c, d, ..] = value.unwrap_or_default().to_le_bytes();
    i32::from_le_bytes([a, b, c, d])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_wire_integers_preserve_boundary_bits() {
        assert_eq!(signed(i64::MAX.cast_unsigned()), i64::MAX);
        assert_eq!(signed(i64::MAX.cast_unsigned() + 1), i64::MIN);
        assert_eq!(signed(u64::MAX), -1);
        assert_eq!(component_count(None), 0);
        assert_eq!(component_count(Some(i64::from(i32::MAX))), i32::MAX);
        assert_eq!(component_count(Some(i64::from(i32::MAX) + 1)), i32::MIN);
        assert_eq!(component_count(Some(i64::MAX)), -1);
    }
}
