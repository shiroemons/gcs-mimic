#![deny(clippy::unwrap_used, clippy::expect_used)]

use serde::Serialize;
use std::{
    fmt,
    io::{self, Write},
    sync::atomic::{AtomicU8, Ordering},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum LogLevel {
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl std::str::FromStr for LogLevel {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err("log_level must be debug, info, warn or error (warning aliases warn)".into()),
        }
    }
}

static LOG_LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info as u8);

/// Configure the process threshold after preflight and before starting workers.
pub fn set_log_level(level: LogLevel) {
    LOG_LEVEL.store(level as u8, Ordering::Relaxed);
}

#[derive(Serialize)]
struct Event<'a, T> {
    event: &'a str,
    level: LogLevel,
    #[serde(flatten)]
    fields: T,
}

fn write_event(
    mut writer: impl Write,
    threshold: u8,
    level: LogLevel,
    event: &str,
    fields: impl Serialize,
) -> io::Result<()> {
    if (level as u8) < threshold {
        return Ok(());
    }
    serde_json::to_writer(
        &mut writer,
        &Event {
            event,
            level,
            fields,
        },
    )?;
    writeln!(writer)
}

/// Ordinary events must not turn a closed stderr pipe into a storage failure.
pub fn emit(level: LogLevel, event: &str, fields: impl Serialize) {
    let _ = write_event(
        io::stderr().lock(),
        LOG_LEVEL.load(Ordering::Relaxed),
        level,
        event,
        fields,
    );
}

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
        for (level, records) in [
            (LogLevel::Warn, &self.warnings),
            (LogLevel::Error, &self.errors),
        ] {
            if records.is_empty() {
                continue;
            }
            let mut records: Vec<_> = records.iter().collect();
            records.sort_by(|a, b| (a.source, &a.key).cmp(&(b.source, &b.key)));
            write_event(
                &mut writer,
                LogLevel::Debug as u8,
                level,
                "diagnostics",
                serde_json::json!({"diagnostics": records}),
            )?;
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

/// Preserve the operation failure and catalog-close cause on every shutdown path.
pub fn combine_shutdown_results(result: io::Result<()>, close: io::Result<()>) -> io::Result<()> {
    let mut result = result.map_err(|error| {
        if error.get_ref().is_some_and(|inner| inner.is::<Diagnostics>()) {
            error
        } else {
            io::Error::new(error.kind(), Diagnostics::error(
                DiagnosticSource::Runtime, "startup",
                format!("Startup or shutdown failed ({:?}): {error}; inspect active connections and configured local resources.", error.kind()),
            ))
        }
    });
    if let Err(close) = close {
        let diagnostic = Diagnostic::new(
            DiagnosticSource::Catalog,
            "data_dir",
            format!(
                "Cannot close catalog ({:?}): {close}; inspect persistence before restarting.",
                close.kind()
            ),
        );
        if let Err(error) = &mut result {
            if let Some(diagnostics) = error
                .get_mut()
                .and_then(|inner| inner.downcast_mut::<Diagnostics>())
            {
                diagnostics.errors.push(diagnostic);
            }
        } else {
            result = Err(io::Error::new(
                close.kind(),
                Diagnostics {
                    warnings: Vec::new(),
                    errors: vec![diagnostic],
                },
            ));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn event_thresholds_filter_each_severity_and_escape_json() {
        let levels = [
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
        ];
        for threshold in levels {
            for level in levels {
                let mut bytes = Vec::new();
                write_event(
                    &mut bytes,
                    threshold as u8,
                    level,
                    "operation",
                    serde_json::json!({"message":"line\n\"quoted\""}),
                )
                .unwrap();
                if level < threshold {
                    assert!(bytes.is_empty());
                } else {
                    assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
                    let event: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(event["event"], "operation");
                    assert_eq!(event["level"], serde_json::to_value(level).unwrap());
                    assert_eq!(event["message"], "line\n\"quoted\"");
                }
            }
        }
    }

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
