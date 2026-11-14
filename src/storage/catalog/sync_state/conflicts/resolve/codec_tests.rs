use super::*;

#[test]
fn operation_target_kinds_keep_their_stored_integers() {
    let connection = Connection::open_in_memory().unwrap();
    for (kind, stored) in [(TargetKind::Conflict, 0), (TargetKind::Recovery, 1)] {
        assert_eq!(
            sql::scalar::<i64>(&connection, "SELECT ?1", [&kind]).unwrap(),
            stored
        );
        assert_eq!(
            sql::scalar::<TargetKind>(&connection, "SELECT ?1", [stored]).unwrap(),
            kind
        );
    }
    for stored in [-1, 2] {
        let error = sql::scalar::<TargetKind>(&connection, "SELECT ?1", [stored]).unwrap_err();
        assert!(matches!(unavailable(error), CatalogError::Corrupt(_)));
    }
}

#[test]
fn resolution_payload_keeps_the_stored_json_bytes() {
    for (target, expected) in [
        (
            ResolveTarget::Conflict(ConflictTarget {
                conflict_id: "0123456789abcdef0123456789abcdef".parse().unwrap(),
                revision: "fedcba9876543210fedcba9876543210".parse().unwrap(),
                expected_generation: 42,
            }),
            r#"{"operation_id":"operation","action":"restore","target":{"Conflict":{"conflict_id":"0123456789abcdef0123456789abcdef","revision":"fedcba9876543210fedcba9876543210","expected_generation":42}}}"#,
        ),
        (
            ResolveTarget::Recovery(RecoveryTarget {
                recovery_id: "00112233445566778899aabbccddeeff".parse().unwrap(),
                revision: "fedcba9876543210fedcba9876543210".parse().unwrap(),
                target_identity: "ffeeddccbbaa99887766554433221100".parse().unwrap(),
                confirmation: Confirmation::BackupVerified,
            }),
            r#"{"operation_id":"operation","action":"restore","target":{"Recovery":{"recovery_id":"00112233445566778899aabbccddeeff","revision":"fedcba9876543210fedcba9876543210","target_identity":"ffeeddccbbaa99887766554433221100","confirmation":"backup_verified"}}}"#,
        ),
        (
            ResolveTarget::Recovery(RecoveryTarget {
                recovery_id: "00112233445566778899aabbccddeeff".parse().unwrap(),
                revision: "fedcba9876543210fedcba9876543210".parse().unwrap(),
                target_identity: "ffeeddccbbaa99887766554433221100".parse().unwrap(),
                confirmation: Confirmation::DiscardWithoutBackup,
            }),
            r#"{"operation_id":"operation","action":"restore","target":{"Recovery":{"recovery_id":"00112233445566778899aabbccddeeff","revision":"fedcba9876543210fedcba9876543210","target_identity":"ffeeddccbbaa99887766554433221100","confirmation":"discard_without_backup"}}}"#,
        ),
    ] {
        for action in [Action::Restore, Action::Discard] {
            let request = ResolveRequest {
                operation_id: "operation".into(),
                action,
                target: target.clone(),
            };
            let expected = if action == Action::Discard {
                expected.replace(r#""action":"restore""#, r#""action":"discard""#)
            } else {
                expected.into()
            };
            assert_eq!(serde_json::to_vec(&request).unwrap(), expected.as_bytes());
        }
    }
}
