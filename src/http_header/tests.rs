use super::*;
use std::assert_matches;

#[test]
fn sole_distinguishes_absent_opaque_and_repeated_fields() {
    let mut headers = HeaderMap::new();
    assert!(sole(&headers, "x-value").value().unwrap().is_none());
    let opaque = HeaderValue::from_bytes(&[0xff]).unwrap();
    headers.append("x-value", opaque.clone());
    assert_eq!(sole(&headers, "x-value").value().unwrap(), Some(&opaque));
    assert_matches!(
        sole(&headers, "x-value").text(TextOrder::RepetitionFirst),
        Err(TextError::Invalid(_))
    );
    headers.append("x-value", HeaderValue::from_static("ok"));
    assert!(sole(&headers, "x-value").value().is_err());
}

#[test]
fn text_order_preserves_repetition_and_first_value_error_precedence() {
    let mut headers = HeaderMap::new();
    headers.append("x-value", HeaderValue::from_bytes(&[0xff]).unwrap());
    headers.append("x-value", HeaderValue::from_static("ok"));
    assert_matches!(
        sole(&headers, "x-value").text(TextOrder::ValueFirst),
        Err(TextError::Invalid(_))
    );
    assert_matches!(
        sole(&headers, "x-value").text(TextOrder::RepetitionFirst),
        Err(TextError::Repeated(_))
    );
    headers.insert("x-value", HeaderValue::from_static(" a b "));
    for order in [TextOrder::RepetitionFirst, TextOrder::ValueFirst] {
        assert_eq!(
            sole(&headers, "x-value").text(order).unwrap(),
            Some(" a b ")
        );
    }
}
