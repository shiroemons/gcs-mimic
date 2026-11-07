//! Rewrites the pins of container images rebuilt under an unchanged version tag, so the daily
//! workflow can propose them as a pull request. A rebuild changes no version, which makes it the
//! one update that needs no migration decision; the proposal's checks decide whether it is safe.
//! A newer version behind a moving tag stays a deliberate manual adoption, so `check` keeps
//! reporting it.
use super::{Result, fetch, json, registry_manifest_source, registry_response, text};
use serde_json::Value;
use std::{path::Path, process::Command};

const LEDGER: &str = "compatibility/dependency-status.json";
const EVIDENCE: &str = "docs/en/evidence/dependency-refresh.md";
/// The Debian snapshot pinned by the apt install scripts is the one this image was built from.
const SNAPSHOT_IMAGE: &str = "library/rust";
const SOURCES_FILE: &str = "/etc/apt/sources.list.d/debian.sources";
const SNAPSHOT_URL: &str = "http://snapshot.debian.org/archive/debian/";
/// These files record what a past run actually used, so a new digest must not rewrite them.
const HISTORY_EXCLUDES: [&str; 4] = [
    ":(exclude,glob)docs/*/evidence/**",
    ":(exclude,glob)docs/*/slices/**",
    ":(exclude,glob)docs/*/releases/**",
    ":(exclude,glob)**/verification.md",
];

#[derive(Debug, PartialEq)]
struct ImageUpdate {
    repository: String,
    reference: String,
    previous: String,
    proposed: String,
    declares_platforms: bool,
    changed_platforms: Vec<String>,
    /// Every digest to swap: the index and each declared platform manifest that changed.
    replacements: Vec<(String, String)>,
}

#[derive(Debug, PartialEq)]
struct SnapshotUpdate {
    package: String,
    script: String,
    previous: String,
    proposed: String,
}

/// Compares a `0` in `shape` with any ASCII digit and every other byte literally.
fn has_shape(value: &str, shape: &str) -> bool {
    value.len() == shape.len()
        && value.bytes().zip(shape.bytes()).all(|(byte, expected)| {
            if expected == b'0' {
                byte.is_ascii_digit()
            } else {
                byte == expected
            }
        })
}

fn plan_image(image: &Value, proposed: &str, index: &Value) -> Result<Option<ImageUpdate>> {
    let previous = text(image, "digest")?;
    if previous == proposed {
        return Ok(None);
    }
    let reference = text(image, "reference")?;
    let declared = image["platforms"]
        .as_object()
        .ok_or("missing platform pins")?;
    let mut update = ImageUpdate {
        repository: text(image, "repository")?.to_owned(),
        reference: reference.to_owned(),
        previous: previous.to_owned(),
        proposed: proposed.to_owned(),
        declares_platforms: !declared.is_empty(),
        changed_platforms: Vec::new(),
        replacements: vec![(previous.to_owned(), proposed.to_owned())],
    };
    for (platform, pinned) in declared {
        let pinned = pinned.as_str().ok_or("invalid platform pin")?;
        let (os, arch) = platform.split_once('/').ok_or("invalid platform")?;
        let found = index["manifests"]
            .as_array()
            .ok_or("missing image index")?
            .iter()
            .find(|m| m["platform"]["os"] == os && m["platform"]["architecture"] == arch)
            .ok_or_else(|| format!("{reference}: the new index lacks {platform}"))?;
        let digest = text(found, "digest")?;
        if digest != pinned {
            update.changed_platforms.push(platform.clone());
            update
                .replacements
                .push((pinned.to_owned(), digest.to_owned()));
        }
    }
    Ok(Some(update))
}

fn image_snapshot(sources: &str) -> Result<String> {
    let snapshot = sources
        .lines()
        .find_map(|line| line.strip_prefix("# ")?.strip_prefix(SNAPSHOT_URL))
        .ok_or_else(|| format!("{SOURCES_FILE} names no {SNAPSHOT_URL} archive"))?;
    if !has_shape(snapshot, "00000000T000000Z") {
        return Err(format!("invalid Debian snapshot timestamp: {snapshot}"));
    }
    Ok(snapshot.to_owned())
}

