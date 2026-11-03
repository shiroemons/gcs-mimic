#![deny(clippy::unwrap_used, clippy::expect_used)]

use rusqlite::{
    ToSql,
    types::{FromSql, FromSqlError, FromSqlResult, ToSqlOutput, ValueRef},
};
use std::{
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BlobId([u8; 16]);

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct InvalidBlobId;

impl fmt::Display for InvalidBlobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid opaque blob ID")
    }
}

impl std::error::Error for InvalidBlobId {}

impl FromStr for BlobId {
    type Err = InvalidBlobId;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text).ok_or(InvalidBlobId)
    }
}

impl fmt::Display for BlobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl ToSql for BlobId {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Owned(self.to_hex().into()))
    }
}

impl FromSql for BlobId {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|error| FromSqlError::Other(Box::new(error)))
    }
}

impl BlobId {
    pub(crate) fn generate() -> Result<Self, getrandom::Error> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        if text.len() != 32 {
            return None;
        }
        let mut bytes = [0; 16];
        for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().as_chunks::<2>().0) {
            let digit = |value| match value {
                b'0'..=b'9' => Some(value - b'0'),
                b'a'..=b'f' => Some(value - b'a' + 10),
                _ => None,
            };
            *byte = digit(pair[0])? * 16 + digit(pair[1])?;
        }
        Some(Self(bytes))
    }

    pub(crate) fn to_hex(self) -> String {
        crate::hashing::encode_lower(&self.0)
    }

    pub(crate) fn blob_path(self, data_dir: &Path) -> PathBuf {
        let text = self.to_hex();
        data_dir.join("blobs").join(&text[..2]).join(text)
    }

    pub(crate) fn staging_path(self, data_dir: &Path) -> PathBuf {
        data_dir.join("staging").join(self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn canonical_ids_preserve_names_shards_and_leading_zeroes() {
        let text = "000102030405060708090a0b0c0d0e0f";
        let id = BlobId::parse(text).unwrap();
        assert_eq!(id.0, std::array::from_fn(|index| index as u8));
        assert_eq!(id.to_hex(), text);
        assert_eq!(id.to_string(), text);
        assert_eq!(text.parse::<BlobId>().unwrap(), id);
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        assert_eq!(
            connection
                .query_row("SELECT ?1", [id], |row| row.get::<_, String>(0))
                .unwrap(),
            text
        );
        assert_eq!(
            connection
                .query_row("SELECT ?1", [id], |row| row.get::<_, BlobId>(0))
                .unwrap(),
            id
        );
        let root = Path::new("catalog");
        assert_eq!(id.blob_path(root), root.join("blobs").join("00").join(text));
        assert_eq!(id.staging_path(root), root.join("staging").join(text));
        let generated = BlobId::generate().unwrap();
        assert_eq!(BlobId::parse(&generated.to_hex()), Some(generated));
        for invalid in [
            "",
            &"0".repeat(31),
            &"0".repeat(33),
            &"A".repeat(32),
            &"g".repeat(32),
            &"/".repeat(32),
            &"é".repeat(16),
        ] {
            assert_eq!(BlobId::parse(invalid), None);
            assert_eq!(invalid.parse::<BlobId>(), Err(InvalidBlobId));
            let error = connection
                .query_row("SELECT ?1", [invalid], |row| row.get::<_, BlobId>(0))
                .unwrap_err();
            assert!(matches!(
                error,
                rusqlite::Error::FromSqlConversionFailure(0, _, _)
            ));
            assert!(matches!(
                crate::storage::failure::unavailable(error),
                crate::bucket::CatalogError::Corrupt(_)
            ));
        }
        assert!(matches!(
            connection.query_row("SELECT 1", [], |row| row.get::<_, BlobId>(0)),
            Err(rusqlite::Error::InvalidColumnType(0, _, _))
        ));
    }
}
