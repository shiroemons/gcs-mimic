use super::Diagnostics;

#[track_caller]
pub(crate) fn has(errors: &Diagnostics, key: &str, fix: &str) {
    assert!(
        errors
            .errors
            .iter()
            .any(|error| error.key == key && error.fix.contains(fix)),
        "{errors}"
    );
}
