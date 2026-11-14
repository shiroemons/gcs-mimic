use super::*;

#[test]
fn finding_rows_reject_unknown_persisted_reasons_instead_of_using_a_remedy_fallback() {
    let connection = Connection::open_in_memory().unwrap();
    let query = "SELECT 0,1,?1,NULL,NULL,NULL,NULL,?2,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL";
    for (category, reason) in [
        ("mapping", "collision"),
        ("path-stop", "IoFailure"),
        ("root-stop", "observer-failure"),
    ] {
        let item = sql::row(&connection, query, [category, reason], read_item).unwrap();
        assert_eq!(item.category.as_str(), category);
        assert_eq!(item.reason.as_str(), reason);
        assert_eq!(
            serde_json::to_vec(&item.reason).unwrap(),
            format!("\"{reason}\"").as_bytes()
        );
        let error = sql::row(&connection, query, [category, "unknown"], read_item).unwrap_err();
        assert!(matches!(unavailable(error), CatalogError::Corrupt(_)));
    }
    let error = sql::row(&connection, query, ["unknown", "IoFailure"], read_item).unwrap_err();
    assert!(matches!(unavailable(error), CatalogError::Corrupt(_)));
}

#[track_caller]
fn text_values<
    T: rusqlite::ToSql + rusqlite::types::FromSql + serde::Serialize + std::fmt::Debug + PartialEq,
>(
    values: &[(T, &str)],
) {
    let connection = Connection::open_in_memory().unwrap();
    for (variant, stored) in values {
        assert_eq!(
            sql::scalar::<String>(&connection, "SELECT ?1", [variant]).unwrap(),
            *stored,
            "{stored}"
        );
        assert_eq!(
            &sql::scalar::<T>(&connection, "SELECT ?1", [stored]).unwrap(),
            variant,
            "{stored}"
        );
        assert_eq!(
            serde_json::to_vec(variant).unwrap(),
            format!("\"{stored}\"").as_bytes(),
            "{stored}"
        );
    }
    for query in ["SELECT 'unknown'", "SELECT 1", "SELECT NULL"] {
        let error = sql::scalar::<T>(&connection, query, []).unwrap_err();
        assert!(
            matches!(unavailable(error), CatalogError::Corrupt(_)),
            "{query}"
        );
    }
}

#[test]
fn synchronization_enums_keep_sql_and_json_spellings() {
    text_values(&[
        (PauseReason::ConflictCapacity, "ConflictCapacity"),
        (PauseReason::IoFailure, "IoFailure"),
        (PauseReason::RecoveryRequired, "RecoveryRequired"),
        (PauseReason::ObserverFailure, "observer-failure"),
        (PauseReason::SourceChanging, "SourceChanging"),
        (PauseReason::SourceUnrepresentable, "SourceUnrepresentable"),
    ]);
    text_values(&[
        (FindingCategory::Mapping, "mapping"),
        (FindingCategory::Writeback, "writeback"),
        (FindingCategory::RootStop, "root-stop"),
        (FindingCategory::PathStop, "path-stop"),
        (FindingCategory::Conflict, "conflict"),
    ]);
    for (stop, expected) in [
        (writeback::Stop::Ready, "Ready"),
        (writeback::Stop::RecoveryRequired, "RecoveryRequired"),
        (writeback::Stop::ProtectedBody, "ProtectedBody"),
        (writeback::Stop::ConflictCapacity, "ConflictCapacity"),
        (writeback::Stop::IoFailure, "IoFailure"),
    ] {
        assert_eq!(serde_json::to_value(stop).unwrap(), expected);
    }
}

#[test]
fn recovery_phases_preserve_stored_values_and_reject_unknown_values() {
    let connection = Connection::open_in_memory().unwrap();
    for (phase, stored) in [
        (RecoveryPhase::Protected, 0),
        (RecoveryPhase::DiscardAccepted, 1),
    ] {
        assert_eq!(
            sql::scalar::<i64>(&connection, "SELECT ?1", [&phase]).unwrap(),
            stored
        );
        assert_eq!(
            sql::scalar::<RecoveryPhase>(&connection, "SELECT ?1", [stored]).unwrap(),
            phase
        );
    }
    for stored in [-1, 2] {
        let error = sql::scalar::<RecoveryPhase>(&connection, "SELECT ?1", [stored]).unwrap_err();
        assert!(
            matches!(error, rusqlite::Error::IntegralValueOutOfRange(0, value) if value == stored)
        );
        assert!(matches!(unavailable(error), CatalogError::Corrupt(_)));
    }
}

