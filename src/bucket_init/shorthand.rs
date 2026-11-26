//! The `buckets` shorthand: comma-separated bucket names that stand for the document creating
//! each bucket with default settings, so `GCS_MIMIC_BUCKETS=uploads,exports` needs no JSON. A
//! document is a JSON object and starts with `{`, which no bucket name contains, so the first
//! character tells the two forms apart.

use serde_json::{Map, Value};
use std::{borrow::Cow, collections::BTreeSet};

/// Why a `buckets` value is neither a document nor a list of valid bucket names. Each names a
/// one-based position rather than the value, as the document's diagnostics name paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShorthandError {
    InvalidName { position: usize },
    RepeatedName { position: usize },
}

impl std::fmt::Display for ShorthandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // A value read the wrong way shows both forms, so the intended one is at hand.
            Self::InvalidName { position } => write!(
                f,
                "buckets does not start with {{, so it is read as comma-separated bucket names, and name {position} is not a valid bucket name; write bucket names such as uploads,exports, or a document such as {{\"version\":1,\"buckets\":[{{\"name\":\"uploads\"}}]}}."
            ),
            Self::RepeatedName { position } => write!(
                f,
                "buckets names bucket {position} more than once; list each bucket once."
            ),
        }
    }
}

impl std::error::Error for ShorthandError {}

/// The document that the `buckets` setting's `value` supplies: `value` itself when it is a
/// document, or else the canonical document its names stand for. The document's own checks then
/// see one form, whichever the operator wrote.
pub(crate) fn document(value: &str) -> Result<Cow<'_, str>, ShorthandError> {
    if value.trim_start().starts_with('{') {
        return Ok(Cow::Borrowed(value));
    }
    let mut seen = BTreeSet::new();
    let mut buckets = Vec::new();
    for (index, name) in value.split(',').map(str::trim).enumerate() {
        let position = index + 1;
        if crate::bucket::name::validate(name).is_err() {
            return Err(ShorthandError::InvalidName { position });
        }
        if !seen.insert(name) {
            return Err(ShorthandError::RepeatedName { position });
        }
        buckets.push(Value::from(Map::from_iter([(
            "name".to_owned(),
            Value::from(name),
        )])));
    }
    let document = Map::from_iter([
        ("version".to_owned(), Value::from(1)),
        ("buckets".to_owned(), Value::from(buckets)),
    ]);
    Ok(Cow::Owned(Value::from(document).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;

    #[test]
    fn names_expand_to_the_canonical_document_in_order() {
        for (value, expected) in [
            ("uploads", r#"{"version":1,"buckets":[{"name":"uploads"}]}"#),
            (
                " uploads , exports.example,cache_1 ",
                r#"{"version":1,"buckets":[{"name":"uploads"},{"name":"exports.example"},{"name":"cache_1"}]}"#,
            ),
        ] {
            assert_eq!(document(value).unwrap(), expected, "{value}");
        }
    }

    #[test]
    fn a_document_passes_through_unchanged() {
        for value in [r#"{"version":1,"buckets":[]}"#, "  {not even json"] {
            assert_matches!(document(value), Ok(Cow::Borrowed(text)) if text == value);
        }
    }

    /// A value that is neither form names the position and shows both forms, without echoing the
    /// value, which might be a mistyped document holding anything.
    #[test]
    fn invalid_names_name_their_position_and_both_forms() {
        for (value, position) in [
            ("Uploads", 1),
            ("uploads,,exports", 2),
            ("uploads,", 2),
            (r#"[{"name":"uploads"}]"#, 1),
            ("uploads,exports,goog-reserved", 3),
        ] {
            let error = document(value).unwrap_err();
            assert_eq!(error, ShorthandError::InvalidName { position }, "{value}");
            let fix = error.to_string();
            assert!(fix.contains(&format!("name {position} ")), "{value}: {fix}");
            assert!(fix.contains("uploads,exports"), "{fix}");
            assert!(fix.contains(r#"{"version":1"#), "{fix}");
            assert!(!fix.contains("goog-reserved"), "{fix}");
        }
    }

    #[test]
    fn a_repeated_name_is_refused() {
        let error = document("uploads,exports,uploads").unwrap_err();
        assert_eq!(error, ShorthandError::RepeatedName { position: 3 });
        assert!(error.to_string().contains("bucket 3 more than once"));
    }
}
