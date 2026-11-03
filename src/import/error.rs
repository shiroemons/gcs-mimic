use super::ImportFailure;
use crate::{bucket::CatalogError, diagnostics::Diagnostics, storage::admin::AdminError};
use std::{error::Error, fmt, io, num::TryFromIntError};

#[derive(Clone, Copy, Debug)]
pub enum CleanupOperation {
    Observations,
    TerminalFailure,
    Catalog,
}

impl CleanupOperation {
    fn message(self) -> &'static str {
        match self {
            Self::Observations => "remove temporary import observations also failed",
            Self::TerminalFailure => "persist terminal import failure also failed",
            Self::Catalog => "catalog close also failed",
        }
    }
}

#[derive(Debug)]
pub enum ImportError {
    Diagnostics(Diagnostics),
    Io(io::Error),
    Admin(AdminError),
    Catalog(CatalogError),
    Source(ImportFailure),
    Json(serde_json::Error),
    Integer(TryFromIntError),
    ObservationCleanup(io::Error),
    Cleanup {
        original: Box<Self>,
        operation: CleanupOperation,
        cause: Box<Self>,
    },
}

impl ImportError {
    pub(super) fn with_cleanup(self, operation: CleanupOperation, cause: impl Into<Self>) -> Self {
        Self::Cleanup {
            original: Box::new(self),
            operation,
            cause: Box::new(cause.into()),
        }
    }
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Diagnostics(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
            Self::Admin(error) => error.fmt(f),
            Self::Catalog(error) => error.fmt(f),
            Self::Source(error) => error.fmt(f),
            Self::Json(error) => error.fmt(f),
            Self::Integer(error) => error.fmt(f),
            Self::ObservationCleanup(error) => {
                write!(f, "remove temporary import observations: {error}")
            }
            Self::Cleanup {
                original,
                operation,
                cause,
            } => write!(f, "{original}; {}: {cause}", operation.message()),
        }
    }
}

impl Error for ImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Diagnostics(error) => error.source(),
            Self::Io(error) => error.source(),
            Self::ObservationCleanup(error) => Some(error),
            Self::Admin(error) => error.source(),
            Self::Catalog(error) => error.source(),
            Self::Source(error) => error.source(),
            Self::Json(error) => error.source(),
            Self::Integer(error) => error.source(),
            // Error chains are linear; the secondary cleanup cause remains in the variant.
            Self::Cleanup { original, .. } => Some(original.as_ref()),
        }
    }
}

impl From<Diagnostics> for ImportError {
    fn from(error: Diagnostics) -> Self {
        Self::Diagnostics(error)
    }
}
impl From<io::Error> for ImportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<AdminError> for ImportError {
    fn from(error: AdminError) -> Self {
        Self::Admin(error)
    }
}
impl From<CatalogError> for ImportError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}
impl From<ImportFailure> for ImportError {
    fn from(error: ImportFailure) -> Self {
        Self::Source(error)
    }
}
impl From<serde_json::Error> for ImportError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
impl From<TryFromIntError> for ImportError {
    fn from(error: TryFromIntError) -> Self {
        Self::Integer(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_keeps_both_typed_causes_and_the_existing_message() {
        let original =
            ImportError::Io(io::Error::new(io::ErrorKind::InvalidData, "source changed"));
        let error = original.with_cleanup(
            CleanupOperation::Observations,
            io::Error::new(io::ErrorKind::PermissionDenied, "cleanup denied"),
        );
        assert_eq!(
            error.to_string(),
            "source changed; remove temporary import observations also failed: cleanup denied"
        );
        assert!(error.source().unwrap().is::<ImportError>());
        assert!(error.source().unwrap().source().is_none());
        let ImportError::Cleanup {
            original, cause, ..
        } = error
        else {
            panic!("expected cleanup causes")
        };
        assert!(
            matches!(*original, ImportError::Io(error) if error.kind() == io::ErrorKind::InvalidData)
        );
        assert!(
            matches!(*cause, ImportError::Io(error) if error.kind() == io::ErrorKind::PermissionDenied)
        );
    }
}
