//! The seed walk: what each entry of the fixture tree is, read on a thread of its own.
//!
//! The walk never takes a name it could not read for an empty one. An entry it could not
//! inspect, a directory it could not list and a file that would not open are unknown; only a
//! descriptor-relative lookup (`fsutil::Lookup`), whose `fstatat` found nothing under the name,
//! proves a name absent. Every open retries a name that fails with `ENOENT` while `fstatat`
//! still finds it, within the lookup's bounded wait. Windows keeps the trusted, path-opened
//! source it had: nothing there proves a name absent.

use super::{SeedError, decision::Unusable};
use crate::{
    api_error::ApiError,
    fsutil::Vacant,
    import::Fingerprint,
    source_tree::{TreeLimits, WalkEntry, WalkError, Walker, is_os_metadata},
    storage::admin::{AdminError, BucketName, ObjectName},
};
use std::{
    ffi::OsStr,
    fs::{File, Metadata},
    io,
    path::{Path, PathBuf},
};
use tokio::sync::mpsc;

/// One message from the walk thread. The walk ends only with [`Walked::Done`]: a thread that
/// panicked or stopped early drops its sender without one, and a closed channel alone would
/// otherwise look like the end of a complete walk and mark a partial seed complete. The entry is
/// boxed because Linux `Metadata` carries statx fields that make an unboxed variant over 200 bytes.
pub(super) enum Walked {
    Entry(Box<Result<Seen, SeedError>>),
    Done,
}

/// One walked entry.
pub(super) struct Seen {
    /// The entry's path below the root, which diagnostics name it by.
    pub(super) label: String,
    pub(super) fixture: Fixture,
}

/// What one walked entry is, decided once from its depth, type and names.
pub(super) enum Fixture {
    /// What the entry is, or what it lists as a directory, could not be read.
    Unknown(Unknown),
    /// The entry was listed, then a lookup proved its name empty.
    Absent(Vacant),
    /// An entry seeding cannot use.
    Unusable(Rejected),
    /// A file macOS or Windows leaves beside fixtures; it is skipped and counted.
    OsMetadata,
    /// A top-level file, which fake-gcs-server ignores; it is skipped with a warning.
    TopLevelFile,
    /// A top-level directory names a bucket.
    Bucket(String),
    /// A directory below a bucket. Its path is a valid object name.
    Directory,
    /// A regular file below a bucket names an object.
    Object(Box<Object>),
}

/// Why an entry's name could not tell whether it holds anything.
#[derive(Debug)]
pub(super) enum Unknown {
    /// Inspecting, listing or opening it failed with an error other than finding nothing.
    Io(io::Error),
    /// It kept failing to open while `fstatat` still found an entry under its name
    /// ([`crate::fsutil::Opened::Unopenable`]).
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "only the descriptor-relative lookup retries an open, and this platform has none"
        )
    )]
    Unopenable,
}

/// A startup replay fails on an unknown entry with the I/O error, as it did before lookups
/// retried. A name that kept failing to open carries [`crate::fsutil::Unopenable`] as a backend
/// cause of its own, which the reseed route reports as a failure that another attempt may pass,
/// as it did the `NotFound` such an open failed with before.
impl From<Unknown> for SeedError {
    fn from(unknown: Unknown) -> Self {
        match unknown {
            Unknown::Io(error) => Self::Io(error),
            Unknown::Unopenable => Self::Admin(AdminError::backend(crate::fsutil::Unopenable)),
        }
    }
}

/// An entry seeding cannot use: why, and the failure a startup replay reports for it.
pub(super) struct Rejected {
    pub(super) reason: Unusable,
    pub(super) error: SeedError,
}

impl Rejected {
    fn non_utf8() -> Self {
        Self {
            reason: Unusable::NonUtf8Name,
            error: SeedError::NonUtf8Name,
        }
    }
}

/// A regular file below a bucket directory.
pub(super) struct Object {
    pub(super) bucket: BucketName,
    pub(super) name: ObjectName,
    /// The last path component, which selects the content type.
    pub(super) leaf: String,
    /// What opening the file found.
    pub(super) source: Source,
}

