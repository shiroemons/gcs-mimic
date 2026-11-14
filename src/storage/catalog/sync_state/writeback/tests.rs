use super::*;

impl Token {
    pub(crate) fn id(&self) -> OpaqueId {
        self.id
    }
}
impl Writeback {
    pub(crate) async fn page(&self, after: i64, limit: usize) -> Result<Vec<Pending>, Error> {
        self.serial_with(move |this| async move { this.page_inner(after, limit).await })
            .await
    }
}

#[test]
fn complete_phase_matches_static_active_row_predicates() {
    let connection = Connection::open_in_memory().unwrap();
    assert!(SCHEMA.contains("WHERE phase != 4"));
    assert!(SELECT_PENDING_PAGE.contains("phase!=4"));
    for phase in [
        Phase::Prepared,
        Phase::Armed,
        Phase::Protected,
        Phase::Durable,
        Phase::Complete,
    ] {
        assert_eq!(
            sql::scalar::<bool>(&connection, "SELECT ?1 != 4", [phase]).unwrap(),
            phase != Phase::Complete
        );
    }
}

/// Write-back operation IDs are stored as 32 lowercase hexadecimal digits; a row holding any
/// other ID is a corrupt catalog rather than an operation.
#[test]
fn write_back_rows_with_a_malformed_operation_id_are_corrupt() {
    let connection = Connection::open_in_memory().unwrap();
    let catalog = CatalogId::from_bytes([1; 16]);
    let root: RootId = "000102030405060708090a0b0c0d0e0f".parse().unwrap();
    let read = |id: &str| {
        sql::row(
            &connection,
            "SELECT ?1,1,'project','bucket','00000000000000000000000000000001','name','path',1,0,0,1,0,0,?2,NULL,0,?3,0",
            params![id, [0u8; 8], [0u8; 16]],
            |row| pending(row, catalog, root),
        )
    };
    let stored = "0123456789abcdef0123456789abcdef";
    assert_eq!(read(stored).unwrap().token.id().to_string(), stored);
    for id in [
        "0123456789ABCDEF0123456789ABCDEF",
        "0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdefa",
        "../workspace-entry-name-length32",
    ] {
        let error = read(id).err().unwrap();
        assert!(
            matches!(error, rusqlite::Error::FromSqlConversionFailure(0, _, _)),
            "{id}"
        );
        assert!(
            matches!(Error::from(error), Error::Admin(AdminError::Corrupt(_))),
            "{id}"
        );
    }
}

#[test]
fn retained_limits_match_static_schema_bounds() {
    use super::super::conflicts::{MAX_BYTES, MAX_ENTRIES};
    assert!(SCHEMA.contains(&format!("max_bytes BETWEEN 1 AND {MAX_BYTES}")));
    assert!(SCHEMA.contains(&format!("max_entries BETWEEN 1 AND {MAX_ENTRIES}")));
    assert!(SCHEMA.contains(&format!("entries BETWEEN 0 AND {MAX_ENTRIES}")));
}
