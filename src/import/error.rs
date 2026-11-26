use super::{
    ImportFailure,
    codec::{CodecError, TimestampError},
};
use crate::{
    bucket::{CatalogError, DefaultBucketError},
    diagnostics::Diagnostics,
    storage::{admin::AdminError, import_journal::ResumeError},
};
use std::{borrow::Cow, error::Error, fmt, io, num::TryFromIntError};

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
    /// The destination holds another import, or this one with other options. A resume compares
    /// one digest of the source and options, so it cannot tell which of them differs.
    OtherImport,
    /// The destination's journal refused the resume.
    Resume(ResumeError),
    /// The source changed between the scans or since its elements were committed.
    SourceChanged(SourceChange),
    /// The source or destination contradicts this import, so a retry cannot succeed; the journal
    /// records the failure.
    Invalid(Cow<'static, str>),
    /// A source read failed during the scan; retry the same snapshot after repairing access.
    SourceUnreadable,
    /// A source value the codec cannot convert.
    Codec(CodecError),
}

/// Why the source no longer matches the import. The `source_changed:` prefix is part of the text
/// the journal records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceChange {
    /// The final scan's export manifest differs from the preflight's.
    ExportMetadata,
    /// The journal's binding names another source fingerprint.
    Binding,
    /// A committed element's fingerprint differs from the one observed now.
    CommittedIdentity,
}

impl fmt::Display for SourceChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ExportMetadata => {
                "source_changed: keep the source immutable; abandon this incomplete destination if export metadata changed"
            }
            Self::Binding => {
                "source_changed: abandon the incomplete destination and use a fresh immutable export"
            }
            Self::CommittedIdentity => "source_changed: committed source identity differs",
        })
    }
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
            Self::OtherImport => f.write_str(
                "data-dir: destination is not the same unfinished import with matching options; repeat the --source, --source-format, --metadata-mode, --source-platform, --max-object-bytes, --max-entries and --max-depth of its first run, or choose a new empty destination",
            ),
            Self::Resume(error) => error.fmt(f),
            Self::SourceChanged(change) => change.fmt(f),
            Self::Invalid(message) => f.write_str(message),
            Self::SourceUnreadable => f.write_str(
                "source I/O failure: resolve the reported filesystem error and retry the same immutable snapshot",
            ),
            Self::Codec(error) => error.fmt(f),
        }
    }
}

impl Error for ImportError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Diagnostics(error) => error.source(),
            Self::Io(error) | Self::ObservationCleanup(error) => error.source(),
            Self::Admin(error) => error.source(),
            Self::Catalog(error) => error.source(),
            Self::Source(error) => error.source(),
            Self::Json(error) => error.source(),
            Self::Integer(error) => error.source(),
            // Display includes the original and the cleanup failure. Follow the original's causes.
            Self::Cleanup { original, .. } => original.source(),
            Self::Resume(error) => error.source(),
            Self::Codec(error) => error.source(),
            Self::OtherImport
            | Self::SourceChanged(_)
            | Self::Invalid(_)
            | Self::SourceUnreadable => None,
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
impl From<DefaultBucketError> for ImportError {
    fn from(error: DefaultBucketError) -> Self {
        let message = match error {
            DefaultBucketError::Project(_) => crate::auth::InvalidProjectId::MESSAGE,
            DefaultBucketError::Timestamp(_) => crate::time_format::TimeOutOfRange::MESSAGE,
        };
        Self::Invalid(message.into())
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
impl From<SourceChange> for ImportError {
    fn from(change: SourceChange) -> Self {
        Self::SourceChanged(change)
    }
}
impl From<CodecError> for ImportError {
    fn from(error: CodecError) -> Self {
        Self::Codec(error)
    }
}
impl From<TimestampError> for ImportError {
    fn from(error: TimestampError) -> Self {
        Self::Codec(error.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The journal stores a terminal failure's text, and a resume shows a refusal's text, so each
    /// named failure keeps the exact text it had as an `InvalidData` I/O error.
    #[test]
    fn named_import_failures_keep_their_recorded_text() {
        for (error, text) in [
            (
                ImportError::OtherImport,
                "data-dir: destination is not the same unfinished import with matching options; repeat the --source, --source-format, --metadata-mode, --source-platform, --max-object-bytes, --max-entries and --max-depth of its first run, or choose a new empty destination",
            ),
            (
                ImportError::Resume(ResumeError::PermanentlyFailed {
                    reason: crate::storage::import_journal::JournalFailure::Adapter(
                        "checksum mismatch".into(),
                    ),
                }),
                "import permanently failed: checksum mismatch; preserve the source snapshot and restart into a new empty destination",
            ),
            (
                SourceChange::ExportMetadata.into(),
                "source_changed: keep the source immutable; abandon this incomplete destination if export metadata changed",
            ),
            (
                SourceChange::Binding.into(),
                "source_changed: abandon the incomplete destination and use a fresh immutable export",
            ),
            (
                SourceChange::CommittedIdentity.into(),
                "source_changed: committed source identity differs",
            ),
            (
                ImportError::Invalid("missing committed bucket".into()),
                "missing committed bucket",
            ),
            (
                ImportError::SourceUnreadable,
                "source I/O failure: resolve the reported filesystem error and retry the same immutable snapshot",
            ),
        ] {
            assert_eq!(error.to_string(), text);
            assert!(error.source().is_none(), "{error:?}");
        }
    }

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
        assert_eq!(crate::error::Report(&error).to_string(), error.to_string());
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

    #[test]
    fn prefixed_import_errors_report_their_cause_once() {
        let json = serde_json::from_str::<bool>("invalid").unwrap_err();
        let json_message = format!("invalid source snapshot JSON: {json}");
        for (error, message) in [
            (ImportError::from(ImportFailure::Json(json)), json_message),
            (
                ImportError::ObservationCleanup(io::Error::other("cleanup denied")),
                "remove temporary import observations: cleanup denied".into(),
            ),
        ] {
            assert_eq!(error.to_string(), message);
            assert_eq!(crate::error::Report(&error).to_string(), message);
        }
    }

    #[test]
    fn cleanup_forwards_the_original_errors_underlying_cause() {
        let original = CatalogError::from(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "disk detail",
        ));
        let error = ImportError::Catalog(original)
            .with_cleanup(CleanupOperation::Catalog, io::Error::other("close detail"));
        assert_eq!(
            crate::error::Report(&error).to_string(),
            "catalog I/O: permission denied; catalog close also failed: close detail: disk detail"
        );
        assert_eq!(
            error
                .source()
                .unwrap()
                .downcast_ref::<io::Error>()
                .unwrap()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }
}