/// What opening a fixture file found. The opened file is boxed because Linux `Metadata` carries
/// statx fields that make an unboxed variant over 200 bytes.
pub(super) enum Source {
    File(Box<Opened>),
    /// A lookup proved the name empty.
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "only the descriptor-relative lookup proves a name absent, and this platform has none"
        )
    )]
    Absent(Vacant),
    Unknown(Unknown),
}

/// A fixture file opened for reading, with what its handle reported. The fingerprint describes
/// the file this handle reads, even where another replaced the one the walk inspected.
pub(super) struct Opened {
    pub(super) file: File,
    pub(super) metadata: Metadata,
    pub(super) fingerprint: Fingerprint,
}

/// What the handle of an opened fixture file reports.
fn inspect(file: File) -> Source {
    let reported = file
        .metadata()
        .and_then(|metadata| Ok((Fingerprint::of_file(&file)?, metadata)));
    match reported {
        Ok((fingerprint, metadata)) => Source::File(Box::new(Opened {
            file,
            metadata,
            fingerprint,
        })),
        Err(error) => Source::Unknown(Unknown::Io(error)),
    }
}

/// The fixture tree below a canonical root, opened through the root's pinned descriptor on Unix.
pub(super) struct Tree {
    root: PathBuf,
    #[cfg(unix)]
    pinned: std::sync::Arc<File>,
    /// One lookup for the whole walk, so that all its retries share one bounded wait.
    #[cfg(unix)]
    lookup: crate::fsutil::Lookup,
}

impl Tree {
    #[cfg(unix)]
    pub(super) fn new(root: PathBuf, pinned: std::sync::Arc<File>) -> Self {
        Self {
            root,
            pinned,
            lookup: crate::fsutil::Lookup::unprobed(),
        }
    }

    #[cfg(not(unix))]
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Walks the tree on a thread of its own, sending each entry over a two-slot channel.
    pub(super) fn walk(self, limits: TreeLimits) -> io::Result<mpsc::Receiver<Walked>> {
        let (send, receive) = mpsc::channel(2);
        crate::task::spawn_thread("seed-walk", move || {
            let walk = match Walker::new(&self.root, limits) {
                Ok(walk) => walk,
                Err(error) => {
                    let _ = send.blocking_send(Walked::Entry(Box::new(Err(error.into()))));
                    return;
                }
            };
            #[cfg(test)]
            let walk = walk.enumerate().map(|(index, entry)| {
                super::tests::injected_walk_panic(&self.root, index);
                entry
            });
            for entry in walk {
                let seen = self.see(entry);
                if send.blocking_send(Walked::Entry(Box::new(seen))).is_err() {
                    return;
                }
            }
            let _ = send.blocking_send(Walked::Done);
        })?;
        Ok(receive)
    }

    /// What one walked entry is. Only a failure that ends the walk, such as a limit, is an error.
    fn see(&self, entry: Result<WalkEntry, WalkError>) -> Result<Seen, SeedError> {
        let (path, found) = match entry {
            Ok(WalkEntry {
                path,
                metadata,
                depth,
            }) => (path, Ok((metadata, depth))),
            Err(WalkError::Entry { path, error }) => (path, Err(error)),
            Err(error) => return Err(error.into()),
        };
        // The walker only yields paths below the root it was given.
        let relative = path
            .strip_prefix(&self.root)
            .map_err(SeedError::RelativePath)?;
        let fixture = match found {
            Ok((metadata, depth)) => self
                .fixture(&path, relative, &metadata, depth)
                .unwrap_or_else(Fixture::Unusable),
            Err(error) => self.unread(relative, error),
        };
        Ok(Seen {
            label: relative.display().to_string(),
            fixture,
        })
    }

    /// What an entry the walk could not inspect or list is: absent only when its inspection
    /// found nothing and a lookup proves it, and unknown otherwise.
    fn unread(&self, relative: &Path, error: io::Error) -> Fixture {
        if error.kind() == io::ErrorKind::NotFound
            && let Source::Absent(vacant) = self.open(relative)
        {
            return Fixture::Absent(vacant);
        }
        Fixture::Unknown(Unknown::Io(error))
    }

