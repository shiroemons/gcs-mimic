use crate::{api_error::ApiError, fsutil::Unopenable, storage::admin::AdminError};
use std::{error::Error, fmt, io, path::StripPrefixError};

#[derive(Debug)]
pub enum SeedError {
    Io(io::Error),
    Unopenable,
    Admin(AdminError),
    BucketName(AdminError),
    ObjectName(ApiError),
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
            Self::ObjectName(error) => f.write_str(&error.message),
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
    fn object_name_messages_keep_the_json_failure_in_the_cause_chain() {
        let error = SeedError::ObjectName(crate::json::validate_object_name("").unwrap_err());
        let cause = error.source().unwrap().downcast_ref::<ApiError>().unwrap();
        assert_eq!(error.to_string(), cause.message);
    }

    #[test]
    fn an_unopenable_fixture_keeps_why_in_the_cause_chain() {
        let cause = SeedError::Unopenable.source().unwrap();
        assert!(cause.is::<Unopenable>());
        assert_eq!(cause.to_string(), Unopenable.to_string());
    }
}
