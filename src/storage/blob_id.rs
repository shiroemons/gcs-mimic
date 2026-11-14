#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

crate::storage::opaque_id::opaque_id! {
    pub(crate) struct BlobId([u8; 16]);
    invalid = "invalid opaque blob ID";
    column = text;
}

impl BlobId {
    pub(crate) fn generate() -> Result<Self, getrandom::Error> {
        crate::storage::opaque_id::random().map(Self)
    }

    pub(crate) fn blob_path(self, data_dir: &Path) -> PathBuf {
        let text = self.to_hex();
        let [first, ..] = self.0;
        data_dir
            .join("blobs")
            .join(format!("{first:02x}"))
            .join(text)
    }

    pub(crate) fn staging_path(self, data_dir: &Path) -> PathBuf {
        data_dir.join("staging").join(self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::opaque_id::InvalidOpaqueId;

    #[test]
    fn canonical_ids_preserve_names_shards_and_leading_zeroes() {
        let text = "000102030405060708090a0b0c0d0e0f";
        let id: BlobId = text.parse().unwrap();
        assert_eq!(id.0, std::array::from_fn(|index| index as u8));
        assert_eq!(id.to_hex(), text);
        assert_eq!(id.to_string(), text);
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
        assert_eq!(generated.to_hex().parse(), Ok(generated));
        for invalid in [
            "",
            "a",
            "../../protected",
            &"0".repeat(31),
            &"0".repeat(33),
            &"A".repeat(32),
            &"g".repeat(32),
            &"/".repeat(32),
            &"é".repeat(16),
        ] {
            assert_eq!(
                invalid.parse::<BlobId>(),
                Err(InvalidOpaqueId::new("invalid opaque blob ID"))
            );
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

    /// Fixed-length lowercase hex sorts like its bytes, so a keyset cursor may compare the
    /// typed ID or the stored text.
    #[test]
    fn ordering_matches_sqlite_text_order() {
        let mut ids: Vec<BlobId> = (0..64).map(|_| BlobId::generate().unwrap()).collect();
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE ids (id TEXT NOT NULL)")
            .unwrap();
        for id in &ids {
            connection
                .execute("INSERT INTO ids VALUES (?1)", [id])
                .unwrap();
        }
        let ordered: Vec<BlobId> = connection
            .prepare("SELECT id FROM ids ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        ids.sort();
        assert_eq!(ordered, ids);
    }

    #[test]
    fn serde_uses_the_lowercase_text_form() {
        let id: BlobId = "000102030405060708090a0b0c0d0e0f".parse().unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"000102030405060708090a0b0c0d0e0f\"");
        assert_eq!(serde_json::from_str::<BlobId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<BlobId>("\"000102030405060708090A0B0C0D0E0F\"").is_err());
        assert_eq!(
            format!("{id:?}"),
            "BlobId(000102030405060708090a0b0c0d0e0f)"
        );
    }
}