/// Reads the snapshot from the image itself, because registry metadata does not expose it.
fn read_image_snapshot(reference: &str, digest: &str) -> Result<String> {
    let pin = format!("{reference}@{digest}");
    let output = Command::new("docker")
        .args(["run", "--rm", &pin, "cat", SOURCES_FILE])
        .output()
        .map_err(|e| format!("cannot run docker: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "docker could not read {SOURCES_FILE} from {pin}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    image_snapshot(&String::from_utf8_lossy(&output.stdout))
}

fn plan_snapshots(status: &Value, proposed: &str) -> Result<Vec<SnapshotUpdate>> {
    let mut updates = Vec::new();
    for package in status["docker"]["apt_packages"]
        .as_array()
        .ok_or("missing apt_packages")?
    {
        let previous = text(package, "snapshot")?;
        if previous != proposed {
            updates.push(SnapshotUpdate {
                package: text(package, "name")?.to_owned(),
                script: text(package, "script")?.to_owned(),
                previous: previous.to_owned(),
                proposed: proposed.to_owned(),
            });
        }
    }
    Ok(updates)
}

fn read(root: &Path, path: &str) -> Result<String> {
    std::fs::read_to_string(root.join(path)).map_err(|e| format!("{path}: {e}"))
}
fn write(root: &Path, path: &str, source: &str) -> Result<()> {
    std::fs::write(root.join(path), source).map_err(|e| format!("{path}: {e}"))
}

fn tracked_files_containing(root: &Path, needle: &str) -> Result<Vec<String>> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["grep", "--files-with-matches", "--fixed-strings", needle])
        .args(["--", "."])
        .args(HISTORY_EXCLUDES)
        .output()
        .map_err(|e| format!("cannot run git grep: {e}"))?;
    // git grep exits 1 when nothing matches, which is the answer rather than a failure.
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(format!(
            "git grep failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}

fn apply_image(root: &Path, update: &ImageUpdate) -> Result<()> {
    for (previous, proposed) in &update.replacements {
        for path in tracked_files_containing(root, previous)? {
            let source = read(root, &path)?;
            write(root, &path, &source.replace(previous, proposed))?;
        }
    }
    Ok(())
}

fn apply_snapshot(root: &Path, update: &SnapshotUpdate) -> Result<()> {
    let assignment = format!("snapshot={}\n", update.previous);
    let script = read(root, &update.script)?;
    if !script.contains(&assignment) {
        return Err(format!("{} does not assign {assignment}", update.script));
    }
    let script = script.replace(&assignment, &format!("snapshot={}\n", update.proposed));
    write(root, &update.script, &script)?;
    // Only the structured field moves: the ledger's prose keeps naming past snapshots. The text
    // edit keeps the ledger's hand-kept layout, so reading it back catches a spelling it missed.
    let ledger = read(root, LEDGER)?.replace(
        &format!("\"snapshot\": \"{}\"", update.previous),
        &format!("\"snapshot\": \"{}\"", update.proposed),
    );
    let moved = json(&ledger)?["docker"]["apt_packages"]
        .as_array()
        .ok_or("missing apt_packages")?
        .iter()
        .any(|p| {
            p["script"] == update.script.as_str() && p["snapshot"] == update.proposed.as_str()
        });
    if !moved {
        return Err(format!(
            "{LEDGER} still records another snapshot for {}",
            update.package
        ));
    }
    write(root, LEDGER, &ledger)
}

fn render_section(
    checked_at: &str,
    images: &[ImageUpdate],
    snapshots: &[SnapshotUpdate],
) -> String {
    let mut section = format!(
        "## Image digest refresh: {checked_at}\n\n\
         `cargo xtask dependencies update-images` found these images rebuilt under an unchanged \
         version tag. The checks of the pull request that adopts them are the validation; this \
         record does not establish a result by itself.\n\n\
         | Input | Previous | Proposed | Changed manifests |\n\
         | --- | --- | --- | --- |\n"
    );
    for image in images {
        let changed = match (image.declares_platforms, image.changed_platforms.is_empty()) {
            (false, _) => "No platforms declared".to_owned(),
            (true, true) => "None on the declared platforms".to_owned(),
            (true, false) => image.changed_platforms.join(", "),
        };
        section.push_str(&format!(
            "| {} | `{}` | `{}` | {changed} |\n",
            image.reference, image.previous, image.proposed
        ));
    }
    for snapshot in snapshots {
        section.push_str(&format!(
            "| Debian snapshot for {} | `{}` | `{}` | Read from the new image |\n",
            snapshot.package, snapshot.previous, snapshot.proposed
        ));
    }
    section
}

/// Applies every rebuilt image's pins and prints the Markdown record to stdout, which stays empty
/// when nothing changed. All registry reads finish before the first write, so a failed lookup
/// leaves the checkout untouched.
pub fn update_images(root: &Path, checked_at: &str) -> Result<()> {
    if !has_shape(checked_at, "0000-00-00T00:00:00Z") {
        return Err(format!("invalid UTC timestamp: {checked_at}"));
    }
    let status = json(&read(root, LEDGER)?)?;
    let mut images = Vec::new();
    for image in status["docker"]["digests"]
        .as_array()
        .ok_or("missing Docker inputs")?
    {
        let (source, headers) = registry_manifest_source(image, |url| fetch(url, &[], false))?;
        let tag = text(image, "tag")?;
        let (proposed, index) =
            registry_response(&fetch(&format!("{source}{tag}"), &headers, true)?)?;
        images.extend(plan_image(image, &proposed, &index)?);
    }
    let mut snapshots = Vec::new();
    if let Some(image) = images.iter().find(|i| i.repository == SNAPSHOT_IMAGE) {
        let snapshot = read_image_snapshot(&image.reference, &image.proposed)?;
        snapshots = plan_snapshots(&status, &snapshot)?;
    }
    if images.is_empty() {
        return Ok(());
    }
    for image in &images {
        apply_image(root, image)?;
    }
    for snapshot in &snapshots {
        apply_snapshot(root, snapshot)?;
    }
    let section = render_section(checked_at, &images, &snapshots);
    let evidence = read(root, EVIDENCE)?;
    write(root, EVIDENCE, &format!("{evidence}\n{section}"))?;
    print!("{section}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json as value;

    const PREVIOUS_INDEX: &str = "sha256:1111";
    const PROPOSED_INDEX: &str = "sha256:2222";

    fn image(platforms: Value) -> Value {
        value!({"repository": "library/rust", "reference": "rust:1-slim", "digest": PREVIOUS_INDEX, "platforms": platforms})
    }
    fn index() -> Value {
        value!({"manifests": [
            {"digest": "sha256:aaaa", "platform": {"os": "linux", "architecture": "amd64"}},
            {"digest": "sha256:bbbb", "platform": {"os": "linux", "architecture": "arm64", "variant": "v8"}},
        ]})
    }

    #[test]
    fn an_unchanged_index_plans_nothing() {
        let image = image(value!({"linux/amd64": "sha256:other"}));
        assert_eq!(plan_image(&image, PREVIOUS_INDEX, &index()), Ok(None));
    }

    #[test]
    fn a_rebuilt_index_replaces_only_the_platforms_that_changed() {
        let image = image(value!({"linux/amd64": "sha256:aaaa", "linux/arm64": "sha256:old"}));
        let update = plan_image(&image, PROPOSED_INDEX, &index())
            .unwrap()
            .unwrap();
        assert_eq!(update.changed_platforms, ["linux/arm64"]);
        assert_eq!(
            update.replacements,
            [
                (PREVIOUS_INDEX.to_owned(), PROPOSED_INDEX.to_owned()),
                ("sha256:old".to_owned(), "sha256:bbbb".to_owned()),
            ]
        );
    }

    #[test]
    fn a_rebuilt_index_without_a_declared_platform_fails() {
        let image = image(value!({"linux/riscv64": "sha256:aaaa"}));
        let error = plan_image(&image, PROPOSED_INDEX, &index()).unwrap_err();
        assert!(error.contains("linux/riscv64"), "{error}");
    }

    #[test]
    fn the_snapshot_comes_from_the_debian_archive_comment() {
        let sources = "Types: deb\n# http://snapshot.debian.org/archive/debian/20261104T212339Z\n\
                       URIs: http://deb.debian.org/debian\n\
                       # http://snapshot.debian.org/archive/debian-security/20261105T000000Z\n";
        assert_eq!(image_snapshot(sources).unwrap(), "20261104T212339Z");
        assert!(image_snapshot("URIs: http://deb.debian.org/debian\n").is_err());
        assert!(image_snapshot("# http://snapshot.debian.org/archive/debian/latest\n").is_err());
    }

    #[test]
    fn only_packages_on_another_snapshot_are_planned() {
        let status = value!({"docker": {"apt_packages": [
            {"name": "socat", "script": "socat.sh", "snapshot": "old"},
            {"name": "musl", "script": "musl.sh", "snapshot": "new"},
        ]}});
        let updates = plan_snapshots(&status, "new").unwrap();
        assert_eq!(
            updates,
            [SnapshotUpdate {
                package: "socat".to_owned(),
                script: "socat.sh".to_owned(),
                previous: "old".to_owned(),
                proposed: "new".to_owned(),
            }]
        );
    }

    #[test]
    fn the_record_names_what_changed_on_each_image() {
        let update = |declares_platforms, changed_platforms: &[&str]| ImageUpdate {
            repository: "r".to_owned(),
            reference: "r:1".to_owned(),
            previous: "sha256:1".to_owned(),
            proposed: "sha256:2".to_owned(),
            declares_platforms,
            changed_platforms: changed_platforms.iter().map(|p| (*p).to_owned()).collect(),
            replacements: Vec::new(),
        };
        let section = render_section(
            "2026-11-07T03:17:00Z",
            &[
                update(true, &["linux/amd64", "linux/arm64"]),
                update(true, &[]),
                update(false, &[]),
            ],
            &[],
        );
        assert!(section.starts_with("## Image digest refresh: 2026-11-07T03:17:00Z\n"));
        assert!(section.contains("| `sha256:2` | linux/amd64, linux/arm64 |\n"));
        assert!(section.contains("| None on the declared platforms |\n"));
        assert!(section.contains("| No platforms declared |\n"));
    }

    /// A git checkout under the temporary directory, removed when the test ends.
    struct Checkout(std::path::PathBuf);
    impl Checkout {
        fn new(name: &str, files: &[(&str, &str)]) -> Self {
            let root =
                std::env::temp_dir().join(format!("xtask-images-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            for (path, source) in files {
                let path = root.join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, source).unwrap();
            }
            for args in [&["init", "-q"][..], &["add", "."][..]] {
                let done = Command::new("git").current_dir(&root).args(args).status();
                assert!(done.unwrap().success());
            }
            Self(root)
        }
        fn read(&self, path: &str) -> String {
            std::fs::read_to_string(self.0.join(path)).unwrap()
        }
    }
    impl Drop for Checkout {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn digests_move_in_live_pins_but_not_in_history() {
        let pin = "rust:1-slim@sha256:1111";
        let live = [
            "Dockerfile",
            "sdk-tests/php/README.md",
            "compatibility/dependency-status.json",
        ];
        let history = [
            "docs/en/evidence/dependency-refresh.md",
            "docs/ja/evidence/run.txt",
            "docs/en/slices/image.md",
            "docs/en/releases/1.0.0.md",
            "sdk-tests/go/verification.md",
        ];
        let files: Vec<(&str, &str)> = live.iter().chain(&history).map(|p| (*p, pin)).collect();
        let checkout = Checkout::new("history", &files);
        let update = ImageUpdate {
            repository: "library/rust".to_owned(),
            reference: "rust:1-slim".to_owned(),
            previous: PREVIOUS_INDEX.to_owned(),
            proposed: PROPOSED_INDEX.to_owned(),
            declares_platforms: false,
            changed_platforms: Vec::new(),
            replacements: vec![(PREVIOUS_INDEX.to_owned(), PROPOSED_INDEX.to_owned())],
        };
        apply_image(&checkout.0, &update).unwrap();
        for path in live {
            assert_eq!(checkout.read(path), "rust:1-slim@sha256:2222", "{path}");
        }
        for path in history {
            assert_eq!(checkout.read(path), pin, "{path}");
        }
    }

    #[test]
    fn a_snapshot_moves_the_script_and_ledger_field_but_not_ledger_prose() {
        let ledger = r#"{"prose":"moved to 2026A","docker":{"apt_packages":[{"script":"install.sh", "snapshot": "2026A"}]}}"#;
        let checkout = Checkout::new(
            "snapshot",
            &[
                (LEDGER, ledger),
                ("install.sh", "snapshot=2026A\nversion=1\n"),
            ],
        );
        let update = SnapshotUpdate {
            package: "socat".to_owned(),
            script: "install.sh".to_owned(),
            previous: "2026A".to_owned(),
            proposed: "2026B".to_owned(),
        };
        apply_snapshot(&checkout.0, &update).unwrap();
        assert_eq!(checkout.read("install.sh"), "snapshot=2026B\nversion=1\n");
        assert_eq!(
            checkout.read(LEDGER),
            r#"{"prose":"moved to 2026A","docker":{"apt_packages":[{"script":"install.sh", "snapshot": "2026B"}]}}"#
        );
    }

    #[test]
    fn a_ledger_field_the_rewrite_cannot_find_fails_instead_of_passing_unchanged() {
        let ledger = r#"{"docker":{"apt_packages":[{"script":"install.sh","snapshot":"2026A"}]}}"#;
        let checkout = Checkout::new(
            "spelling",
            &[(LEDGER, ledger), ("install.sh", "snapshot=2026A\n")],
        );
        let update = SnapshotUpdate {
            package: "socat".to_owned(),
            script: "install.sh".to_owned(),
            previous: "2026A".to_owned(),
            proposed: "2026B".to_owned(),
        };
        let error = apply_snapshot(&checkout.0, &update).unwrap_err();
        assert!(error.contains("socat"), "{error}");
        assert_eq!(checkout.read(LEDGER), ledger);
    }

    #[test]
    fn the_committed_ledger_spells_the_snapshot_field_as_the_rewrite_expects() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let ledger = read(root, LEDGER).unwrap();
        let status = json(&ledger).unwrap();
        for package in status["docker"]["apt_packages"].as_array().unwrap() {
            let field = format!("\"snapshot\": \"{}\"", text(package, "snapshot").unwrap());
            assert!(ledger.contains(&field), "{field}");
        }
    }

    #[test]
    fn a_script_without_the_recorded_snapshot_fails() {
        let checkout = Checkout::new(
            "script",
            &[(LEDGER, "{}"), ("install.sh", "snapshot=other\n")],
        );
        let update = SnapshotUpdate {
            package: "socat".to_owned(),
            script: "install.sh".to_owned(),
            previous: "2026A".to_owned(),
            proposed: "2026B".to_owned(),
        };
        assert!(apply_snapshot(&checkout.0, &update).is_err());
        assert_eq!(checkout.read("install.sh"), "snapshot=other\n");
    }

    #[test]
    fn a_malformed_timestamp_is_rejected_before_any_lookup() {
        let error = update_images(Path::new("/nonexistent"), "2026-11-07").unwrap_err();
        assert!(error.contains("invalid UTC timestamp"), "{error}");
    }
}
