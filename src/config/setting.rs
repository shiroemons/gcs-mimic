use super::{SettingFix, bounded, unknown_setting};
use crate::{diagnostics::DiagnosticSource, time_limit::TimeLimit};
use std::fmt;

pub(super) const UNKNOWN_SETTING: &str = "Unknown setting; remove it and use --help.";
pub(crate) const MAX_OBJECT_BYTES: u64 = 5 * 1024_u64.pow(4);
/// A 1024-byte object name cannot nest deeper than this, so a larger limit could admit nothing.
/// A walk that holds a descriptor per level stops here even when no limit is set.
pub(crate) const MAX_TREE_DEPTH: usize = 1024;

super::string_enum! {
    no_default listed pub(crate) enum Setting {
        Config => "config",
        Scheme => "scheme",
        Backend => "backend",
        SeedDir => "seed_dir",
        LogLevel => "log_level",
        DefaultLocation => "default_location",
        DefaultSoftDeleteRetentionSeconds => "default_soft_delete_retention_seconds",
        LifecycleExecution => "lifecycle_execution",
        Listen => "listen",
        ListenHttp => "listen_http",
        GrpcListen => "grpc_listen",
        TrustedListen => "trusted_listen",
        TrustedPrincipal => "trusted_principal",
        TrustedAllowedHosts => "trusted_allowed_hosts",
        UnixSocket => "unix_socket",
        TlsCert => "tls_cert",
        TlsKey => "tls_key",
        TlsHandshakeTimeoutMs => "tls_handshake_timeout_ms",
        DataDir => "data_dir",
        MemoryMaxBytes => "memory_max_bytes",
        MemoryCatalogMaxBytes => "memory_catalog_max_bytes",
        DefaultSeedDir => "default_seed_dir",
        SeedProject => "seed_project",
        MaxSeedEntries => "max_seed_entries",
        MaxSeedDepth => "max_seed_depth",
        SeedContentType => "seed_content_type",
        SyncDir => "sync_dir",
        SyncDeleteGuard => "sync_delete_guard",
        SyncReinitialize => "sync_reinitialize",
        MaxSyncEntries => "max_sync_entries",
        MaxSyncDepth => "max_sync_depth",
        BucketsFile => "buckets_file",
        Buckets => "buckets",
        BucketsMode => "buckets_mode",
        MaxBucketsPerBatch => "max_buckets_per_batch",
        ApiBaseUrl => "api_base_url",
        PublicBaseUrl => "public_base_url",
        AdminEnabled => "admin_enabled",
        AdminPrincipal => "admin_principal",
        AdminAllowedHosts => "admin_allowed_hosts",
        LocalCreatorEmail => "local_creator_email",
        XmlVirtualHostDomains => "xml_virtual_host_domains",
        MaxConnections => "max_connections",
        MaxGrpcStreams => "max_grpc_streams",
        MaxObjectBytes => "max_object_bytes",
        MaxResumableSessions => "max_resumable_sessions",
        MaxRewriteSessions => "max_rewrite_sessions",
        WorkerThreads => "worker_threads",
        MaxBlockingThreads => "max_blocking_threads",
        HeaderReadTimeoutMs => "header_read_timeout_ms",
        BodyInactivityTimeoutMs => "body_inactivity_timeout_ms",
        RequestTimeoutMs => "request_timeout_ms",
        ConnectionTimeoutMs => "connection_timeout_ms",
        KeepAliveTimeoutMs => "keep_alive_timeout_ms",
        ObjectRequestTimeoutMs => "object_request_timeout_ms",
        CatalogWaitTimeoutMs => "catalog_wait_timeout_ms",
        ShutdownTimeoutMs => "shutdown_timeout_ms",
        AuthMode => "auth_mode",
        CompatProfile => "compat_profile",
        CorsFallback => "cors_fallback",
        SignedHostPort => "signed_host_port",
        IdentityFile => "identity_file",
        Identities => "identities",
        SignedUrls => "signed_urls",
        TestSigner => "test_signer",
        PubsubEmulatorHost => "pubsub_emulator_host",
        EventPubsubProjectId => "event_pubsub_project_id",
        EventPubsubTopic => "event_pubsub_topic",
        EventBucket => "event_bucket",
        EventObjectPrefix => "event_object_prefix",
        EventList => "event_list",
    }
    UNKNOWN_SETTING
}

