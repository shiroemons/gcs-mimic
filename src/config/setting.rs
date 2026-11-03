use super::{SettingFix, bounded, unknown_setting};

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
        LocalCreatorEmail => "local_creator_email",
        XmlVirtualHostDomains => "xml_virtual_host_domains",
        MaxConnections => "max_connections",
        MaxObjectBytes => "max_object_bytes",
        WorkerThreads => "worker_threads",
        MaxBlockingThreads => "max_blocking_threads",
        ConnectionTimeoutMs => "connection_timeout_ms",
        KeepAliveTimeoutMs => "keep_alive_timeout_ms",
        ObjectRequestTimeoutMs => "object_request_timeout_ms",
        CatalogWaitTimeoutMs => "catalog_wait_timeout_ms",
        ShutdownTimeoutMs => "shutdown_timeout_ms",
        AuthMode => "auth_mode",
        CompatProfile => "compat_profile",
        CorsFallback => "cors_fallback",
        IdentityFile => "identity_file",
        PubsubEmulatorHost => "pubsub_emulator_host",
        EventPubsubProjectId => "event_pubsub_project_id",
        EventPubsubTopic => "event_pubsub_topic",
        EventBucket => "event_bucket",
        EventObjectPrefix => "event_object_prefix",
        EventList => "event_list",
    }
    "Unknown setting; remove it and use --help."
}

const INTEGER_BOUNDS: &[(Setting, u64, u64)] = &[
    (Setting::TlsHandshakeTimeoutMs, 1, 60_000),
    (Setting::MemoryMaxBytes, 1, 1024 * 1024 * 1024),
    (
        Setting::MemoryCatalogMaxBytes,
        1024 * 1024,
        1024 * 1024 * 1024,
    ),
    (Setting::MaxConnections, 1, 1024),
    (Setting::MaxObjectBytes, 1, 5 * 1024_u64.pow(4)),
    (Setting::WorkerThreads, 1, 32),
    (Setting::MaxBlockingThreads, 1, 32),
    (Setting::ConnectionTimeoutMs, 1, 60_000),
    (Setting::KeepAliveTimeoutMs, 1, 600_000),
    (Setting::ObjectRequestTimeoutMs, 1, 86_400_000),
    (Setting::CatalogWaitTimeoutMs, 1, 60_000),
    (Setting::ShutdownTimeoutMs, 1, 60_000),
];

impl Setting {
    pub(super) fn integer(self, value: &str) -> Result<u64, SettingFix> {
        let (_, minimum, maximum) = INTEGER_BOUNDS
            .iter()
            .find(|(setting, _, _)| *setting == self)
            .ok_or_else(unknown_setting)?;
        bounded(self.as_str(), value, *minimum, *maximum)
    }
}
