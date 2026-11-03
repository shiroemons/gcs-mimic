use super::*;

#[test]
fn resolution_payload_keeps_the_stored_json_bytes() {
    for (target, expected) in [
        (
            ResolveTarget::Conflict(ConflictTarget {
                conflict_id: "conflict".into(),
                revision: "revision".into(),
                expected_generation: 42,
            }),
            r#"{"operation_id":"operation","action":"restore","target":{"Conflict":{"conflict_id":"conflict","revision":"revision","expected_generation":42}}}"#,
        ),
        (
            ResolveTarget::Recovery(RecoveryTarget {
                recovery_id: "recovery".into(),
                revision: "revision".into(),
                target_identity: "identity".into(),
                confirmation: Confirmation::BackupVerified,
            }),
            r#"{"operation_id":"operation","action":"restore","target":{"Recovery":{"recovery_id":"recovery","revision":"revision","target_identity":"identity","confirmation":"backup_verified"}}}"#,
        ),
        (
            ResolveTarget::Recovery(RecoveryTarget {
                recovery_id: "recovery".into(),
                revision: "revision".into(),
                target_identity: "identity".into(),
                confirmation: Confirmation::DiscardWithoutBackup,
            }),
            r#"{"operation_id":"operation","action":"restore","target":{"Recovery":{"recovery_id":"recovery","revision":"revision","target_identity":"identity","confirmation":"discard_without_backup"}}}"#,
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
