//! Authentication failures and identity document errors.

use super::*;

/// Transport-independent authentication or authorization failure; adapters render it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AuthFailure {
    /// Credentials were presented but are unknown, malformed or of an unsupported kind.
    #[error("invalid credentials")]
    InvalidCredentials,
    /// An anonymous caller needs a permission.
    #[error("login required")]
    LoginRequired,
    /// An authenticated caller lacks the named Cloud Storage permission.
    #[error("{0}")]
    Denied(Box<Denied>),
    /// An authenticated caller is refused for a reason no Cloud Storage permission names,
    /// such as an emulator administration route or project visibility.
    #[error("forbidden")]
    Forbidden,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum AuthError {
    #[error(transparent)]
    Failure(AuthFailure),
    #[error("Authentication verifier unavailable")]
    Unavailable(#[source] crate::cause::BackendCause),
}

impl From<AuthFailure> for AuthError {
    fn from(error: AuthFailure) -> Self {
        Self::Failure(error)
    }
}

/// A persisted resumable-upload identity snapshot that no longer decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid upload identity snapshot")]
pub(crate) struct InvalidUploadSnapshot;

/// Why an identity document is invalid or could not be read. `Display` names, for an invalid
/// entry, its index; token values are never included. Configuration, which reads the document,
/// adds the setting that supplied it.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RegistryError {
    #[error("exceeds {MAX_REGISTRY_MIB} MiB")]
    TooLarge,
    #[error(transparent)]
    Io(std::io::Error),
    /// The file is not valid JSON.
    #[error(transparent)]
    Syntax(serde_json::Error),
    /// The identity document as a whole is invalid.
    #[error("{0}")]
    Invalid(String),
    /// One `identities` entry is invalid.
    #[error("identities[{entry}]: {reason}")]
    InvalidEntry { entry: usize, reason: String },
    /// An identity's `public_keys` entry is not a supported RSA public key.
    #[error("identities[{entry}]: {defect}")]
    PublicKey {
        entry: usize,
        defect: signature::InvalidPublicKey,
    },
    /// An identity's `project_roles` names an invalid project.
    #[error("identities[{entry}]: project {project:?}: {defect}")]
    Project {
        entry: usize,
        project: String,
        defect: InvalidProjectId,
    },
}

impl RegistryError {
    pub(super) fn document(reason: impl Into<String>) -> Self {
        Self::Invalid(reason.into())
    }

    pub(super) fn entry(entry: usize, reason: impl Into<String>) -> Self {
        Self::InvalidEntry {
            entry,
            reason: reason.into(),
        }
    }
}
