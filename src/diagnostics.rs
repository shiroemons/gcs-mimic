#![deny(clippy::unwrap_used, clippy::expect_used)]

#[cfg(test)]
pub(crate) mod test_support;

use serde::Serialize;
use std::{
    fmt,
    io::{self, Write},
    sync::atomic::{AtomicU8, Ordering},
};

crate::choice::string_enum! {
    table
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
    #[repr(u8)]
    pub enum LogLevel as pub as_str {
        Debug => "debug",
        #[default]
        Info => "info",
        Warn => "warn" | "warning",
        Error => "error",
    };
    from_str(crate::choice::InvalidChoice, _ => Err(crate::choice::InvalidChoice(
        "log_level must be debug, info, warn or error (warning aliases warn)",
    )));
    serde()
}

static LOG_LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info as u8);

/// Configure the process threshold after preflight and before starting workers.
pub fn set_log_level(level: LogLevel) {
    LOG_LEVEL.store(level as u8, Ordering::Relaxed);
}

/// The threshold [`set_log_level`] stored. It is the only writer, so the stored value always
/// names a level; anything else would hide all but errors.
fn configured_level() -> LogLevel {
    let stored = LOG_LEVEL.load(Ordering::Relaxed);
    [
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
    ]
    .into_iter()
    .find(|level| *level as u8 == stored)
    .unwrap_or(LogLevel::Error)
}

pub fn enabled(level: LogLevel) -> bool {
    level >= configured_level()
}

crate::choice::string_enum! {
    table
    /// An event written as a JSON line by the server or a command.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Event as pub as_str {
        /// Published event name.
        Ready => "ready",
        /// Published event name.
        Stopped => "stopped",
        /// Published event name.
        Diagnostics => "diagnostics",
        /// Published event name.
        DataDirCreated => "data_dir_created",
        /// Published event name.
        ImportPage => "import_page",
        /// Published event name.
        ImportSummary => "import_summary",
        /// Published event name.
        ImportComplete => "import_complete",
        AcceptFailed => "accept_failed",
        AcceptRecovered => "accept_recovered",
        AdminResultUnencodable => "admin_result_unencodable",
        AppendableCleanupDeferred => "appendable_cleanup_deferred",
        AuthSelfSignedJwtRejected => "auth_self_signed_jwt_rejected",
        AuthorizationDenied => "authorization_denied",
        BackgroundTaskCancelled => "background_task_cancelled",
        BackgroundTaskPanicked => "background_task_panicked",
        BackgroundTaskRestarting => "background_task_restarting",
        BatchResponseStreamFailed => "batch_response_stream_failed",
        BindWait => "bind_wait",
        BlobReclaimDeferred => "blob_reclaim_deferred",
        BlobShardCleanupDeferred => "blob_shard_cleanup_deferred",
        BucketPurgeCleanupDeferred => "bucket_purge_cleanup_deferred",
        CatalogReaderUnavailable => "catalog_reader_unavailable",
        CatalogSchemaMigrated => "catalog_schema_migrated",
        ComposeCleanupDeferred => "compose_cleanup_deferred",
        ConnectionClosed => "connection_closed",
        ConnectionFailed => "connection_failed",
        ConnectionTaskCancelled => "connection_task_cancelled",
        ConnectionTaskPanicked => "connection_task_panicked",
        ConnectionTimeout => "connection_timeout",
        CorsFallbackDefault => "cors_fallback_default",
        CorsLookupFailed => "cors_lookup_failed",
        CorsUnconfigured => "cors_unconfigured",
        DataLockReleaseFailed => "data_lock_release_failed",
        DerivedCleanupDeferred => "derived_cleanup_deferred",
        DerivedTransferDiscarded => "derived_transfer_discarded",
        GrpcConnectionFailed => "grpc_connection_failed",
        GrpcDrainTimeout => "grpc_drain_timeout",
        GrpcHandshakeTimeout => "grpc_handshake_timeout",
        GrpcIdleReclaimed => "grpc_idle_reclaimed",
        GrpcIdleTimeout => "grpc_idle_timeout",
        GrpcKeepaliveTimeout => "grpc_keepalive_timeout",
        GrpcObjectReadFailure => "grpc_object_read_failure",
        GrpcStreamInactivityTimeout => "grpc_stream_inactivity_timeout",
        ImportProgress => "import_progress",
        ImportReport => "import_report",
        LifecycleBucketFailed => "lifecycle_bucket_failed",
        LifecycleRulesQuarantined => "lifecycle_rules_quarantined",
        LifecycleVersionQuarantined => "lifecycle_version_quarantined",
        LockPoisonRecovered => "lock_poison_recovered",
        MultipartCleanupDeferred => "multipart_cleanup_deferred",
        NotificationCatalogRetry => "notification_catalog_retry",
        NotificationOutboxEvicted => "notification_outbox_evicted",
        NotificationPublishRetry => "notification_publish_retry",
        NotificationTopicCheckFailed => "notification_topic_check_failed",
        ObjectBackendFailure => "object_backend_failure",
        ObjectBodyReadFailed => "object_body_read_failed",
        ObjectDeleteRollbackFailed => "object_delete_rollback_failed",
        ObjectRequestTimeout => "object_request_timeout",
        ObjectTranscodingFailed => "object_transcoding_failed",
        PublicationCleanupFailed => "publication_cleanup_failed",
        RamCheckpointCleanupFailed => "ram_checkpoint_cleanup_failed",
        RamDraftCleanupFailed => "ram_draft_cleanup_failed",
        ResetCleanupDeferred => "reset_cleanup_deferred",
        ResumableMaintenanceRetry => "resumable_maintenance_retry",
        RewriteCleanupDeferred => "rewrite_cleanup_deferred",
        SeedCleanupDeferred => "seed_cleanup_deferred",
        SeedCommitStarted => "seed_commit_started",
        SeedObjectDeleted => "seed_object_deleted",
        SeedObjectKept => "seed_object_kept",
        SeedPassFailed => "seed_pass_failed",
        SeedPassRecovered => "seed_pass_recovered",
        SeedSkipped => "seed_skipped",
        SeedTreeLarge => "seed_tree_large",
        Seeding => "seeding",
        SignatureHostPortOmitted => "signature_host_port_omitted",
        Stopping => "stopping",
        SyncRetiredFiles => "sync_retired_files",
        SyncRetiredFilesListed => "sync_retired_files_listed",
        SyncRetiredFilesUnchecked => "sync_retired_files_unchecked",
        SyncRetiredStagedKept => "sync_retired_staged_kept",
        TcpKeepaliveFailed => "tcp_keepalive_failed",
        TcpNodelayFailed => "tcp_nodelay_failed",
        TlsHandshakeFailed => "tls_handshake_failed",
        TlsHandshakeTimeout => "tls_handshake_timeout",
        UnixSocketCleanupFailed => "unix_socket_cleanup_failed",
        UploadBatchRetried => "upload_batch_retried",
        UploadCleanupDeferred => "upload_cleanup_deferred",
    };
    serde()
}

