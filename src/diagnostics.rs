#![deny(clippy::unwrap_used, clippy::expect_used)]

use serde::Serialize;
use std::{
    fmt,
    io::{self, Write},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSource {
    Cli,
    Environment,
    Config,
    Tls,
    Identity,
    Filesystem,
    Catalog,
    Initializer,
    Seed,
    Sync,
    Import,
    Listener,
    Runtime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub source: DiagnosticSource,
    pub key: String,
    pub fix: String,
}

impl Diagnostic {
    pub fn new(source: DiagnosticSource, key: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            source,
            key: key.into(),
            fix: fix.into(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub warnings: Vec<Diagnostic>,
    pub errors: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn error(source: DiagnosticSource, key: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            warnings: Vec::new(),
            errors: vec![Diagnostic::new(source, key, fix)],
        }
    }

    pub fn is_err(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn write(&self, mut writer: impl Write) -> io::Result<()> {
        for (level, records) in [("warn", &self.warnings), ("error", &self.errors)] {
            if records.is_empty() {
                continue;
            }
            let mut records: Vec<_> = records.iter().collect();
            records.sort_by(|a, b| (a.source, &a.key).cmp(&(b.source, &b.key)));
            serde_json::to_writer(
                &mut writer,
                &serde_json::json!({
                    "event": "diagnostics", "level": level, "diagnostics": records
                }),
            )?;
            writeln!(writer)?;
        }
        Ok(())
    }
}

impl fmt::Display for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, diagnostic) in self.errors.iter().enumerate() {
            if index != 0 {
                f.write_str("; ")?;
            }
            write!(f, "{}: {}", diagnostic.key, diagnostic.fix)?;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostics {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn batches_are_ordered_escaped_independent_and_do_not_replay_pages() {
        let mut first = Diagnostics::default();
        first
            .errors
            .push(Diagnostic::new(DiagnosticSource::Config, "a", "config"));
        first
            .errors
            .push(Diagnostic::new(DiagnosticSource::Cli, "z", "last-key"));
        first.errors.push(Diagnostic::new(
            DiagnosticSource::Cli,
            "a\n\t",
            "first\nline",
        ));
        first
            .errors
            .push(Diagnostic::new(DiagnosticSource::Cli, "a\n\t", "second"));
        first.warnings.push(Diagnostic::new(
            DiagnosticSource::Environment,
            "warning",
            "remove",
        ));
        let mut bytes = Vec::new();
        first.write(&mut bytes).unwrap();
        let second = Diagnostics::error(DiagnosticSource::Import, "objects[2]", "fix next page");
        second.write(&mut bytes).unwrap();
        let lines: Vec<serde_json::Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["level"], "warn");
        assert_eq!(lines[1]["level"], "error");
        assert_eq!(lines[1]["diagnostics"][0]["fix"], "first\nline");
        assert_eq!(lines[1]["diagnostics"][1]["fix"], "second");
        assert_eq!(lines[1]["diagnostics"][2]["key"], "z");
        assert_eq!(lines[1]["diagnostics"][3]["source"], "config");
        assert_eq!(lines[2]["diagnostics"].as_array().unwrap().len(), 1);
        assert!(first.is_err() && second.is_err());
        for (source, expected) in [
            (DiagnosticSource::Tls, "tls"),
            (DiagnosticSource::Identity, "identity"),
            (DiagnosticSource::Filesystem, "filesystem"),
            (DiagnosticSource::Catalog, "catalog"),
            (DiagnosticSource::Initializer, "initializer"),
            (DiagnosticSource::Seed, "seed"),
            (DiagnosticSource::Sync, "sync"),
            (DiagnosticSource::Listener, "listener"),
            (DiagnosticSource::Runtime, "runtime"),
        ] {
            assert_eq!(serde_json::to_value(source).unwrap(), expected);
        }
    }
}
