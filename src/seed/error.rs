use crate::{
    fsutil::{IdentityField, Unopenable},
    storage::admin::{AdminError, InvalidObjectName},
};
use std::{error::Error, fmt, io, path::StripPrefixError};

#[derive(Debug)]
pub enum SeedError {
    Io(io::Error),
    Unopenable,
    Admin(AdminError),
    BucketName(AdminError),
    ObjectName(InvalidObjectName),
    RelativePath(StripPrefixError),
    EntryLimit { limit: usize },
    DepthLimit { limit: usize },
    BucketLimit { limit: usize },
    NotDirectory,
    Overlap,
    WalkStopped,
    TreeChanged,
    NonUtf8Name,
    UnsupportedFile,
    FileLimit,
    MissingRoot,
    Entry { label: String, source: Box<Self> },
    Import(Box<Self>),
}

impl fmt::Display for SeedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Unopenable => f.write_str("fixture could not be opened"),
            Self::Admin(error) | Self::BucketName(error) => error.fmt(f),
            Self::ObjectName(_) => f.write_str("Invalid object name"),
            Self::RelativePath(error) => error.fmt(f),
            Self::EntryLimit { limit } => write!(
                f,
                "fixture exceeds {limit} entries; raise max_seed_entries or reduce the tree"
            ),
            Self::DepthLimit { limit } => write!(
                f,
                "fixture exceeds {limit} directory levels below a bucket; raise max_seed_depth or flatten the tree"
            ),
            Self::BucketLimit { limit } => write!(
                f,
                "fixture exceeds {limit} buckets; raise max_buckets_per_batch or reduce the tree"
            ),
            Self::NotDirectory => f.write_str("seed_dir must be a directory, not a symlink"),
            Self::Overlap => f.write_str("seed_dir and data_dir must not overlap"),
            Self::WalkStopped => f.write_str("seed directory walk stopped before it finished"),
            Self::TreeChanged => {
                f.write_str("a seed directory was replaced while the walk read the tree")
            }
            Self::NonUtf8Name => f.write_str("fixture names must be UTF-8"),
            Self::UnsupportedFile => {
                f.write_str("only regular files and directories are supported")
            }
            Self::FileLimit => f.write_str("seed file is not regular or exceeds max_object_bytes"),
            Self::MissingRoot => f.write_str("reseed requires a configured seed_dir"),
            Self::Entry { label, source } => write!(f, "{label}: {source}"),
            Self::Import(source) => write!(f, "seed import: {source}"),
        }
    }
}

impl SeedError {
    pub(crate) fn failed_pass_diagnostic(&self) -> String {
        crate::error::Report(self).to_string()
    }

    /// This error as a pass report states it, in `problems[].reason` and `error`.
    pub(crate) const fn reason(&self) -> Reason<'_> {
        Reason(self)
    }
}

/// A [`SeedError`] in a sentence. Its own text names a failure of the catalog as the variant of
/// [`AdminError`], which startup diagnostics keep; a report a script reads says what happened.
pub(crate) struct Reason<'a>(&'a SeedError);

impl fmt::Display for Reason<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            SeedError::Admin(error) | SeedError::BucketName(error) => admin_reason(error, f),
            SeedError::Entry { label, source } => write!(f, "{label}: {}", source.reason()),
            SeedError::Import(source) => write!(f, "seed import: {}", source.reason()),
            SeedError::Io(_)
            | SeedError::Unopenable
            | SeedError::ObjectName(_)
            | SeedError::RelativePath(_)
            | SeedError::EntryLimit { .. }
            | SeedError::DepthLimit { .. }
            | SeedError::BucketLimit { .. }
            | SeedError::NotDirectory
            | SeedError::Overlap
            | SeedError::WalkStopped
            | SeedError::TreeChanged
            | SeedError::NonUtf8Name
            | SeedError::UnsupportedFile
            | SeedError::FileLimit
            | SeedError::MissingRoot => self.0.fmt(f),
        }
    }
}

