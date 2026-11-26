use super::{CatalogError, admin::LocalAdmin, principals::Profile};
use crate::{auth::Auth, clock::Clock, config::Config};
use std::{path::Path, sync::Arc};

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
}

impl CatalogOptions {
    pub(super) fn embedded(clock: Arc<dyn Clock>, local_admin: LocalAdmin) -> Self {
        Self {
            clock,
            local_admin,
            principals: None,
        }
    }

    pub fn authenticated(clock: Arc<dyn Clock>, local_admin: LocalAdmin, auth: &Auth) -> Self {
        Self {
            principals: Some(Profile::from(auth)),
            ..Self::embedded(clock, local_admin)
        }
    }
}

/// The startup phase determines which diagnostic and remedy the CLI reports.
#[derive(Debug, thiserror::Error)]
pub enum CatalogOpenError {
    #[error(transparent)]
    Storage(#[from] CatalogError),
    #[error(transparent)]
    Authentication(CatalogError),
}

impl CatalogOpenError {
    pub(super) fn into_cause(self) -> CatalogError {
        match self {
            Self::Storage(cause) | Self::Authentication(cause) => cause,
        }
    }
}