    /// Classifies an inspected entry. Its checks fail in a fixed order: the leaf name, the entry
    /// type, the bucket component, the object name, then the bucket name of an object.
    fn fixture(
        &self,
        path: &Path,
        relative: &Path,
        metadata: &Metadata,
        depth: usize,
    ) -> Result<Fixture, Rejected> {
        let leaf = path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(Rejected::non_utf8)?;
        let kind = metadata.file_type();
        let unsupported = if kind.is_symlink() {
            Some(Unusable::Symlink)
        } else {
            (!kind.is_file() && !kind.is_dir()).then_some(Unusable::Special)
        };
        if let Some(reason) = unsupported {
            return Err(Rejected {
                reason,
                error: SeedError::UnsupportedFile,
            });
        }
        if kind.is_file() && is_os_metadata(leaf) {
            return Ok(Fixture::OsMetadata);
        }
        let mut components = relative.iter();
        let bucket = components
            .next()
            .and_then(OsStr::to_str)
            .ok_or_else(Rejected::non_utf8)?;
        if depth == 0 {
            return Ok(if kind.is_dir() {
                Fixture::Bucket(leaf.to_owned())
            } else {
                Fixture::TopLevelFile
            });
        }
        let name = components
            .map(|part| part.to_str().ok_or_else(Rejected::non_utf8))
            .collect::<Result<Vec<_>, _>>()?
            .join("/");
        // Startup and the reseed route report a fixed message, so the rule it broke is dropped.
        let name = ObjectName::try_from(name).map_err(|_| Rejected {
            reason: Unusable::InvalidName,
            error: SeedError::ObjectName(ApiError::invalid_parameter("Invalid object name")),
        })?;
        if kind.is_dir() {
            return Ok(Fixture::Directory);
        }
        let bucket = BucketName::new(bucket).map_err(|error| Rejected {
            reason: Unusable::InvalidName,
            error: SeedError::BucketName(error),
        })?;
        #[cfg(test)]
        super::tests::injected_source_swap(path);
        Ok(Fixture::Object(Box::new(Object {
            bucket,
            name,
            leaf: leaf.to_owned(),
            source: self.open(relative),
        })))
    }

    /// Opens the regular file `relative` through the pinned root without following a symlink
    /// at any level, retrying a name that fails with `ENOENT` while `fstatat` still finds it.
    #[cfg(unix)]
    fn open(&self, relative: &Path) -> Source {
        self.open_file(relative)
            .map_or_else(|source| source, inspect)
    }

    #[cfg(unix)]
    fn open_file(&self, relative: &Path) -> Result<File, Source> {
        let mut parent = None;
        let mut names = relative.iter().peekable();
        while let Some(name) = names.next() {
            let directory = parent.as_ref().unwrap_or(self.pinned.as_ref());
            if names.peek().is_none() {
                return opened(self.lookup.regular_unconfirmed(directory, name));
            }
            parent = Some(opened(self.lookup.directory_unconfirmed(directory, name))?);
        }
        Err(Source::Unknown(Unknown::Io(
            io::ErrorKind::InvalidInput.into(),
        )))
    }

    /// Opens the regular file `relative` by its path below the trusted root.
    #[cfg(not(unix))]
    fn open(&self, relative: &Path) -> Source {
        File::open(self.root.join(relative))
            .map_or_else(|error| Source::Unknown(Unknown::Io(error)), inspect)
    }
}

/// The entry a lookup opened, or what a name that did not open is to the replay.
#[cfg(unix)]
fn opened(opened: io::Result<crate::fsutil::Opened<File>>) -> Result<File, Source> {
    match opened {
        Ok(crate::fsutil::Opened::Entry(file)) => Ok(file),
        Ok(crate::fsutil::Opened::Missing(vacant)) => Err(Source::Absent(vacant)),
        Ok(crate::fsutil::Opened::Unopenable) => Err(Source::Unknown(Unknown::Unopenable)),
        Err(error) => Err(Source::Unknown(Unknown::Io(error))),
    }
}

#[cfg(test)]
mod tests;