/// Integer columns round-trip each variant, and any other stored value reads as corrupt.
#[track_caller]
fn integer_values<T: rusqlite::ToSql + rusqlite::types::FromSql + std::fmt::Debug + PartialEq>(
    values: &[(T, i64)],
    unknown: &[i64],
) {
    let connection = Connection::open_in_memory().unwrap();
    for (variant, stored) in values {
        assert_eq!(
            sql::scalar::<i64>(&connection, "SELECT ?1", [variant]).unwrap(),
            *stored,
            "{variant:?}"
        );
        assert_eq!(
            &sql::scalar::<T>(&connection, "SELECT ?1", [stored]).unwrap(),
            variant,
            "{stored}"
        );
    }
    for stored in unknown {
        let error = sql::scalar::<T>(&connection, "SELECT ?1", [stored]).unwrap_err();
        assert!(
            matches!(error, rusqlite::Error::IntegralValueOutOfRange(0, value) if value == *stored),
            "{stored}"
        );
        assert!(matches!(unavailable(error), CatalogError::Corrupt(_)));
    }
}

#[test]
fn conflict_states_and_deletion_authorizations_keep_their_stored_integers() {
    use super::super::{conflicts::ConflictState, deletions::DeletionAuthorization};
    integer_values(
        &[
            (ConflictState::Capturing, 0),
            (ConflictState::Registered, 1),
            (ConflictState::Discarded, 2),
        ],
        &[-1, 3],
    );
    for (state, spelling) in [
        (ConflictState::Capturing, "capturing"),
        (ConflictState::Registered, "registered"),
        (ConflictState::Discarded, "discarded"),
    ] {
        assert_eq!(
            serde_json::to_vec(&state).unwrap(),
            format!("\"{spelling}\"").as_bytes()
        );
    }
    // No release writes the third value the column check allows.
    integer_values(
        &[
            (DeletionAuthorization::Withheld, 0),
            (DeletionAuthorization::Granted, 1),
        ],
        &[-1, 2],
    );
}

#[test]
fn writeback_and_conflict_findings_spell_their_stored_integers() {
    let connection = Connection::open_in_memory().unwrap();
    let query = "SELECT 1,1,?1,NULL,NULL,NULL,NULL,?2,NULL,NULL,'revision','identity',CAST('{\"size\":3,\"hashes\":null}' AS BLOB),?3,NULL,NULL";
    for (category, stored, spelling) in [
        ("writeback", 0, "prepared"),
        ("writeback", 1, "armed"),
        ("writeback", 3, "durable"),
        ("conflict", 0, "capturing"),
        ("conflict", 1, "registered"),
    ] {
        let item = sql::row(
            &connection,
            query,
            params![category, stored, Option::<i64>::None],
            read_item,
        )
        .unwrap();
        assert_eq!(item.reason.as_str(), spelling, "{category} {stored}");
        assert_eq!(
            serde_json::to_vec(&item.reason).unwrap(),
            format!("\"{spelling}\"").as_bytes()
        );
        assert!(item.recovery.is_none());
    }
    for (stored, spelling) in [(0, "protected"), (1, "discard_accepted")] {
        let item = sql::row(
            &connection,
            query,
            params!["writeback", 2, stored],
            read_item,
        )
        .unwrap();
        assert_eq!(item.reason.as_str(), "protected");
        let recovery = item.recovery.unwrap();
        assert_eq!(recovery.phase.as_str(), spelling);
        assert_eq!(
            serde_json::to_vec(&recovery.phase).unwrap(),
            format!("\"{spelling}\"").as_bytes()
        );
    }
    for (category, stored) in [("writeback", 5), ("conflict", 3)] {
        let error = sql::row(
            &connection,
            query,
            params![category, stored, Option::<i64>::None],
            read_item,
        )
        .unwrap_err();
        assert!(
            matches!(unavailable(error), CatalogError::Corrupt(_)),
            "{category} {stored}"
        );
    }
}
