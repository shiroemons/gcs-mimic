use super::*;

impl Token {
    pub(crate) fn id(&self) -> &str {
        &self.id
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

#[test]
fn retained_limits_match_static_schema_bounds() {
    use super::super::conflicts::{MAX_BYTES, MAX_ENTRIES};
    assert!(SCHEMA.contains(&format!("max_bytes BETWEEN 1 AND {MAX_BYTES}")));
    assert!(SCHEMA.contains(&format!("max_entries BETWEEN 1 AND {MAX_ENTRIES}")));
    assert!(SCHEMA.contains(&format!("entries BETWEEN 0 AND {MAX_ENTRIES}")));
}
