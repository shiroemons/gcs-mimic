//! Ties the ledger's test IDs to the code. `evidence` only compares the ledger with itself, so a
//! test renamed or merged without a ledger update leaves evidence that points at nothing.

use crate::{Result, array, ensure, strings};
use serde_json::Value;
use std::{collections::BTreeSet, fs, path::Path};

/// Where tests live, and the scripts and workflows that run them.
const SOURCE_DIRS: [&str; 8] = [
    "src",
    "tests",
    "xtask",
    "sdk-tests",
    "go",
    "scripts",
    "examples",
    ".github",
];

/// Build output and installed dependencies, whose words say nothing about this repository's tests.
const SKIPPED_DIRS: [&str; 3] = ["target", "node_modules", "vendor"];

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The test functions an ID names: a plain or `module::path::` qualified name, or `TestName/Subtest`
/// for a Go subtest. Any other ID, such as `guide-native-object-media`, labels a lane that its row's
/// `verification[].command` describes, so there is no name to look up.
fn test_names(id: &str) -> Vec<&str> {
    let leaf = id.rsplit("::").next().unwrap_or(id);
    let names: Vec<&str> = leaf.split('/').collect();
    if names
        .iter()
        .all(|name| !name.is_empty() && name.bytes().all(is_word))
    {
        names
    } else {
        Vec::new()
    }
}

fn strike_out_words(path: &Path, missing: &mut BTreeSet<&str>) -> Result<()> {
    let describe = |error: std::io::Error| format!("{}: {error}", path.display());
    for entry in fs::read_dir(path).map_err(describe)? {
        let entry = entry.map_err(describe)?;
        let kind = entry.file_type().map_err(describe)?;
        if kind.is_dir() {
            let skipped = entry
                .file_name()
                .to_str()
                .is_some_and(|name| SKIPPED_DIRS.contains(&name));
            if !skipped {
                strike_out_words(&entry.path(), missing)?;
            }
        } else if kind.is_file() {
            let bytes = fs::read(entry.path()).map_err(describe)?;
            for word in bytes.split(|byte| !is_word(*byte)) {
                if let Ok(word) = std::str::from_utf8(word) {
                    missing.remove(word);
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn check(root: &Path, ledger: &Value) -> Result<()> {
    let mut missing = BTreeSet::new();
    for row in array(ledger, "features")? {
        for id in strings(row, "test_ids")? {
            missing.extend(test_names(id));
        }
    }
    for dir in SOURCE_DIRS {
        strike_out_words(&root.join(dir), &mut missing)?;
    }
    ensure(
        missing.is_empty(),
        &format!(
            "the ledger names tests that no longer exist; name their replacements: {}",
            missing.into_iter().collect::<Vec<_>>().join(", ")
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn repository() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap()
    }

    #[test]
    fn ids_name_functions_and_labels_name_nothing() {
        assert_eq!(test_names("a_test"), ["a_test"]);
        assert_eq!(test_names("module::tests::a_test"), ["a_test"]);
        assert_eq!(test_names("TestSuite/Subtest"), ["TestSuite", "Subtest"]);
        assert!(test_names("guide-native-object-media").is_empty());
        assert!(test_names("https.rb (GCS_MIMIC_SIGNED_TESTS=1)").is_empty());
    }

    #[test]
    fn every_ledger_test_exists() {
        let ledger = crate::read_json(&repository().join("compatibility/features.json")).unwrap();
        check(repository(), &ledger).unwrap();
    }

    #[test]
    fn a_test_missing_from_the_code_is_named_and_a_lane_label_is_not() {
        // Built at run time: this file is scanned too, so a literal would count as the test.
        let gone = ["gone", "after", "a", "rename"].join("_");
        let qualified = format!("module::tests::{gone}_too");
        let ledger = json!({"features": [{"test_ids": [
            gone,
            qualified,
            "a_test_missing_from_the_code_is_named_and_a_lane_label_is_not",
            "some-lane-label",
        ]}]});
        let error = check(repository(), &ledger).unwrap_err();
        assert!(error.ends_with(&format!("{gone}, {gone}_too")), "{error}");
    }
}
