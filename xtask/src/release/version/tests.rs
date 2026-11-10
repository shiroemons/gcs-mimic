use super::*;
use std::{fs, path::Path, process::Command};

#[test]
fn release_version_grammar_matches_shell_and_workflows() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for (value, valid) in [
        ("0.0.0", true),
        ("0.17.1", true),
        ("1.0.0-rc.1", true),
        ("1.0.0-rc.2", true),
        ("12.34.56-rc.123", true),
        ("184467440737095516160.0.0-rc.184467440737095516160", true),
        ("", false),
        ("01.2.3", false),
        ("1.02.3", false),
        ("1.2.03", false),
        ("1.2", false),
        ("1.2.3.4", false),
        ("v1.2.3", false),
        ("1.2.3-alpha.1", false),
        ("1.2.3-beta.1", false),
        ("1.2.3-rc", false),
        ("1.2.3-rc.0", false),
        ("1.2.3-rc.01", false),
        ("1.2.3-rc.1.2", false),
        ("1.2.3-rc.1-rc.2", false),
        ("1.2.3-RC.1", false),
        ("1.2.3+build", false),
        ("1.2.3-rc.1+build", false),
        ("1.2.3\n", false),
        ("1.2.3-rc.1\n", false),
        (" 1.2.3", false),
        ("1.2.3 ", false),
        ("1.2.+3", false),
        ("１.2.3", false),
    ] {
        let parsed = value.parse::<Version>();
        assert_eq!(parsed.is_ok(), valid, "{value:?}");
        if let Ok(version) = parsed {
            assert_eq!(version.to_string(), value);
        }
        let output = Command::new("bash")
            .arg(root.join("scripts/release-version.sh"))
            .arg(value)
            .output()
            .unwrap();
        assert_eq!(output.status.success(), valid, "shell: {value:?}");
    }
    for path in [
        ".github/workflows/binaries.yaml",
        ".github/workflows/ghcr.yaml",
    ] {
        let workflow = fs::read_to_string(root.join(path)).unwrap();
        assert!(
            workflow.contains("bash scripts/release-version.sh \"$"),
            "{path}"
        );
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
