use super::*;

fn text_values<
    T: rusqlite::ToSql + rusqlite::types::FromSql + serde::Serialize + std::fmt::Debug + PartialEq,
>(
    values: &[(T, &str)],
) {
    let connection = Connection::open_in_memory().unwrap();
    for (variant, stored) in values {
        assert_eq!(
            sql::scalar::<String>(&connection, "SELECT ?1", [variant]).unwrap(),
            *stored
        );
        assert_eq!(
            &sql::scalar::<T>(&connection, "SELECT ?1", [stored]).unwrap(),
            variant
        );
        assert_eq!(
            serde_json::to_vec(variant).unwrap(),
            format!("\"{stored}\"").as_bytes()
        );
    }
    for query in ["SELECT 'unknown'", "SELECT 1", "SELECT NULL"] {
        let error = sql::scalar::<T>(&connection, query, []).unwrap_err();
        assert!(matches!(unavailable(error), CatalogError::Corrupt(_)));
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
    text_values(&[
        (writeback::Stop::Ready, "Ready"),
        (writeback::Stop::RecoveryRequired, "RecoveryRequired"),
        (writeback::Stop::ProtectedBody, "ProtectedBody"),
        (writeback::Stop::ConflictCapacity, "ConflictCapacity"),
        (writeback::Stop::IoFailure, "IoFailure"),
    ]);
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
