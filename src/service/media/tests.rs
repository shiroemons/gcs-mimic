use super::*;

#[test]
fn range_classification_is_shared_and_ignores_repeated_fields() {
    for (value, expected) in [
        ("bytes=0-1,3-4", RangeRequest::Multiple("bytes=0-1,3-4")),
        ("BYTES=0-1", RangeRequest::Single("0-1")),
        ("items=0-1", RangeRequest::Whole),
    ] {
        let mut headers = hyper::HeaderMap::new();
        headers.insert("range", value.parse().unwrap());
        assert_eq!(range_request(&headers), expected);
        headers.append("range", "bytes=2-3".parse().unwrap());
        assert_eq!(range_request(&headers), RangeRequest::Whole);
        assert_eq!(range_request(&headers).resolve(10).unwrap(), None);
    }
}

#[test]
fn only_a_nonzero_suffix_of_an_empty_object_is_an_empty_suffix() {
    for (range, size, expected) in [
        ("-5", 0, true),
        (" -1 ", 0, true),
        ("-0", 0, false),
        ("-5", 1, false),
        ("0-", 0, false),
        ("-x", 0, false),
    ] {
        assert_eq!(
            RangeRequest::Single(range).is_suffix_of_empty(size),
            expected,
            "{range} of {size}"
        );
    }
    assert!(!RangeRequest::Whole.is_suffix_of_empty(0));
}
