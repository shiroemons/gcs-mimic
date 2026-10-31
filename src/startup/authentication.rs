//! Authentication setup failures retain catalog causes through startup cleanup.

use gcs_mimic::{
    bucket::CatalogError,
    diagnostics::{Diagnostic, DiagnosticSource, Diagnostics},
    storage::Catalog,
};
use std::{error::Error, fmt, io};

#[derive(Debug)]
pub(crate) struct Failure {
    diagnostics: Diagnostics,
    cause: CatalogError,
    cleanup: Option<CatalogError>,
}

impl Failure {
    pub(super) fn new(cause: CatalogError, cleanup: Option<CatalogError>) -> Self {
        let mut diagnostics = if matches!(cause, CatalogError::AuthenticationAlreadyConfigured) {
            Diagnostics::error(
                DiagnosticSource::Runtime,
                "authentication",
                "Catalog authentication was initialized more than once; restart with one startup owner.",
            )
        } else {
            Diagnostics::error(
                DiagnosticSource::Catalog,
                "data_dir",
                "Cannot reconcile stored bucket IAM and ACL state; inspect the catalog or restore a valid backup before restarting.",
            )
        };
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

    #[cfg(test)]
    pub(super) fn cleanup_cause(&self) -> Option<&CatalogError> {
        self.cleanup.as_ref()
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

impl Error for Failure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.cause)
    }
}

pub(super) async fn close(catalog: &Catalog, cause: CatalogError) -> io::Error {
    let cleanup = catalog.close().await.err();
    io::Error::other(Failure::new(cause, cleanup))
}