impl Setting {
    const fn bounds(self) -> Option<(u64, u64)> {
        match self {
            // Zero disables a limit, and `TimeLimit` keeps every other value safe to use.
            Self::TlsHandshakeTimeoutMs
            | Self::ConnectionTimeoutMs
            | Self::HeaderReadTimeoutMs
            | Self::BodyInactivityTimeoutMs
            | Self::RequestTimeoutMs
            | Self::KeepAliveTimeoutMs
            | Self::ObjectRequestTimeoutMs
            | Self::CatalogWaitTimeoutMs
            | Self::ShutdownTimeoutMs => Some((0, u64::MAX)),
            Self::MemoryMaxBytes => Some((1, 1024_u64.pow(4))),
            Self::MemoryCatalogMaxBytes => Some((1024 * 1024, 1024_u64.pow(4))),
            Self::MaxConnections => Some((1, 1_048_576)),
            // A stream's memory is bounded but counts per connection, so the ceiling stays finite.
            Self::MaxGrpcStreams => Some((1, 65_536)),
            // Cloud Storage's documented object size limit, whatever the write method.
            Self::MaxObjectBytes => Some((1, MAX_OBJECT_BYTES)),
            // 0 means unlimited here only; the config holds `Option<NonZeroU32>`.
            Self::MaxResumableSessions | Self::MaxRewriteSessions => Some((0, u32::MAX as u64)),
            // 0 means unlimited here too; the config holds `Option<NonZeroUsize>`.
            Self::MaxSeedEntries | Self::MaxSyncEntries | Self::MaxBucketsPerBatch => {
                Some((0, u64::MAX))
            }
            Self::MaxSeedDepth | Self::MaxSyncDepth => Some((0, MAX_TREE_DEPTH as u64)),
            Self::WorkerThreads | Self::MaxBlockingThreads => Some((1, 1024)),
            Self::Config
            | Self::LogLevel
            | Self::Listen
            | Self::ListenHttp
            | Self::GrpcListen
            | Self::TrustedListen
            | Self::UnixSocket
            | Self::Scheme
            | Self::TlsCert
            | Self::TlsKey
            | Self::DataDir
            | Self::Backend
            | Self::SeedDir
            | Self::DefaultSeedDir
            | Self::SeedProject
            | Self::SeedContentType
            | Self::SyncDir
            | Self::SyncDeleteGuard
            | Self::SyncReinitialize
            | Self::DefaultLocation
            | Self::DefaultSoftDeleteRetentionSeconds
            | Self::LifecycleExecution
            | Self::BucketsFile
            | Self::Buckets
            | Self::BucketsMode
            | Self::ApiBaseUrl
            | Self::PublicBaseUrl
            | Self::AdminEnabled
            | Self::AdminPrincipal
            | Self::AdminAllowedHosts
            | Self::TrustedPrincipal
            | Self::TrustedAllowedHosts
            | Self::LocalCreatorEmail
            | Self::XmlVirtualHostDomains
            | Self::AuthMode
            | Self::CompatProfile
            | Self::CorsFallback
            | Self::SignedHostPort
            | Self::IdentityFile
            | Self::Identities
            | Self::TestSigner
            | Self::SignedUrls
            | Self::PubsubEmulatorHost
            | Self::EventPubsubProjectId
            | Self::EventPubsubTopic
            | Self::EventBucket
            | Self::EventObjectPrefix
            | Self::EventList => None,
        }
    }

    pub(super) fn integer(self, value: &str) -> Result<u64, SettingFix> {
        let (minimum, maximum) = self.bounds().ok_or_else(unknown_setting)?;
        bounded(self.as_str(), value, minimum, maximum)
    }

    pub(super) fn time_limit(self, value: &str) -> Result<TimeLimit, SettingFix> {
        self.integer(value).map(TimeLimit::from_millis)
    }
}

/// A setting name as one source writes it, so that a fix can be copied back into that source:
/// `--foo-bar` on the command line, `GCS_MIMIC_FOO_BAR` in the environment and `foo_bar` in
/// the JSON file. Every other source names settings as the JSON file does.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Spelled<'a> {
    name: &'a str,
    source: DiagnosticSource,
}

impl<'a> Spelled<'a> {
    /// `name` is a setting name in its JSON form, such as `buckets_mode`.
    pub(crate) const fn new(name: &'a str, source: DiagnosticSource) -> Self {
        Self { name, source }
    }
}

impl Setting {
    pub(crate) fn spelled(self, source: DiagnosticSource) -> Spelled<'static> {
        Spelled::new(self.as_str(), source)
    }
}

impl fmt::Display for Spelled<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use fmt::Write as _;
        let (prefix, map): (&str, fn(char) -> char) = match self.source {
            DiagnosticSource::Cli => ("--", |c| if c == '_' { '-' } else { c }),
            DiagnosticSource::Environment => ("GCS_MIMIC_", |c| c.to_ascii_uppercase()),
            _ => ("", |c| c),
        };
        f.write_str(prefix)?;
        self.name.chars().try_for_each(|c| f.write_char(map(c)))
    }
}

/// Whether a setting does nothing without `sync_dir`, so that naming it alone is a mistake.
pub(super) fn requires_sync_dir(name: &str) -> bool {
    name.starts_with("sync_")
        || name == Setting::MaxSyncEntries.as_str()
        || name == Setting::MaxSyncDepth.as_str()
}