#[derive(Serialize)]
struct EventLine<T> {
    event: Event,
    level: LogLevel,
    #[serde(flatten)]
    fields: T,
}

fn write_event<T: Serialize, W: Write>(
    writer: impl FnOnce() -> W,
    threshold: LogLevel,
    level: LogLevel,
    event: Event,
    fields: impl FnOnce() -> T,
) -> io::Result<()> {
    if level < threshold {
        return Ok(());
    }
    let fields = fields();
    let mut writer = writer();
    serde_json::to_writer(
        &mut writer,
        &EventLine {
            event,
            level,
            fields,
        },
    )?;
    writeln!(writer)
}

/// Ordinary events must not turn a closed stderr pipe into a storage failure.
pub fn emit(level: LogLevel, event: Event, fields: impl Serialize) {
    emit_lazy(level, event, || fields);
}

/// Builds fields only when the level is enabled. Use this when preparing fields allocates,
/// as in debug events on connection or request paths. Like [`emit`], it discards write errors.
pub(crate) fn emit_lazy<T: Serialize>(level: LogLevel, event: Event, fields: impl FnOnce() -> T) {
    let _ = write_event(
        || io::stderr().lock(),
        configured_level(),
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

impl From<Diagnostic> for Diagnostics {
    /// One error and nothing else.
    fn from(error: Diagnostic) -> Self {
        Self {
            info: Vec::new(),
            warnings: Vec::new(),
            errors: vec![error],
        }
    }
}

/// The I/O kind printed alongside a startup or shutdown failure.
pub trait RuntimeFailure: std::error::Error {
    fn kind(&self) -> io::ErrorKind;
}

impl RuntimeFailure for io::Error {
    fn kind(&self) -> io::ErrorKind {
        self.kind()
    }
}

impl Diagnostics {
    pub fn error(source: DiagnosticSource, key: impl Into<String>, fix: impl Into<String>) -> Self {
        Diagnostic::new(source, key, fix).into()
    }

    /// The advice for a startup or shutdown failure that has none of its own: its kind and
    /// message, and where to look. Every such failure is reported through this one text.
    pub fn startup_failure(error: &impl RuntimeFailure) -> Self {
        Self::error(
            DiagnosticSource::Runtime,
            "startup",
            format!(
                "Startup or shutdown failed ({:?}): {error}; inspect active connections and configured local resources.",
                error.kind()
            ),
        )
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
                || &mut writer,
                LogLevel::Debug,
                level,
                Event::Diagnostics,
                || serde_json::json!({"diagnostics": records}),
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

/// Appends a failed close to `result`, after the operation's own failure.
pub fn combine_shutdown_results(
    result: Result<(), Diagnostics>,
    close: Result<(), impl RuntimeFailure>,
) -> Result<(), Diagnostics> {
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
    result: Result<(), Diagnostics>,
    cleanup: Result<(), impl RuntimeFailure>,
) -> Result<(), Diagnostics> {
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

fn append_shutdown_failure<E>(
    result: Result<(), Diagnostics>,
    close: Result<(), E>,
    describe: impl FnOnce(&E) -> Diagnostic,
) -> Result<(), Diagnostics> {
    let Err(close) = close else {
        return result;
    };
    let mut diagnostics = result.err().unwrap_or_default();
    diagnostics.errors.push(describe(&close));
    Err(diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_event_names_are_stable() {
        for (event, expected) in [
            (Event::Ready, "ready"),
            (Event::Stopped, "stopped"),
            (Event::Diagnostics, "diagnostics"),
            (Event::DataDirCreated, "data_dir_created"),
            (Event::ImportPage, "import_page"),
            (Event::ImportSummary, "import_summary"),
            (Event::ImportComplete, "import_complete"),
        ] {
            assert_eq!(
                serde_json::to_string(&event).unwrap(),
                format!("{expected:?}")
            );
        }
    }

    #[test]
    fn log_level_spellings_and_json_are_stable() {
        for (level, spelling) in [
            (LogLevel::Debug, "debug"),
            (LogLevel::Info, "info"),
            (LogLevel::Warn, "warn"),
            (LogLevel::Error, "error"),
        ] {
            assert_eq!(spelling.parse::<LogLevel>().unwrap(), level);
            assert_eq!(
                serde_json::to_string(&level).unwrap(),
                format!("{spelling:?}")
            );
        }
        assert_eq!("warning".parse::<LogLevel>().unwrap(), LogLevel::Warn);
        assert_eq!(
            "WARN".parse::<LogLevel>().unwrap_err().to_string(),
            "log_level must be debug, info, warn or error (warning aliases warn)"
        );
    }

    /// Every startup or shutdown failure without advice of its own reports this one text.
    #[test]
    fn startup_failures_render_their_kind_message_and_where_to_look() {
        let diagnostics =
            Diagnostics::startup_failure(&io::Error::new(io::ErrorKind::TimedOut, "x"));
        assert!(diagnostics.info.is_empty() && diagnostics.warnings.is_empty());
        let [error] = diagnostics.errors.as_slice() else {
            panic!("one error expected: {:?}", diagnostics.errors);
        };
        assert_eq!(error.source, DiagnosticSource::Runtime);
        assert_eq!(error.key, "startup");
        assert_eq!(
            error.fix,
            "Startup or shutdown failed (TimedOut): x; inspect active connections and configured local resources."
        );
    }

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
                let mut opened_writer = false;
                let mut built_fields = false;
                write_event(
                    || {
                        opened_writer = true;
                        &mut bytes
                    },
                    threshold,
                    level,
                    Event::ConnectionClosed,
                    || {
                        built_fields = true;
                        serde_json::json!({"message":"line\n\"quoted\""})
                    },
                )
                .unwrap();
                assert_eq!(opened_writer, level >= threshold);
                assert_eq!(built_fields, level >= threshold);
                if level < threshold {
                    assert!(bytes.is_empty());
                } else {
                    assert_eq!(bytes.iter().filter(|byte| **byte == b'\n').count(), 1);
                    let event: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(event["event"], "connection_closed");
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
    fn close_failure_preserves_primary_diagnostics_and_unstructured_errors() {
        let close = Diagnostic::new(
            DiagnosticSource::Catalog,
            "data_dir",
            "Cannot close catalog (PermissionDenied): close failed; inspect persistence before restarting.",
        );
        for primary in [
            Ok(()),
            Err(Diagnostics::error(
                DiagnosticSource::Seed,
                "seed_dir",
                "invalid seed",
            )),
            Err(Diagnostics::startup_failure(&io::Error::other(
                "original failure",
            ))),
        ] {
            let original = primary.as_ref().err().map(|error| error.errors.clone());
            let error = combine_shutdown_results(
                primary,
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "close failed",
                )),
            )
            .unwrap_err();
            assert_eq!(error.errors.last(), Some(&close));
            match original {
                Some(original) => {
                    assert_eq!(error.errors.len(), 2);
                    assert_eq!(error.errors[..1], original[..]);
                }
                None => assert_eq!(error.errors.len(), 1),
            }
        }
        assert!(combine_shutdown_results(Ok(()), Ok::<_, io::Error>(())).is_ok());
    }

    #[test]
    fn unix_socket_cleanup_failure_is_named_once_after_the_shutdown_failure() {
        let denied = || Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));
        let cleanup = Diagnostic::new(
            DiagnosticSource::Listener,
            "unix_socket",
            "Cannot remove Unix socket (PermissionDenied): denied; remove the stale entry before restarting.",
        );
        for (primary, expected) in [
            (Ok(()), 1),
            (
                Err(Diagnostics::startup_failure(&io::Error::other("drain"))),
                2,
            ),
        ] {
            let errors = combine_unix_socket_cleanup(primary, denied())
                .unwrap_err()
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
            DiagnosticSource::Import,
            DiagnosticSource::Listener,
            DiagnosticSource::Runtime,
        ] {
            assert_eq!(serde_json::to_value(source).unwrap(), published(source));
        }
    }
}
