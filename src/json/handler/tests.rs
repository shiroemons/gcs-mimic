use super::*;

/// Bodiless JSON methods read the declared length as a number, as Cloud Storage accepted
/// `Content-Length: 00` on them.
#[test]
fn bodiless_json_methods_accept_any_spelling_of_a_zero_length() {
    assert!(require_empty_body(&hyper::HeaderMap::new()).is_ok());
    for (name, value, accepted) in [
        ("content-length", "0", true),
        ("content-length", "00", true),
        ("content-length", "000", true),
        ("content-length", "5", false),
        ("content-length", "05", false),
        ("transfer-encoding", "chunked", false),
    ] {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(name, value.parse().unwrap());
        match require_empty_body(&headers) {
            Ok(()) => assert!(accepted, "{name}: {value}"),
            Err(error) => {
                assert!(!accepted, "{name}: {value}");
                assert_eq!(error.status(), StatusCode::BAD_REQUEST);
                assert_eq!(error.message(), "This method requires an empty body");
            }
        }
    }
}

#[test]
fn not_modified_response_tolerates_missing_representation_headers() {
    let response = not_modified_response(None, None);
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert!(!response.headers().contains_key("etag"));
    assert!(!response.headers().contains_key("cache-control"));
}
