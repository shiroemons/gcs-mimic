#![deny(clippy::unwrap_used, clippy::expect_used)]

#[cfg(test)]
pub(crate) mod test_support;

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
    type Err = crate::config::InvalidChoice;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err(crate::config::InvalidChoice(
                "log_level must be debug, info, warn or error (warning aliases warn)",
            )),
        }
    }
}

static LOG_LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info as u8);

/// Configure the process threshold after preflight and before starting workers.
pub fn set_log_level(level: LogLevel) {
    LOG_LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn enabled(level: LogLevel) -> bool {
    (level as u8) >= LOG_LEVEL.load(Ordering::Relaxed)
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_args: Option<Vec<String>>,
}

impl Diagnostic {
    pub fn new(source: DiagnosticSource, key: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            source,
            key: key.into(),
            fix: fix.into(),
            native_args: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub info: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
    pub errors: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn error(source: DiagnosticSource, key: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            info: Vec::new(),
            warnings: Vec::new(),
            errors: vec![Diagnostic::new(source, key, fix)],
        }
    }

    pub fn is_err(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn write(&self, mut writer: impl Write) -> io::Result<()> {
        for (level, records) in [
            (LogLevel::Info, &self.info),
            (LogLevel::Warn, &self.warnings),
            (LogLevel::Error, &self.errors),
        ] {
            if records.is_empty() || (level == LogLevel::Info && !enabled(level)) {
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
    append_shutdown_failure(result, close, |close| {
        Diagnostic::new(
            DiagnosticSource::Catalog,
            "data_dir",
            format!(
                "Cannot close catalog ({:?}): {close}; inspect persistence before restarting.",
                close.kind()
            ),
        )
    })
}

/// Adds a failed removal of the owned Unix socket after the operation's own failure. Only the
/// Unix listener has cleanup that can fail, and the stale entry, not the catalog, needs care.
pub(crate) fn combine_unix_socket_cleanup(
    result: io::Result<()>,
    cleanup: io::Result<()>,
) -> io::Result<()> {
    append_shutdown_failure(result, cleanup, |cleanup| {
        Diagnostic::new(
            DiagnosticSource::Listener,
            "unix_socket",
            format!(
                "Cannot remove Unix socket ({:?}): {cleanup}; remove the stale entry before restarting.",
                cleanup.kind()
            ),
        )
    })
}

fn append_shutdown_failure(
    result: io::Result<()>,
    close: io::Result<()>,
    describe: impl FnOnce(&io::Error) -> Diagnostic,
) -> io::Result<()> {
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
        let diagnostic = describe(&close);
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
                    info: Vec::new(),
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
        first.info.push(Diagnostic::new(
            DiagnosticSource::Cli,
            "native_equivalent",
            "--backend 'disk'",
        ));
        assert!(!first.is_err());
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
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0]["level"], "info");
        assert_eq!(lines[1]["level"], "warn");
        assert_eq!(lines[2]["level"], "error");
        assert_eq!(lines[2]["diagnostics"][0]["fix"], "first\nline");
        assert_eq!(lines[2]["diagnostics"][1]["fix"], "second");
        assert_eq!(lines[2]["diagnostics"][2]["key"], "z");
        assert_eq!(lines[2]["diagnostics"][3]["source"], "config");
        assert_eq!(lines[3]["diagnostics"].as_array().unwrap().len(), 1);
        assert!(first.is_err() && second.is_err());
    }

    #[test]
    fn unix_socket_cleanup_failure_is_named_once_after_the_shutdown_failure() {
        let denied = || Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
        let cleanup = Diagnostic::new(
            DiagnosticSource::Listener,
            "unix_socket",
            "Cannot remove Unix socket (PermissionDenied): denied; remove the stale entry before restarting.",
        );
        for (primary, expected) in [(Ok(()), 1), (Err(io::Error::other("drain")), 2)] {
            let error = combine_unix_socket_cleanup(primary, denied()).unwrap_err();
            let errors = &error
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<Diagnostics>())
                .unwrap()
                .errors;
            assert_eq!(errors.len(), expected, "{errors:?}");
            assert_eq!(errors.last(), Some(&cleanup));
        }
    }

    /// The `source` values are public contract. The match has no wildcard arm, so a new variant
    /// does not compile until its published name is decided here.
    #[test]
    fn every_source_keeps_its_published_name() {
        fn published(source: DiagnosticSource) -> &'static str {
            match source {
                DiagnosticSource::Cli => "cli",
                DiagnosticSource::Environment => "environment",
                DiagnosticSource::Config => "config",
                DiagnosticSource::Tls => "tls",
                DiagnosticSource::Identity => "identity",
                DiagnosticSource::Filesystem => "filesystem",
                DiagnosticSource::Catalog => "catalog",
                DiagnosticSource::Initializer => "initializer",
                DiagnosticSource::Seed => "seed",
                DiagnosticSource::Sync => "sync",
                DiagnosticSource::Import => "import",
                DiagnosticSource::Listener => "listener",
                DiagnosticSource::Runtime => "runtime",
            }
        }
        for source in [
            DiagnosticSource::Cli,
            DiagnosticSource::Environment,
            DiagnosticSource::Config,
            DiagnosticSource::Tls,
            DiagnosticSource::Identity,
            DiagnosticSource::Filesystem,
            DiagnosticSource::Catalog,
            DiagnosticSource::Initializer,
            DiagnosticSource::Seed,
            DiagnosticSource::Sync,
            DiagnosticSource::Import,
            DiagnosticSource::Listener,
            DiagnosticSource::Runtime,
        ] {
            assert_eq!(serde_json::to_value(source).unwrap(), published(source));
        }
    }
}
