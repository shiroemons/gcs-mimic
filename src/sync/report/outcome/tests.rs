use super::*;

#[test]
fn wait_reasons_parse_from_the_names_they_serialize_as() {
    for reason in WaitReason::ALL {
        assert_eq!(reason.as_str().parse::<WaitReason>(), Ok(reason));
        assert_eq!(
            serde_json::to_value(reason).unwrap(),
            serde_json::Value::from(reason.as_str())
        );
    }
    assert!("ready".parse::<WaitReason>().is_err());
    assert!("".parse::<WaitReason>().is_err());
}

/// Every way a wait ends has one outcome, and the outcome alone decides the exit status and
/// whether the command asks again; the HTTP status follows the reason, with 408 only for a wait
/// whose time ran out.
#[test]
fn a_wait_reason_decides_the_outcome_the_exit_status_and_the_http_status() {
    use hyper::StatusCode;
    for (reason, outcome, code, asks_again, status) in [
        (WaitReason::Ready, Outcome::Ready, 0, false, StatusCode::OK),
        (
            WaitReason::Timeout,
            Outcome::InProgress,
            3,
            true,
            StatusCode::REQUEST_TIMEOUT,
        ),
        (
            WaitReason::SourceChanging,
            Outcome::InProgress,
            3,
            true,
            StatusCode::CONFLICT,
        ),
        (
            WaitReason::JournalChanged,
            Outcome::InProgress,
            3,
            true,
            StatusCode::CONFLICT,
        ),
        (
            WaitReason::Paused,
            Outcome::ActionNeeded,
            2,
            false,
            StatusCode::CONFLICT,
        ),
        (
            WaitReason::SourceUnrepresentable,
            Outcome::ActionNeeded,
            2,
            false,
            StatusCode::CONFLICT,
        ),
        (
            WaitReason::DeletionApproval,
            Outcome::ActionNeeded,
            2,
            false,
            StatusCode::CONFLICT,
        ),
        (
            WaitReason::FindingsPending,
            Outcome::ActionNeeded,
            2,
            false,
            StatusCode::CONFLICT,
        ),
        (
            WaitReason::Stopped,
            Outcome::Failed,
            1,
            false,
            StatusCode::CONFLICT,
        ),
    ] {
        assert_eq!(reason.outcome(), outcome, "{reason}");
        assert_eq!(outcome.exit_code(), code, "{reason}");
        assert_eq!(outcome.asks_again(), asks_again, "{reason}");
        assert_eq!(reason.http_status(), status, "{reason}");
    }
}
