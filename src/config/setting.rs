use super::{SettingFix, bounded, unknown_setting};
use crate::time_limit::TimeLimit;

pub(super) const UNKNOWN_SETTING: &str = "Unknown setting; remove it and use --help.";
pub(crate) const MAX_OBJECT_BYTES: u64 = 5 * 1024_u64.pow(4);
/// A 1024-byte object name cannot nest deeper than this, so a larger limit could admit nothing.
/// A walk that holds a descriptor per level stops here even when no limit is set.
pub(crate) const MAX_TREE_DEPTH: usize = 1024;

super::string_enum! {
    no_default pub(crate) enum Setting {
        Config => "config",
        LogLevel => "log_level",
        Listen => "listen",
        ListenHttp => "listen_http",
        GrpcListen => "grpc_listen",
        UnixSocket => "unix_socket",
        Scheme => "scheme",
        TlsCert => "tls_cert",
        TlsKey => "tls_key",
        TlsHandshakeTimeoutMs => "tls_handshake_timeout_ms",
        DataDir => "data_dir",
        Backend => "backend",
        MemoryMaxBytes => "memory_max_bytes",
        MemoryCatalogMaxBytes => "memory_catalog_max_bytes",
        SeedDir => "seed_dir",
        DefaultSeedDir => "default_seed_dir",
        SeedProject => "seed_project",
        SeedContentType => "seed_content_type",
        MaxSeedEntries => "max_seed_entries",
        MaxSeedDepth => "max_seed_depth",
        MaxBucketsPerBatch => "max_buckets_per_batch",
        MaxSyncEntries => "max_sync_entries",
        MaxSyncDepth => "max_sync_depth",
        SyncDir => "sync_dir",
        SyncDeleteGuard => "sync_delete_guard",
        SyncReinitialize => "sync_reinitialize",
        DefaultLocation => "default_location",
        DefaultSoftDeleteRetentionSeconds => "default_soft_delete_retention_seconds",
        BucketsFile => "buckets_file",
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
        ConnectionTimeoutMs => "connection_timeout_ms",
        HeaderReadTimeoutMs => "header_read_timeout_ms",
        BodyInactivityTimeoutMs => "body_inactivity_timeout_ms",
        RequestTimeoutMs => "request_timeout_ms",
        KeepAliveTimeoutMs => "keep_alive_timeout_ms",
        ObjectRequestTimeoutMs => "object_request_timeout_ms",
        CatalogWaitTimeoutMs => "catalog_wait_timeout_ms",
        ShutdownTimeoutMs => "shutdown_timeout_ms",
        AuthMode => "auth_mode",
        CompatProfile => "compat_profile",
        CorsFallback => "cors_fallback",
        IdentityFile => "identity_file",
        TestSigner => "test_signer",
        SignedUrls => "signed_urls",
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
            | Self::BucketsFile
            | Self::ApiBaseUrl
            | Self::PublicBaseUrl
            | Self::AdminEnabled
            | Self::AdminPrincipal
            | Self::AdminAllowedHosts
            | Self::LocalCreatorEmail
            | Self::XmlVirtualHostDomains
            | Self::AuthMode
            | Self::CompatProfile
            | Self::CorsFallback
            | Self::IdentityFile
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

/// Whether a setting does nothing without `sync_dir`, so that naming it alone is a mistake.
pub(super) fn requires_sync_dir(name: &str) -> bool {
    name.starts_with("sync_")
        || name == Setting::MaxSyncEntries.as_str()
        || name == Setting::MaxSyncDepth.as_str()
}
