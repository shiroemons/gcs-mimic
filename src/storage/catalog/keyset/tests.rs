use super::*;
use rusqlite::{Connection, OptionalExtension};

/// Pages `(value, charge)` rows of `VALUES` with the given limit and budget.
fn page(sql: &str, limit: usize, budget: usize) -> Page<i64> {
    let connection = Connection::open_in_memory().unwrap();
    let mut statement = connection.prepare(sql).unwrap();
    let mut rows = statement.query([]).unwrap();
    budgeted_page(
        &mut rows,
        limit,
        budget,
        |row| Ok(row.get(1)?),
        |row| Ok(row.get(0)?),
    )
    .unwrap()
}

const ROWS: &str = "SELECT * FROM (VALUES (1, 10), (2, 10), (3, 10))";

#[test]
fn a_budgeted_page_stops_at_its_limit_or_budget() {
    let full = page(ROWS, 2, 100);
    assert_eq!(full.items, [1, 2]);
    assert!(full.has_more);
    let budgeted = page(ROWS, 3, 20);
    assert_eq!(budgeted.items, [1, 2], "the third row exceeds what remains");
    assert!(budgeted.has_more);
    let exhausted = page(ROWS, 3, 30);
    assert_eq!(exhausted.items, [1, 2, 3]);
    assert!(!exhausted.has_more);
    let empty = page("SELECT * FROM (VALUES (1, 10)) WHERE column1 > 1", 3, 30);
    assert!(empty.items.is_empty());
    assert!(!empty.has_more);
}

/// A row larger than the whole budget still fills an empty page, so a scan always advances;
/// a page of no rows has nothing to continue.
#[test]
fn an_oversized_row_fills_an_empty_page_and_a_zero_limit_reads_nothing() {
    let oversized = page(ROWS, 3, 5);
    assert_eq!(oversized.items, [1]);
    assert!(oversized.has_more);
    let zero = page(ROWS, 0, 100);
    assert!(zero.items.is_empty());
    assert!(!zero.has_more);
}

/// A scan starts at `''` and resumes from the key's own TEXT, so binding a key that `Ord` sorts
/// first resumes at the next key in SQLite's order too: no row is skipped or read twice.
#[test]
fn cursors_bind_an_empty_start_and_keys_in_sqlite_text_order() {
    use crate::storage::BlobId;
    let connection = Connection::open_in_memory().unwrap();
    let text = |after: &dyn rusqlite::ToSql| -> String {
        connection
            .query_row("SELECT ?1", [after], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(text(&After::<BlobId>::Start), "");
    assert_eq!(text(&After::<String>::Start), "");
    assert_eq!(text(&After::Key("bucket".to_owned())), "bucket");
    let mut ids: Vec<BlobId> = [
        "f0000000000000000000000000000000",
        "00000000000000000000000000000001",
        "0a000000000000000000000000000000",
        "a0000000000000000000000000000000",
    ]
    .into_iter()
    .map(|id| id.parse().unwrap())
    .collect();
    ids.sort();
    connection
        .execute_batch("CREATE TABLE ids (id TEXT PRIMARY KEY) WITHOUT ROWID")
        .unwrap();
    for id in &ids {
        connection
            .execute("INSERT INTO ids VALUES (?1)", [id])
            .unwrap();
        assert_eq!(text(&After::Key(*id)), id.to_string());
    }
    let mut after = After::Start;
    let mut scanned = Vec::new();
    while let Some(id) = connection
        .query_row(
            "SELECT id FROM ids WHERE id > ?1 ORDER BY id LIMIT 1",
            [&after],
            |row| row.get::<_, BlobId>(0),
        )
        .optional()
        .unwrap()
    {
        scanned.push(id);
        after = After::Key(id);
    }
    assert_eq!(scanned, ids);
}

#[test]
fn page_limits_accept_one_through_their_maximum() {
    for (limit, valid) in [(0, false), (1, true), (256, true), (257, false)] {
        let parsed = PageLimit::<256>::try_from(limit);
        assert_eq!(parsed.is_ok(), valid, "{limit}");
        if let Ok(parsed) = parsed {
            assert_eq!(parsed.get(), limit);
        }
    }
    assert_eq!(
        PageLimit::<256>::try_from(usize::MAX),
        Err(InvalidPageLimit)
    );
    assert_eq!(PageLimit::<257>::LARGEST.get(), 257);
    assert_eq!(
        PageLimit::<256>::LARGEST.with_lookahead::<257>(),
        PageLimit::<257>::LARGEST
    );
    let connection = Connection::open_in_memory().unwrap();
    let bound: i64 = connection
        .query_row("SELECT ?1", [PageLimit::<256>::LARGEST], |row| row.get(0))
        .unwrap();
    assert_eq!(bound, 256);
}

#[test]
fn ordinals_are_non_negative_and_a_stored_negative_one_is_corrupt() {
    assert_eq!(Ordinal::try_from(-1), Err(InvalidOrdinal));
    assert_eq!(Ordinal::try_from(0), Ok(Ordinal::START));
    assert_eq!(Ordinal::try_from(i64::MAX).map(Ordinal::get), Ok(i64::MAX));
    assert_eq!(Ordinal::default(), Ordinal::START);
    let connection = Connection::open_in_memory().unwrap();
    let ordinal = Ordinal::try_from(7).unwrap();
    let read: Ordinal = connection
        .query_row("SELECT ?1", [ordinal], |row| row.get(0))
        .unwrap();
    assert_eq!(read, ordinal);
    let error = connection
        .query_row("SELECT -1", [], |row| row.get::<_, Ordinal>(0))
        .unwrap_err();
    assert!(matches!(
        error,
        rusqlite::Error::IntegralValueOutOfRange(0, -1)
    ));
    assert!(matches!(
        super::super::unavailable(error),
        CatalogError::Corrupt(_)
    ));
}
