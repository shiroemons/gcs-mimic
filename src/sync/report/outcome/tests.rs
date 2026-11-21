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
/// whether the command asks again.
#[test]
fn a_wait_reason_decides_the_outcome_the_exit_status_and_whether_to_ask_again() {
    use Unsettled::{ActionNeeded, Failed, InProgress};
    for (reason, outcome, code, asks_again) in [
        (WaitReason::Ready, Outcome::Ready, 0, false),
        (WaitReason::Timeout, Outcome::Unsettled(InProgress), 3, true),
        (
            WaitReason::SourceChanging,
            Outcome::Unsettled(InProgress),
            3,
            true,
        ),
        (
            WaitReason::JournalChanged,
            Outcome::Unsettled(InProgress),
            3,
            true,
        ),
        (
            WaitReason::Paused,
            Outcome::Unsettled(ActionNeeded),
            2,
            false,
        ),
        (
            WaitReason::SourceUnrepresentable,
            Outcome::Unsettled(ActionNeeded),
            2,
            false,
        ),
        (
            WaitReason::DeletionApproval,
            Outcome::Unsettled(ActionNeeded),
            2,
            false,
        ),
        (
            WaitReason::FindingsPending,
            Outcome::Unsettled(ActionNeeded),
            2,
            false,
        ),
        (WaitReason::Stopped, Outcome::Unsettled(Failed), 1, false),
    ] {
        assert_eq!(reason.outcome(), outcome, "{reason}");
        assert_eq!(outcome.exit_code(), code, "{reason}");
        assert_eq!(outcome.asks_again(), asks_again, "{reason}");
    }
}
