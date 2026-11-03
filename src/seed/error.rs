use crate::{json::JsonError, storage::admin::AdminError};
use std::{error::Error, fmt, io, path::StripPrefixError};

#[derive(Debug)]
pub enum SeedError {
    Io(io::Error),
    Admin(AdminError),
    BucketName(AdminError),
    ObjectName(JsonError),
    RelativePath(StripPrefixError),
    EntryLimit,
    DepthLimit,
    NotDirectory,
    Overlap,
    WalkStopped,
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
            Self::Admin(error) | Self::BucketName(error) => error.fmt(f),
            Self::ObjectName(error) => f.write_str(&error.message),
            Self::RelativePath(error) => error.fmt(f),
            Self::EntryLimit => f.write_str("fixture exceeds 10000 entries"),
            Self::DepthLimit => f.write_str("fixture exceeds 32 directory levels below a bucket"),
            Self::NotDirectory => f.write_str("seed_dir must be a directory, not a symlink"),
            Self::Overlap => f.write_str("seed_dir and data_dir must not overlap"),
            Self::WalkStopped => f.write_str("seed directory walk stopped before it finished"),
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
            Self::Admin(error) | Self::BucketName(error) => error.source(),
            Self::ObjectName(error) => error.source(),
            Self::RelativePath(error) => error.source(),
            Self::Entry { source, .. } | Self::Import(source) => Some(source.as_ref()),
            _ => None,
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
