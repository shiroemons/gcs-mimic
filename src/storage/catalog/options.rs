use super::{CatalogError, admin::LocalAdmin, principals::Profile};
use crate::{auth::Auth, clock::Clock, config::Config};
use std::{path::Path, sync::Arc};

#[derive(Debug)]
pub enum CatalogBacking<'a> {
    Disk(&'a Path),
    Memory { blob_bytes: u64, catalog_bytes: u64 },
}

impl<'a> From<&'a Config> for CatalogBacking<'a> {
    fn from(config: &'a Config) -> Self {
        match config.backend() {
            crate::config::Backend::Disk => Self::Disk(config.data_dir()),
            crate::config::Backend::Memory => Self::Memory {
                blob_bytes: config.memory_max_bytes(),
                catalog_bytes: config.memory_catalog_max_bytes(),
            },
        }
    }
}

pub struct CatalogOptions {
    pub(super) clock: Arc<dyn Clock>,
    pub(super) local_admin: LocalAdmin,
    pub(super) principals: Option<Profile>,
    pub(super) session_limits: super::session_limits::SessionLimits,
    pub(super) default_soft_delete_retention: Option<std::num::NonZeroU64>,
    pub(super) lifecycle: crate::config::LifecycleExecution,
    pub(super) pubsub_endpoint: Option<crate::notification::Endpoint>,
    pub(super) parent_projects: Vec<String>,
}

impl std::fmt::Debug for CatalogOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CatalogOptions")
            .field("session_limits", &self.session_limits)
            .field("lifecycle", &self.lifecycle)
            .field(
                "default_soft_delete_retention",
                &self.default_soft_delete_retention,
            )
            .finish_non_exhaustive()
    }
}

impl CatalogOptions {
    pub(super) fn embedded(clock: Arc<dyn Clock>, local_admin: LocalAdmin) -> Self {
        Self {
            clock,
            local_admin,
            principals: None,
            session_limits: Default::default(),
            default_soft_delete_retention: None,
            lifecycle: crate::config::LifecycleExecution::Disabled,
            pubsub_endpoint: None,
            parent_projects: Vec::new(),
        }
    }

    pub fn for_server(config: &Config, auth: &Auth, clock: Arc<dyn Clock>) -> Self {
        let admin = if config.admin_enabled() {
            LocalAdmin::Enabled
        } else {
            LocalAdmin::Disabled
        };
        Self::authenticated(clock, admin, auth).with_config(config)
    }

    pub(super) fn with_config(mut self, config: &Config) -> Self {
        self.session_limits = config.into();
        self.default_soft_delete_retention = match config.compat_profile() {
            crate::config::CompatProfile::FakeGcsServer => None,
            crate::config::CompatProfile::Native => config.default_soft_delete_retention_seconds(),
        };
        self.lifecycle = config.lifecycle_execution();
        self.pubsub_endpoint
            .clone_from(&config.notification_runtime().endpoint);
        self
    }

    pub(crate) fn authenticated(
        clock: Arc<dyn Clock>,
        local_admin: LocalAdmin,
        auth: &Auth,
    ) -> Self {
        Self {
            principals: Some(Profile::from(auth)),
            parent_projects: auth.public_access_prevention_projects().to_vec(),
            ..Self::embedded(clock, local_admin)
        }
    }
}

/// The startup phase determines which diagnostic and remedy the CLI reports.
#[derive(Debug, thiserror::Error)]
pub enum CatalogOpenFailure {
    #[error(transparent)]
    Storage(#[from] CatalogError),
    #[error(transparent)]
    Authentication(CatalogError),
    #[error(transparent)]
    ImportState(CatalogError),
    #[error("import is incomplete")]
    ImportInProgress,
    #[error(transparent)]
    ParentPolicy(CatalogError),
}

/// The setup stage that failed, together with any error releasing recovered storage.
#[derive(Debug, thiserror::Error)]
#[error("catalog open failed")]
pub struct CatalogOpenError {
    /// The failure that prevented the catalog from accepting requests.
    #[source]
    pub failure: CatalogOpenFailure,
    /// A secondary failure while closing the recovered catalog.
    pub cleanup: Option<Box<CatalogError>>,
}

impl From<CatalogError> for CatalogOpenError {
    fn from(error: CatalogError) -> Self {
        Self {
            failure: CatalogOpenFailure::Storage(error),
            cleanup: None,
        }
    }
}

impl CatalogOpenError {
    pub(super) fn into_cause(self) -> CatalogError {
        self.failure.into_cause()
    }
}

impl CatalogOpenFailure {
    fn into_cause(self) -> CatalogError {
        match self {
            Self::ImportInProgress => CatalogError::PreconditionFailed,
            Self::Storage(cause)
            | Self::Authentication(cause)
            | Self::ImportState(cause)
            | Self::ParentPolicy(cause) => cause,
        }
    }
}
