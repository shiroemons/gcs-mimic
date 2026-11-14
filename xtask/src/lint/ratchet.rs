use super::{Finding, RATCHETED_CLIPPY_LINTS, RATCHETED_TEXT_RULES, production_file};
use std::{collections::BTreeMap, fmt, fs, path::Path};

pub(super) const FILE: &str = "xtask/lint-baseline.txt";

pub(super) fn eligible(rule: &str) -> bool {
    RATCHETED_CLIPPY_LINTS.contains(&rule) || RATCHETED_TEXT_RULES.contains(&rule)
}

pub(super) fn file(target: Option<&str>) -> String {
    target.map_or_else(
        || FILE.to_owned(),
        |target| format!("xtask/lint-baseline.{target}.txt"),
    )
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Baseline {
    entries: BTreeMap<(String, String), usize>,
    file: String,
}

#[derive(Debug)]
pub(super) enum Problem {
    Exceeded {
        rule: String,
        file: String,
        allowed: usize,
        findings: Vec<Finding>,
    },
    Stale {
        baseline: String,
        rule: String,
        file: String,
        recorded: usize,
        found: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StaleCounts {
    Reject,
    Warn,
}

impl Problem {
    fn fails(&self, stale: StaleCounts) -> bool {
        !matches!((self, stale), (Self::Stale { .. }, StaleCounts::Warn))
    }
}

pub(super) fn report(problems: Vec<Problem>, stale: StaleCounts) -> Vec<Problem> {
    problems.into_iter().filter(|problem| {
        let fails = problem.fails(stale);
        if let Problem::Stale { rule, file, recorded, found, .. } = problem
            && !fails
        {
            eprintln!("warning: {rule}: {file}: compiler found {found}, below baseline {recorded}; keep counts from the pinned lane");
        } else {
            eprintln!("error: {problem}");
        }
        fails
    }).collect()
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exceeded {
                rule,
                file,
                allowed,
                findings,
            } => {
                write!(
                    f,
                    "{rule}: {file}: {} findings exceed baseline {allowed}",
                    findings.len()
                )?;
                for finding in findings {
                    write!(
                        f,
                        "\nerror: {}: {}:{}",
                        finding.rule, finding.file, finding.line
                    )?;
                }
                Ok(())
            }
            Self::Stale {
                baseline,
                rule,
                file,
                recorded,
                found,
            } => {
                write!(
                    f,
                    "{rule}: {file}: stale baseline {recorded}, now {found}; "
                )?;
                if *found == 0 {
                    write!(f, "delete its {baseline} entry")
                } else {
                    write!(f, "lower {baseline} entry to: {rule}\t{file}\t{found}")
                }
            }
        }
    }
}

impl Baseline {
    pub(super) fn load(root: &Path, target: Option<&str>) -> Result<Self, String> {
        let file = file(target);
        let text = match fs::read_to_string(root.join(&file)) {
            Ok(text) => text,
            Err(error) if target.is_some() && error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!(
                    "warning: {file} is missing; all ratcheted findings count as new; establish it with `cargo xtask lint --baseline --target {}`",
                    target.unwrap_or_default()
                );
                String::new()
            }
            Err(error) => return Err(format!("{file}: {error}")),
        };
        Self::parse(&text, &file)
    }

    fn parse(text: &str, baseline_file: &str) -> Result<Self, String> {
        let mut entries = BTreeMap::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }
            let invalid = || {
                format!(
                    "{baseline_file}:{}: expected a unique ratcheted rule, production file and positive count separated by tabs",
                    index + 1
                )
            };
            let mut fields = line.split('\t');
            let rule = fields
                .next()
                .filter(|rule| eligible(rule))
                .ok_or_else(invalid)?;
            let file = fields
                .next()
                .filter(|file| production_file(file) && super::normalize(file) == *file)
                .ok_or_else(invalid)?;
            let count = fields
                .next()
                .and_then(|count| count.parse::<usize>().ok())
                .filter(|count| *count > 0)
                .ok_or_else(invalid)?;
            if fields.next().is_some()
                || entries
                    .insert((rule.to_owned(), file.to_owned()), count)
                    .is_some()
            {
                return Err(invalid());
            }
        }
        Ok(Self {
            entries,
            file: baseline_file.to_owned(),
        })
    }

    pub(super) fn from_findings(findings: &[Finding]) -> Self {
        let mut entries = BTreeMap::new();
        for finding in findings.iter().filter(|finding| eligible(&finding.rule)) {
            *entries
                .entry((finding.rule.clone(), finding.file.clone()))
                .or_default() += 1;
        }
        Self {
            entries,
            file: FILE.to_owned(),
        }
    }

    pub(super) fn render(&self) -> String {
        let mut text = "# rule\tfile\tcount — generated by `cargo xtask lint --baseline [--target TRIPLE]`; only lower counts by hand.\n".to_owned();
        for ((rule, file), count) in &self.entries {
            text.push_str(&format!("{rule}\t{file}\t{count}\n"));
        }
        text
    }

    pub(super) fn check(&self, findings: &[Finding]) -> Vec<Problem> {
        let current = Self::from_findings(findings);
        let mut pairs = self.entries.clone();
        for pair in current.entries.keys() {
            pairs.entry(pair.clone()).or_default();
        }
        pairs
            .into_iter()
            .filter_map(|((rule, file), allowed)| {
                let found = current
                    .entries
                    .get(&(rule.clone(), file.clone()))
                    .copied()
                    .unwrap_or_default();
                if found > allowed {
                    let findings = findings
                        .iter()
                        .filter(|finding| finding.rule == rule && finding.file == file)
                        .cloned()
                        .collect();
                    Some(Problem::Exceeded {
                        rule,
                        file,
                        allowed,
                        findings,
                    })
                } else if found < allowed {
                    Some(Problem::Stale {
                        baseline: self.file.clone(),
                        rule,
                        file,
                        recorded: allowed,
                        found,
                    })
                } else {
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempRoot(std::path::PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "gcs-mimic-lint-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir_all(path.join("xtask")).unwrap();
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_dir_all(&self.0) {
                eprintln!("lint test directory cleanup failed: {error}");
            }
        }
    }

    fn finding(line: usize) -> Finding {
        Finding::new("clippy::wildcard_enum_match_arm", "src/example.rs", line)
    }

    #[test]
    fn counts_cannot_increase_and_improvements_must_be_recorded() {
        let baseline = Baseline::from_findings(&[finding(1), finding(2)]);
        assert!(baseline.check(&[finding(10), finding(20)]).is_empty());
        let findings = [finding(1), finding(2), finding(3)];
        let problems = baseline.check(&findings);
        assert!(
            matches!(problems.as_slice(), [Problem::Exceeded { allowed: 2, findings, .. }] if findings.len() == 3)
        );
        assert!(matches!(
            baseline.check(&[finding(1)]).as_slice(),
            [Problem::Stale {
                recorded: 2,
                found: 1,
                ..
            }]
        ));
        assert!(matches!(
            baseline.check(&[]).as_slice(),
            [Problem::Stale { found: 0, .. }]
        ));
        assert!(
            !baseline
                .check(&[Finding::new(
                    "clippy::wildcard_enum_match_arm",
                    "src/new.rs",
                    1
                )])
                .is_empty()
        );
    }

    #[test]
    fn shared_and_target_files_are_isolated_and_each_rejects_growth_and_staleness() {
        let directory = TempRoot::new();
        let root = &directory.0;
        let shared = [Finding::new("text::downcast", "src/shared.rs", 1)];
        fs::write(root.join(FILE), Baseline::from_findings(&shared).render()).unwrap();
        for (target, count) in [
            ("x86_64-unknown-linux-gnu", 2),
            ("x86_64-pc-windows-msvc", 3),
        ] {
            let findings = (0..count).map(finding).collect::<Vec<_>>();
            fs::write(
                root.join(file(Some(target))),
                Baseline::from_findings(&findings).render(),
            )
            .unwrap();
        }
        let baseline = Baseline::load(root, None).unwrap();
        assert!(baseline.check(&shared).is_empty());
        assert!(matches!(
            baseline.check(&[]).as_slice(),
            [Problem::Stale { .. }]
        ));
        assert!(!baseline.check(&[shared[0].clone(), finding(4)]).is_empty());
        for (target, count) in [
            ("x86_64-unknown-linux-gnu", 2),
            ("x86_64-pc-windows-msvc", 3),
        ] {
            let baseline = Baseline::load(root, Some(target)).unwrap();
            let findings = (0..count).map(finding).collect::<Vec<_>>();
            assert!(baseline.check(&findings).is_empty());
            let stale = baseline.check(&findings[..count - 1]);
            assert!(matches!(stale.as_slice(), [Problem::Stale { .. }]));
            assert!(stale[0].to_string().contains(&file(Some(target))));
            let mut increased = findings;
            increased.push(finding(count));
            assert!(matches!(
                baseline.check(&increased).as_slice(),
                [Problem::Exceeded { .. }]
            ));
        }
        let missing = Baseline::load(root, Some("aarch64-unknown-linux-gnu")).unwrap();
        assert!(missing.check(&[]).is_empty());
        assert!(matches!(
            missing.check(&[finding(1)]).as_slice(),
            [Problem::Exceeded { allowed: 0, .. }]
        ));
    }

    #[test]
    fn newer_compilers_warn_on_decreases_but_never_allow_growth() {
        let baseline = Baseline::from_findings(&[finding(1), finding(2)]);
        let stale = baseline.check(&[finding(1)]);
        assert!(stale[0].fails(StaleCounts::Reject));
        assert!(!stale[0].fails(StaleCounts::Warn));
        let growth = baseline.check(&[finding(1), finding(2), finding(3)]);
        assert!(growth[0].fails(StaleCounts::Reject));
        assert!(growth[0].fails(StaleCounts::Warn));
    }

    #[test]
    fn baseline_format_round_trips_and_rejects_invalid_entries() {
        let baseline = Baseline::from_findings(&[
            finding(1),
            finding(2),
            Finding::new("text::downcast", "src/other.rs", 5),
        ]);
        assert_eq!(Baseline::parse(&baseline.render(), FILE).unwrap(), baseline);
        for text in [
            "clippy::wildcard_enum_match_arm\tsrc/example.rs\tzero",
            "clippy::wildcard_enum_match_arm\tsrc/example.rs\t0",
            "clippy::unwrap_used\tsrc/example.rs\t1",
            "unknown\tsrc/example.rs\t1",
            "clippy::wildcard_enum_match_arm\tsrc/../outside.rs\t1",
            "clippy::wildcard_enum_match_arm\tsrc/example.rs\t1\textra",
            "clippy::wildcard_enum_match_arm\tsrc/example.rs\t1\nclippy::wildcard_enum_match_arm\tsrc/example.rs\t2",
        ] {
            assert!(Baseline::parse(text, FILE).is_err(), "{text}");
        }
    }
}
