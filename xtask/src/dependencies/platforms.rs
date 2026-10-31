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

/// How a source names the shipped set.
enum Kind {
    /// A workflow matrix: each `platform:` entry names one platform and the entries are merged.
    /// Fails closed: any other non-comment line mentioning `linux/` is an error, so a spelling
    /// the line selector does not understand cannot hide a platform.
    Matrix,
    /// Selected lines each name the full set and must agree.
    Lines,
}

/// One repository file that names the shipped architectures, and which of its lines count.
struct Source {
    path: &'static str,
    kind: Kind,
    /// Restricts a workflow to one job: from this key until the next job key.
    job: Option<&'static str>,
    selects: fn(&str) -> bool,
}

const SOURCES: &[Source] = &[
    Source {
        path: AUTHORITY,
        kind: Kind::Matrix,
        job: None,
        selects: |line| platform_value(line).is_some(),
    },
    Source {
        path: ".github/workflows/ci.yaml",
        kind: Kind::Matrix,
        job: Some("containers"),
        selects: |line| platform_value(line).is_some(),
    },
    Source {
        path: "Justfile",
        kind: Kind::Lines,
        job: None,
        selects: |line| line.starts_with("check-containers:"),
    },
    Source {
        path: "scripts/smoke-containers.sh",
        kind: Kind::Lines,
        job: None,
        // The usage text, the default platform list and the accepted-platform case pattern.
        selects: |line| {
            line.contains("Usage:") || line.contains("set -- linux/") || line.starts_with("linux/")
        },
    },
    Source {
        path: "scripts/container-mirror.sh",
        kind: Kind::Lines,
        job: None,
        selects: |line| line.contains("keys == [\"linux/"),
    },
];

/// The value of a `platform:` key on a line, in block or flow form, quotes removed. Empty values
/// (the job named `platform`) yield `None`.
fn platform_value(line: &str) -> Option<&str> {
    let mut from = 0;
    while let Some(at) = line[from..].find("platform:") {
        let start = from + at;
        from = start + "platform:".len();
        let boundary = line[..start]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || matches!(c, '{' | ',' | '-'));
        if !boundary {
            continue;
        }
        let value = line[from..]
            .split([',', '}'])
            .next()
            .unwrap_or_default()
            .trim()
            .trim_matches(['"', '\'']);
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

/// Drops a trailing comment (`#` at line start or after whitespace) and surrounding whitespace.
fn strip_comment(line: &str) -> &str {
    let cut = line
        .char_indices()
        .find(|&(i, c)| {
            c == '#'
                && line[..i]
                    .chars()
                    .next_back()
                    .is_none_or(char::is_whitespace)
        })
        .map_or(line.len(), |(i, _)| i);
    line[..cut].trim()
}

/// Comment-free, trimmed lines with backslash continuations joined into one logical line.
fn logical_lines(contents: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut pending = String::new();
    for raw in contents.lines() {
        let line = strip_comment(raw);
        if let Some(head) = line.strip_suffix('\\') {
            pending.push_str(head.trim_end());
            pending.push(' ');
        } else {
            pending.push_str(line);
            lines.push(std::mem::take(&mut pending));
        }
    }
    if !pending.is_empty() {
        lines.push(pending);
    }
    lines
}

/// The lines of one job body, found by its two-space-indented key.
fn job_lines<'a>(contents: &'a str, job: &str) -> Result<Vec<&'a str>> {
    let key = format!("  {job}:");
    let mut lines = contents.lines().skip_while(|l| l.trim_end() != key);
    lines.next().ok_or_else(|| format!("job {job} not found"))?;
    Ok(lines
        .take_while(|l| {
            !(l.starts_with("  ") && !l.starts_with("   ") && l.trim_end().ends_with(':'))
        })
        .collect())
}

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

/// The platform set a source names.
fn source_platforms(source: &Source, contents: &str) -> Result<Platforms> {
    let scoped = match source.job {
        Some(job) => job_lines(contents, job)?.join("\n"),
        None => contents.to_owned(),
    };
    let mut merged = Platforms::new();
    let mut count = 0;
    for line in logical_lines(&scoped) {
        let selected = (source.selects)(&line);
        if !selected {
            if matches!(source.kind, Kind::Matrix) && line.contains("linux/") {
                return Err(format!(
                    "{}: unrecognised platform line `{line}`",
                    source.path
                ));
            }
            continue;
        }
        let found = line_platforms(&line);
        if found.is_empty() {
            return Err(format!(
                "{}: selected line names no linux/ platform: `{line}`",
                source.path
            ));
        }
        count += 1;
        match source.kind {
            Kind::Matrix => merged.extend(found),
            Kind::Lines if count == 1 => merged = found,
            Kind::Lines if found != merged => {
                return Err(format!(
                    "{}: lines disagree about the shipped platforms ({}) vs ({})",
                    source.path,
                    join(&merged),
                    join(&found)
                ));
            }
            Kind::Lines => {}
        }
    }
    if count == 0 {
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
            kind: Kind::Matrix,
            job: None,
            selects: SOURCES[0].selects,
        };
        let text = "include:\n  - runner: r\n    platform: linux/amd64\n  - platform: linux/arm64\n    architecture: arm64\n  platform:\n";
        assert_eq!(source_platforms(&yaml, text).unwrap(), set(BOTH));
        let script = Source {
            path: "s.sh",
            kind: Kind::Lines,
            job: None,
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

    fn matrix(text: &str) -> Result<Platforms> {
        source_platforms(&SOURCES[0], text)
    }

    #[test]
    fn matrix_spellings_are_all_understood() {
        assert_eq!(
            matrix("- platform: \"linux/riscv64\"").unwrap(),
            set(&["linux/riscv64"])
        );
        assert_eq!(matrix("platform:  linux/x").unwrap(), set(&["linux/x"]));
        assert_eq!(
            matrix("- {runner: r, platform: linux/x}").unwrap(),
            set(&["linux/x"])
        );
        assert_eq!(
            matrix("  platform:\n    - platform: linux/a # linux/b later").unwrap(),
            set(&["linux/a"])
        );
    }

    #[test]
    fn matrix_fails_closed() {
        assert!(matrix("platform:\n").is_err());
        assert!(matrix("- platform: amd64\n").is_err());
        assert!(matrix("- platform: linux/a\n  other: linux/b\n").is_err());
        assert_eq!(
            matrix("# linux/b\n- platform: linux/a").unwrap(),
            set(&["linux/a"])
        );
    }

    #[test]
    fn continuations_join_and_jobs_are_scoped() {
        assert_eq!(
            logical_lines("a: \\\n  linux/x\nb"),
            vec!["a: linux/x", "b"]
        );
        let ci = "jobs:\n  other:\n    platform: linux/z\n  containers:\n    platform: linux/a\n  next:\n    platform: linux/y\n";
        assert_eq!(
            source_platforms(&SOURCES[1], ci).unwrap(),
            set(&["linux/a"])
        );
    }
}