fn admin_reason(error: &AdminError, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match error {
        AdminError::Busy { .. } => f.write_str("the catalog stayed busy"),
        AdminError::DiskFull(_) => f.write_str("the disk under data_dir is full"),
        AdminError::Closed => f.write_str("the server is closing"),
        AdminError::Precondition => f.write_str("the object changed meanwhile"),
        AdminError::Hold => f.write_str("a hold or retention period protects the object"),
        AdminError::SeedProjectMismatch => {
            f.write_str("a bucket of another project has the directory's name")
        }
        AdminError::SeedMetadataBudget { mib } => {
            write!(f, "seed metadata exceeds {mib} MiB; raise max_seed_entries")
        }
        AdminError::SourceChanged { fields } if fields.is_empty() => {
            f.write_str("the file changed while it was read")
        }
        AdminError::SourceChanged { fields } => {
            f.write_str("the file changed while it was read: its ")?;
            for (index, field) in fields.iter().enumerate() {
                if index > 0 {
                    f.write_str(", ")?;
                }
                f.write_str(match field {
                    IdentityField::Device => "device",
                    IdentityField::Inode => "inode",
                    IdentityField::Size => "size",
                    IdentityField::Mtime => "modification time",
                    IdentityField::Ctime => "status change time",
                })?;
            }
            f.write_str(" changed")
        }
        AdminError::Corrupt(cause) => write!(f, "the catalog is corrupt: {cause}"),
        AdminError::Io(cause) => write!(f, "an I/O operation failed: {cause}"),
        AdminError::NotFound => f.write_str("the bucket or object is gone"),
        AdminError::Invalid(reason) => f.write_str(reason),
        AdminError::Checksum => f.write_str("the bytes read do not match their checksum"),
        AdminError::TooLarge => f.write_str("the file exceeds max_object_bytes"),
        AdminError::Forbidden => f.write_str("the write is not authorized"),
        AdminError::BodyAborted(cause) => write!(f, "reading the file stopped: {cause}"),
        AdminError::Backend(cause) => write!(f, "the catalog failed: {cause}"),
    }
}

// Entry and Import expose the SeedError inside Box<Self>, and Unopenable exposes a static cause.
// Deriving Error would expose the box itself and omit the unit variant's cause.
impl Error for SeedError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => error.source(),
            // The cause carries no data, so a static one keeps it in the chain without making
            // the crate-private type part of this public error.
            Self::Unopenable => Some(&Unopenable),
            Self::Admin(error) | Self::BucketName(error) => error.source(),
            Self::ObjectName(error) => Some(error),
            Self::RelativePath(error) => error.source(),
            Self::Entry { source, .. } | Self::Import(source) => Some(source.as_ref()),
            Self::EntryLimit { .. }
            | Self::DepthLimit { .. }
            | Self::BucketLimit { .. }
            | Self::NotDirectory
            | Self::Overlap
            | Self::WalkStopped
            | Self::TreeChanged
            | Self::NonUtf8Name
            | Self::UnsupportedFile
            | Self::FileLimit
            | Self::MissingRoot => None,
        }
    }
}

impl From<io::Error> for SeedError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<AdminError> for SeedError {
    fn from(error: AdminError) -> Self {
        Self::Admin(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_name_messages_keep_the_rejected_rule_in_the_cause_chain() {
        let error = SeedError::ObjectName(crate::object::name::validate("").unwrap_err());
        crate::error::assert_distinct_causes(&error);
        assert_eq!(error.to_string(), "Invalid object name");
        assert_eq!(
            error.source().unwrap().downcast_ref::<InvalidObjectName>(),
            Some(&InvalidObjectName::Empty)
        );
    }

    /// A pass report states a failure of the catalog in words, while the error's own text keeps
    /// the variant that startup diagnostics print.
    #[test]
    fn a_pass_report_states_a_catalog_failure_in_words() {
        let changed = SeedError::Admin(AdminError::SourceChanged {
            fields: vec![IdentityField::Size, IdentityField::Mtime],
        });
        assert_eq!(
            changed.reason().to_string(),
            "the file changed while it was read: its size, modification time changed"
        );
        assert!(
            changed
                .to_string()
                .starts_with("administrative operation: SourceChanged"),
            "{changed}"
        );
        let entry = SeedError::Entry {
            label: "bucket/a".to_owned(),
            source: Box::new(SeedError::Admin(AdminError::Invalid("bad name".into()))),
        };
        assert_eq!(entry.reason().to_string(), "bucket/a: bad name");
        assert_eq!(
            SeedError::FileLimit.reason().to_string(),
            SeedError::FileLimit.to_string()
        );
        for error in [
            AdminError::Busy {
                retry_after: std::time::Duration::from_secs(2),
            },
            AdminError::DiskFull(io::ErrorKind::StorageFull.into()),
            AdminError::Closed,
            AdminError::Precondition,
            AdminError::Hold,
            AdminError::SeedProjectMismatch,
            AdminError::SeedMetadataBudget { mib: 1 },
            AdminError::SourceChanged { fields: Vec::new() },
            AdminError::Corrupt("row".into()),
            AdminError::Io(io::ErrorKind::PermissionDenied.into()),
            AdminError::NotFound,
            AdminError::Checksum,
            AdminError::TooLarge,
            AdminError::Forbidden,
            AdminError::BodyAborted("body".into()),
            AdminError::Backend("database".into()),
        ] {
            let reason = SeedError::Admin(error).reason().to_string();
            assert!(
                !reason.contains(['{', '(', '"']) && !reason.starts_with("administrative"),
                "{reason}"
            );
        }
    }

    #[test]
    fn an_unopenable_fixture_keeps_why_in_the_cause_chain() {
        let cause = SeedError::Unopenable.source().unwrap();
        assert!(cause.is::<Unopenable>());
        assert_eq!(cause.to_string(), Unopenable.to_string());
    }
}
