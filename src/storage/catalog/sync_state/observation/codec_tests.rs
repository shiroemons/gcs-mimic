use super::*;

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
