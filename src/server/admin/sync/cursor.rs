use super::{JsonError, ObservationCursor, OpaqueId, Ordinal, stale};
use crate::storage::sync_state::observation::FindingCategory;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SyncCursor {
    Findings(ObservationCursor),
    Deletions(Ordinal),
    Conflicts {
        revision: OpaqueId,
        ordinal: Ordinal,
    },
}

impl SyncCursor {
    pub(super) fn encode(&self, prefix: &str) -> String {
        match self {
            Self::Findings(ObservationCursor { category, ordinal }) => {
                format!("{prefix}{:02x}{:016x}", category.rank(), ordinal.get())
            }
            Self::Deletions(ordinal) => format!("{prefix}{:016x}", ordinal.get()),
            Self::Conflicts { revision, ordinal } => {
                format!("{prefix}{revision}{:016x}", ordinal.get())
            }
        }
    }

    pub(super) fn decode(raw: &str, prefix: &str) -> Result<Self, JsonError> {
        let suffix = raw.strip_prefix(prefix).ok_or_else(stale)?;
        if !crate::hex::is_lower(suffix, suffix.len()) {
            return Err(stale());
        }
        let number = |raw| i64::from_str_radix(raw, 16).map_err(|_| stale());
        // Lowercase hexadecimal has no sign, so an ordinal that fits `i64` is never negative.
        let ordinal = |raw| Ordinal::try_from(number(raw)?).map_err(|_| stale());
        match suffix.len() {
            16 => Ok(Self::Deletions(ordinal(suffix)?)),
            18 => {
                let (kind, ordinal_digits) = suffix.split_at_checked(2).ok_or_else(stale)?;
                let category = FindingCategory::from_rank(number(kind)?).ok_or_else(stale)?;
                Ok(Self::Findings(ObservationCursor {
                    category,
                    ordinal: ordinal(ordinal_digits)?,
                }))
            }
            48 => {
                let (revision, ordinal_digits) = suffix.split_at_checked(32).ok_or_else(stale)?;
                Ok(Self::Conflicts {
                    revision: revision.parse().map_err(|_| stale())?,
                    ordinal: ordinal(ordinal_digits)?,
                })
            }
            _ => Err(stale()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordinal(value: i64) -> Ordinal {
        Ordinal::try_from(value).unwrap()
    }

    #[test]
    fn cursors_preserve_fixed_lowercase_wire_format() {
        let prefix = "0123456789abcdef".repeat(6);
        for cursor in [
            SyncCursor::Deletions(ordinal(i64::MAX)),
            SyncCursor::Findings(ObservationCursor {
                category: FindingCategory::Conflict,
                ordinal: ordinal(10),
            }),
            SyncCursor::Conflicts {
                revision: "ab".repeat(16).parse().unwrap(),
                ordinal: ordinal(0),
            },
        ] {
            let raw = cursor.encode(&prefix);
            assert_eq!(SyncCursor::decode(&raw, &prefix).unwrap(), cursor);
            assert!(SyncCursor::decode(&raw, "different").is_err());
        }
        assert_eq!(
            SyncCursor::Deletions(ordinal(10)).encode(""),
            "000000000000000a"
        );
        for suffix in [
            "-000000000000001",
            "+000000000000001",
            "000000000000000A",
            "8000000000000000",
            "000000000000000",
            "00000000000000000",
            "050000000000000001",
            "é000000000000000",
            "000000000000000g",
            &format!("{}0000000000000001", "AB".repeat(16)),
        ] {
            assert_eq!(
                SyncCursor::decode(&format!("{prefix}{suffix}"), &prefix)
                    .unwrap_err()
                    .status,
                hyper::StatusCode::CONFLICT,
                "{suffix}"
            );
        }
    }
}
