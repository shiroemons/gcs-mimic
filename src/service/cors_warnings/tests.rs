use super::*;

#[test]
fn unconfigured_warning_is_once_per_bucket_and_bounded() {
    let warned = CorsWarnings::default();
    assert!(warned.claim("bucket-a"));
    assert!(!warned.claim("bucket-a"));
    for index in 1..WARNED_BUCKET_LIMIT {
        assert!(warned.claim(&format!("bucket-{index}")));
    }
    assert!(!warned.claim("bucket-over-limit"));
    assert_eq!(warned.0.lock().unwrap().len(), WARNED_BUCKET_LIMIT);
}
