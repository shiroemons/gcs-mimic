use super::{JsonError, hex, stale};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SyncCursor {
    Findings { kind: i64, ordinal: i64 },
    Deletions(i64),
    Conflicts { revision: String, ordinal: i64 },
}

impl SyncCursor {
    pub(super) fn encode(&self, prefix: &str) -> String {
        match self {
            Self::Findings { kind, ordinal } => format!("{prefix}{kind:02x}{ordinal:016x}"),
            Self::Deletions(ordinal) => format!("{prefix}{ordinal:016x}"),
            Self::Conflicts { revision, ordinal } => {
                format!("{prefix}{revision}{ordinal:016x}")
            }
        }
    }

    pub(super) fn decode(raw: &str, prefix: &str) -> Result<Self, JsonError> {
        let suffix = raw.strip_prefix(prefix).ok_or_else(stale)?;
        if !hex(suffix, suffix.len()) {
            return Err(stale());
        }
        let number = |raw| i64::from_str_radix(raw, 16).map_err(|_| stale());
        match suffix.len() {
            16 => Ok(Self::Deletions(number(suffix)?)),
            18 => {
                let kind = number(&suffix[..2])?;
                if kind > 4 {
                    return Err(stale());
                }
                Ok(Self::Findings {
                    kind,
                    ordinal: number(&suffix[2..])?,
                })
            }
            48 => Ok(Self::Conflicts {
                revision: suffix[..32].into(),
                ordinal: number(&suffix[32..])?,
            }),
            _ => Err(stale()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_preserve_fixed_lowercase_wire_format() {
        let prefix = "0123456789abcdef".repeat(6);
        for cursor in [
            SyncCursor::Deletions(i64::MAX),
            SyncCursor::Findings {
                kind: 4,
                ordinal: 10,
            },
            SyncCursor::Conflicts {
                revision: "ab".repeat(16),
                ordinal: 0,
            },
        ] {
            let raw = cursor.encode(&prefix);
            assert_eq!(SyncCursor::decode(&raw, &prefix).unwrap(), cursor);
            assert!(SyncCursor::decode(&raw, "different").is_err());
        }
        assert_eq!(SyncCursor::Deletions(10).encode(""), "000000000000000a");
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
        ] {
            assert_eq!(
                SyncCursor::decode(&format!("{prefix}{suffix}"), &prefix)
                    .unwrap_err()
                    .status,
                hyper::StatusCode::CONFLICT,
                "{suffix}"
            );
        }
        let uppercase_revision = format!("{prefix}{}0000000000000001", "AB".repeat(16));
        assert!(SyncCursor::decode(&uppercase_revision, &prefix).is_err());
    }
}
