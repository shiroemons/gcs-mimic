//! Authentication setup failures retain catalog causes during catalog opening.

use gcs_mimic::{
    bucket::CatalogError,
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
};
use std::fmt;

#[derive(Debug, thiserror::Error)]
pub(crate) struct Failure {
    diagnostics: Diagnostics,
    #[source]
    cause: CatalogError,
    cleanup: Option<CatalogError>,
}

impl Failure {
    pub(super) fn new(cause: CatalogError, cleanup: Option<CatalogError>) -> Self {
        let mut diagnostics = Diagnostics::error(
            DiagnosticSource::Catalog,
            "data_dir",
            "Cannot reconcile stored bucket IAM and ACL state; inspect the catalog or restore a valid backup before restarting.",
        );
        if cleanup.is_some() {
            diagnostics.errors.push(Diagnostic::new(
                DiagnosticSource::Catalog,
                "data_dir",
                "Cannot close the catalog after authentication setup failed; inspect persistence before restarting.",
            ));
        }
        Self {
            diagnostics,
            cause,
            cleanup,
        }
    }

    pub(crate) fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.diagnostics.fmt(formatter)?;
        if let Some(cleanup) = &self.cleanup {
            write!(formatter, "; catalog close failed: {cleanup}")?;
        }
        Ok(())
    }
}
