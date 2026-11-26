use super::*;
use hyper::header::HeaderValue;

#[test]
fn opaque_tokens_admit_bytes_that_text_tokens_refuse() {
    let mut headers = HeaderMap::new();
    assert!(IdempotencyToken::validate_opaque(&headers).is_ok());
    assert!(
        IdempotencyToken::from_text(&headers, TextOrder::RepetitionFirst)
            .unwrap()
            .is_none()
    );
    headers.insert(
        IdempotencyToken::HEADER,
        HeaderValue::from_bytes(&[0xff]).unwrap(),
    );
    assert!(IdempotencyToken::validate_opaque(&headers).is_ok());
    assert!(matches!(
        IdempotencyToken::from_text(&headers, TextOrder::ValueFirst),
        Err(TextTokenError::Header(TextError::Invalid(_)))
    ));
    headers.append(
        IdempotencyToken::HEADER,
        HeaderValue::from_static("repeated"),
    );
    assert!(matches!(
        IdempotencyToken::validate_opaque(&headers),
        Err(OpaqueTokenError::Repeated(_))
    ));
    assert!(matches!(
        IdempotencyToken::from_text(&headers, TextOrder::RepetitionFirst),
        Err(TextTokenError::Header(TextError::Repeated(_)))
    ));
    assert!(matches!(
        IdempotencyToken::from_text(&headers, TextOrder::ValueFirst),
        Err(TextTokenError::Header(TextError::Invalid(_)))
    ));
}

#[test]
fn tokens_refuse_empty_values_and_keep_nonempty_spelling() {
    let mut headers = HeaderMap::new();
    for empty in ["", " "] {
        headers.insert(IdempotencyToken::HEADER, HeaderValue::from_static(empty));
        assert!(matches!(
            IdempotencyToken::validate_opaque(&headers),
            Err(OpaqueTokenError::Empty)
        ));
        assert!(matches!(
            IdempotencyToken::from_text(&headers, TextOrder::RepetitionFirst),
            Err(TextTokenError::Empty)
        ));
    }
    headers.insert(
        IdempotencyToken::HEADER,
        HeaderValue::from_static(" opaque, token=1 "),
    );
    assert!(IdempotencyToken::validate_opaque(&headers).is_ok());
    let token = IdempotencyToken::from_text(&headers, TextOrder::RepetitionFirst)
        .unwrap()
        .unwrap();
    assert_eq!(token.as_str(), " opaque, token=1 ");
}
