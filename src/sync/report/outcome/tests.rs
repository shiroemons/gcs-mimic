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
