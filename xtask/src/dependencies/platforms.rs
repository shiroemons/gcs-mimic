//! Offline cross-check between the architectures the project ships and the `platforms` scope of
//! each release-input Docker image in the dependency ledger. The daily freshness comparison only
//! looks at declared platforms, so a shipped architecture missing from the ledger would never be
//! checked for updates.

use super::{Result, release_input, text};
use serde_json::Value;
use std::{collections::BTreeSet, path::Path};

type Platforms = BTreeSet<String>;

/// Release container platforms are defined by the GHCR publication matrix. Every other place
/// that names the shipped architectures must agree with it.
const AUTHORITY: &str = ".github/workflows/ghcr.yaml";

/// One repository file that names the shipped architectures, and which of its lines count.
struct Source {
    path: &'static str,
    selects: fn(&str) -> bool,
}

const SOURCES: &[Source] = &[
    Source {
        path: AUTHORITY,
        selects: |line| {
            line.trim_start_matches("- ")
                .starts_with("platform: linux/")
        },
    },
    Source {
        path: ".github/workflows/ci.yaml",
        selects: |line| {
            line.trim_start_matches("- ")
                .starts_with("platform: linux/")
        },
    },
    Source {
        path: "Justfile",
        selects: |line| line.starts_with("check-containers:"),
    },
    Source {
        path: "scripts/smoke-containers.sh",
        // The default platform list and the accepted-platform case pattern.
        selects: |line| line.contains("set -- linux/") || line.trim_start().starts_with("linux/"),
    },
    Source {
        path: "scripts/container-mirror.sh",
        selects: |line| line.contains("keys == [\"linux/"),
    },
];

/// Every `linux/<arch>[/<variant>]` token on a line.
fn line_platforms(line: &str) -> Platforms {
    line.match_indices("linux/")
        .map(|(start, _)| {
            line[start..]
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '/'))
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// The platform set of each selected line of a source. Each line must stand for the full
/// shipped set on its own, except matrix entries, which each name one platform and are merged.
fn source_platforms(source: &Source, contents: &str) -> Result<Platforms> {
    let matrix = source.path.ends_with(".yaml");
    let mut merged = Platforms::new();
    let mut lines = 0;
    for line in contents
        .lines()
        .map(str::trim)
        .filter(|l| (source.selects)(l))
    {
        let found = line_platforms(line);
        if found.is_empty() {
            continue;
        }
        lines += 1;
        if matrix {
            merged.extend(found);
        } else if lines == 1 {
            merged = found;
        } else if found != merged {
            return Err(format!(
                "{}: lines disagree about the shipped platforms ({}) vs ({})",
                source.path,
                join(&merged),
                join(&found)
            ));
        }
    }
    if lines == 0 {
        return Err(format!("{}: no shipped platforms found", source.path));
    }
    Ok(merged)
}

fn join(set: &Platforms) -> String {
    set.iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

fn difference(name: &str, expected: &Platforms, actual: &Platforms) -> Result<()> {
    let missing: Vec<_> = expected.difference(actual).cloned().collect();
    let extra: Vec<_> = actual.difference(expected).cloned().collect();
    if missing.is_empty() && extra.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{name}: missing [{}], extra [{}] compared with the shipped platforms [{}]",
        missing.join(", "),
        extra.join(", "),
        join(expected)
    ))
}

/// The first source is authoritative; every other source must name the same set.
fn shipped_platforms(sources: &[(&str, Platforms)]) -> Result<Platforms> {
    let (_, shipped) = sources.first().ok_or("no platform sources")?;
    if shipped.is_empty() {
        return Err("the authoritative platform source is empty".into());
    }
    for (name, found) in &sources[1..] {
        difference(name, shipped, found)?;
    }
    Ok(shipped.clone())
}

/// A release-input image must declare exactly the shipped platforms. Empty `platforms` is
/// allowed only when the whole index is compared (`tag == latest_tag`); development-only
/// images are out of scope.
fn check_ledger(images: &[Value], shipped: &Platforms) -> Result<()> {
    for image in images {
        if !release_input(image)? {
            continue;
        }
        let reference = text(image, "reference")?;
        let declared: Platforms = image["platforms"]
            .as_object()
            .ok_or_else(|| format!("{reference}: missing platforms map"))?
            .keys()
            .cloned()
            .collect();
        if declared.is_empty() {
            if text(image, "tag")? != text(image, "latest_tag")? {
                return Err(format!(
                    "{reference}: empty platforms need tag == latest_tag so the whole index is compared"
                ));
            }
            continue;
        }
        difference(&format!("{reference} platforms"), shipped, &declared)?;
    }
    Ok(())
}

