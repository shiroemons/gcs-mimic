use super::*;
#[cfg(unix)]
use std::process::Command;
use std::{fs, path::Path};

#[test]
fn release_version_grammar_matches_shell_and_workflows() {
    let cases: Vec<(String, bool)> = serde_json::from_str(include_str!(
        "../../../../scripts/release-version-cases.json"
    ))
    .unwrap();
    for (value, valid) in cases {
        let parsed = value.parse::<Version>();
        assert_eq!(parsed.is_ok(), valid, "{value:?}");
        if let Ok(version) = parsed {
            assert_eq!(version.to_string(), value);
        }
    }
    #[cfg(unix)]
    {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let output = Command::new("bash")
            .arg(root.join("scripts/test-release-workflows.sh"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn invalid_release_versions_identify_the_complete_rejected_input() {
    for value in ["01.2.3", "1.2.3-rc.0", "1.2.3-rc.1+build"] {
        let error = value.parse::<Version>().unwrap_err();
        assert_eq!(error.0, value);
        assert!(error.to_string().contains(value));
    }
}

#[test]
fn candidates_order_numerically_before_the_final_release() {
    let versions = [
        "0.17.1",
        "1.0.0-rc.1",
        "1.0.0-rc.2",
        "1.0.0-rc.10",
        "1.0.0",
        "1.0.1-rc.1",
        "1.0.1",
    ]
    .map(|value| value.parse::<Version>().unwrap());
    assert!(versions.windows(2).all(|pair| pair[0] < pair[1]));
}

fn missing_fixtures(root: &Path, current: &Version) -> Vec<String> {
    let first: Version = "0.8.0".parse().unwrap();
    let mut missing = Vec::new();
    for entry in fs::read_dir(root.join("docs/en/releases")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|extension| extension != "md") {
            continue;
        }
        let name = path.file_stem().unwrap().to_str().unwrap();
        if let Ok(version) = name.parse::<Version>()
            && first <= version
            && &version < current
            && !root
                .join("tests/fixtures/released-data")
                .join(name)
                .is_dir()
        {
            missing.push(name.to_owned());
        }
    }
    missing.sort();
    missing
}

#[test]
fn each_candidate_requires_its_own_upgrade_fixture_before_the_final_release() {
    let directory = crate::release::tests::Directory::new();
    let root = &directory.0;
    fs::create_dir_all(root.join("docs/en/releases")).unwrap();
    for version in ["0.7.0", "1.0.0-rc.1", "1.0.0-rc.2", "1.0.0"] {
        fs::write(
            root.join("docs/en/releases").join(format!("{version}.md")),
            "notes",
        )
        .unwrap();
    }
    assert!(missing_fixtures(root, &"1.0.0-rc.1".parse().unwrap()).is_empty());
    assert_eq!(
        missing_fixtures(root, &"1.0.0-rc.2".parse().unwrap()),
        ["1.0.0-rc.1"]
    );
    let final_version = "1.0.0".parse().unwrap();
    assert_eq!(
        missing_fixtures(root, &final_version),
        ["1.0.0-rc.1", "1.0.0-rc.2"]
    );
    fs::create_dir_all(root.join("tests/fixtures/released-data/1.0.0-rc.1")).unwrap();
    assert_eq!(missing_fixtures(root, &final_version), ["1.0.0-rc.2"]);
    fs::create_dir_all(root.join("tests/fixtures/released-data/1.0.0-rc.2")).unwrap();
    assert!(missing_fixtures(root, &final_version).is_empty());
}

/// A fixture can only be captured after publication, so require it before the next version.
#[test]
fn every_published_release_is_supported_before_the_next_version() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let manifest = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let current: Version = manifest
        .lines()
        .skip_while(|line| *line != "[package]")
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .find_map(|line| line.strip_prefix("version = \"")?.strip_suffix('"'))
        .unwrap()
        .parse()
        .unwrap();
    let missing = missing_fixtures(root, &current);
    assert!(
        missing.is_empty(),
        "add released-data fixtures through scripts/released-data-fixtures.sh: {missing:?}"
    );
}
