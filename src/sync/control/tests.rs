use super::*;

/// A decision parses from its command, which the control route ends with and the accepted
/// answer names it by, and from nothing else.
#[test]
fn decisions_parse_from_the_command_their_route_ends_with() {
    for decision in Decision::ALL {
        assert_eq!(decision.as_str().parse::<Decision>(), Ok(decision));
        assert_eq!(
            decision.path(),
            format!("/_internal/sync/{}", decision.as_str())
        );
        assert_eq!(
            serde_json::to_value(decision).unwrap(),
            serde_json::Value::from(decision.as_str())
        );
    }
    for name in ["", "Approve", "approved", "/_internal/sync/approve"] {
        assert!(name.parse::<Decision>().is_err(), "{name}");
    }
}
