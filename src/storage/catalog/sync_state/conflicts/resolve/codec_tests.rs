use super::*;

#[test]
fn resolution_payload_keeps_the_stored_json_bytes() {
    for (target, expected) in [
        (
            ResolveTarget::Conflict {
                conflict_id: "conflict".into(),
                revision: "revision".into(),
                expected_generation: Some(42),
            },
            r#"{"operation_id":"operation","action":"restore","target":{"Conflict":{"conflict_id":"conflict","revision":"revision","expected_generation":42}}}"#,
        ),
        (
            ResolveTarget::Conflict {
                conflict_id: "conflict".into(),
                revision: "revision".into(),
                expected_generation: None,
            },
            r#"{"operation_id":"operation","action":"restore","target":{"Conflict":{"conflict_id":"conflict","revision":"revision","expected_generation":null}}}"#,
        ),
        (
            ResolveTarget::Recovery {
                recovery_id: "recovery".into(),
                revision: "revision".into(),
                target_identity: "identity".into(),
                confirmation: Some(Confirmation::BackupVerified),
            },
            r#"{"operation_id":"operation","action":"restore","target":{"Recovery":{"recovery_id":"recovery","revision":"revision","target_identity":"identity","confirmation":"backup_verified"}}}"#,
        ),
        (
            ResolveTarget::Recovery {
                recovery_id: "recovery".into(),
                revision: "revision".into(),
                target_identity: "identity".into(),
                confirmation: Some(Confirmation::DiscardWithoutBackup),
            },
            r#"{"operation_id":"operation","action":"restore","target":{"Recovery":{"recovery_id":"recovery","revision":"revision","target_identity":"identity","confirmation":"discard_without_backup"}}}"#,
        ),
        (
            ResolveTarget::Recovery {
                recovery_id: "recovery".into(),
                revision: "revision".into(),
                target_identity: "identity".into(),
                confirmation: None,
            },
            r#"{"operation_id":"operation","action":"restore","target":{"Recovery":{"recovery_id":"recovery","revision":"revision","target_identity":"identity","confirmation":null}}}"#,
        ),
    ] {
        for action in [Action::Restore, Action::Discard] {
            let request = ResolveRequest {
                operation_id: "operation".into(),
                action,
                target: target.clone(),
            };
            let encoded = serde_json::to_vec(&request).unwrap();
            let expected = if action == Action::Discard {
                expected.replace("\"restore\"", "\"discard\"")
            } else {
                expected.into()
            };
            assert_eq!(encoded, expected.as_bytes());
            assert_eq!(
                serde_json::from_slice::<ResolveRequest>(&encoded).unwrap(),
                request
            );
        }
    }
}
