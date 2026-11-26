//! Authentication setup failures retain catalog causes during catalog opening.

use gcs_mimic::{
    bucket::CatalogError,
    diagnostics::{DiagnosticSource, Diagnostics},
};
use std::{error::Error, fmt};

#[derive(Debug)]
pub(crate) struct Failure {
    diagnostics: Diagnostics,
    cause: CatalogError,
}

impl Failure {
    pub(super) fn new(cause: CatalogError) -> Self {
        let diagnostics = Diagnostics::error(
            DiagnosticSource::Catalog,
            "data_dir",
            "Cannot reconcile stored bucket IAM and ACL state; inspect the catalog or restore a valid backup before restarting.",
        );
        Self { diagnostics, cause }
    }

    pub(crate) fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.diagnostics.fmt(formatter)
    }
}

impl Error for Failure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.cause)
    }
}