pub(super) fn check(root: &Path, status: &Value) -> Result<()> {
    let mut found = Vec::new();
    for source in SOURCES {
        let contents = std::fs::read_to_string(root.join(source.path))
            .map_err(|e| format!("{}: {e}", source.path))?;
        found.push((source.path, source_platforms(source, &contents)?));
    }
    let shipped = shipped_platforms(&found)?;
    let images = status["docker"]["digests"]
        .as_array()
        .ok_or("missing Docker inputs")?;
    check_ledger(images, &shipped)?;
    println!("release platforms: {} match the ledger", join(&shipped));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set(items: &[&str]) -> Platforms {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn image(release: bool, tag: &str, latest: &str, platforms: &[&str]) -> Value {
        let map: serde_json::Map<_, _> = platforms
            .iter()
            .map(|p| ((*p).to_owned(), json!("sha256:a")))
            .collect();
        json!({"reference": "x/y:t", "release_input": release, "tag": tag, "latest_tag": latest, "platforms": map})
    }

    const BOTH: &[&str] = &["linux/amd64", "linux/arm64"];

    #[test]
    fn matching_release_image_passes() {
        check_ledger(&[image(true, "1", "latest", BOTH)], &set(BOTH)).unwrap();
    }

    #[test]
    fn missing_architecture_is_named() {
        let err =
            check_ledger(&[image(true, "1", "latest", &["linux/amd64"])], &set(BOTH)).unwrap_err();
        assert!(
            err.contains("x/y:t") && err.contains("missing [linux/arm64]"),
            "{err}"
        );
    }

    #[test]
    fn extra_architecture_is_named() {
        let three = &["linux/amd64", "linux/arm64", "linux/riscv64"];
        let err = check_ledger(&[image(true, "1", "latest", three)], &set(BOTH)).unwrap_err();
        assert!(err.contains("extra [linux/riscv64]"), "{err}");
    }

    #[test]
    fn development_only_images_are_out_of_scope() {
        check_ledger(&[image(false, "1", "latest", &["linux/amd64"])], &set(BOTH)).unwrap();
    }

    #[test]
    fn empty_platforms_need_a_whole_index_comparison() {
        check_ledger(&[image(true, "1", "1", &[])], &set(BOTH)).unwrap();
        assert!(check_ledger(&[image(true, "1", "latest", &[])], &set(BOTH)).is_err());
    }

    #[test]
    fn other_sources_must_match_the_authority() {
        let ok = [("a", set(BOTH)), ("b", set(BOTH))];
        assert_eq!(shipped_platforms(&ok).unwrap(), set(BOTH));
        let bad = [("a", set(BOTH)), ("Justfile", set(&["linux/amd64"]))];
        let err = shipped_platforms(&bad).unwrap_err();
        assert!(
            err.contains("Justfile") && err.contains("missing [linux/arm64]"),
            "{err}"
        );
        assert!(shipped_platforms(&[("a", Platforms::new())]).is_err());
    }

    #[test]
    fn matrix_entries_merge_and_script_lines_must_agree() {
        let yaml = Source {
            path: "w.yaml",
            selects: SOURCES[0].selects,
        };
        let text = "include:\n  - runner: r\n    platform: linux/amd64\n  - platform: linux/arm64\n    architecture: arm64\n  platform:\n";
        assert_eq!(source_platforms(&yaml, text).unwrap(), set(BOTH));
        let script = Source {
            path: "s.sh",
            selects: |l| l.contains("keys =="),
        };
        let agree = "keys == [\"linux/amd64\", \"linux/arm64\"]\nkeys == [\"linux/amd64\", \"linux/arm64\"]";
        assert_eq!(source_platforms(&script, agree).unwrap(), set(BOTH));
        let split = "keys == [\"linux/amd64\", \"linux/arm64\"]\nkeys == [\"linux/amd64\"]";
        assert!(source_platforms(&script, split).is_err());
        assert!(source_platforms(&script, "nothing").is_err());
    }

    #[test]
    fn variants_stay_part_of_the_platform() {
        assert_eq!(
            line_platforms("linux/arm/v7|linux/amd64)"),
            set(&["linux/arm/v7", "linux/amd64"])
        );
    }
}
