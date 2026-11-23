//! The seed walk: what each entry of the fixture tree is, read on a thread of its own.
//!
//! The walk never takes a name it could not read for an empty one. An entry it could not
//! inspect, a directory it could not list and a file that would not open are unknown; only a
//! descriptor-relative lookup (`fsutil::Lookup`), whose `fstatat` found nothing under the name,
//! proves a name absent. Every open retries a name that fails with `ENOENT` while `fstatat`
//! still finds it, within the lookup's bounded wait. Windows keeps the trusted, path-opened
//! source it had: nothing there proves a name absent.

use super::SeedError;
use crate::{
    api_error::ApiError,
    fsutil::Vacant,
    source_tree::{TreeLimits, WalkEntry, WalkError, Walker, is_os_metadata},
    storage::admin::{AdminError, BucketName, ObjectName, replay::Fingerprint},
};
use std::{
    ffi::OsStr,
    fs::{File, Metadata},
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};
use tokio::sync::mpsc;

#[cfg(unix)]
use crate::fsutil::NodeId;

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
    Bucket(BucketName),
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
    /// A path no longer names the directory the walk listed there: the root, or a directory a
    /// lookup descended through.
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "only the descriptor-relative lookup opens fixtures apart from the listing, and this platform has none"
        )
    )]
    Replaced,
}

/// A startup replay fails on an unknown entry with the I/O error, as it did before lookups
/// retried. A name that kept failing to open fails as [`SeedError::Unopenable`], which the
/// reseed route reports as a failure that another attempt may pass, as it did the `NotFound`
/// such an open failed with before. A replaced directory fails as [`SeedError::TreeChanged`], as
/// a root replaced before it was pinned does, which another attempt reads as the tree now is.
impl From<Unknown> for SeedError {
    fn from(unknown: Unknown) -> Self {
        match unknown {
            Unknown::Io(error) => Self::Io(error),
            Unknown::Unopenable => Self::Unopenable(crate::fsutil::Unopenable),
            Unknown::Replaced => Self::TreeChanged,
        }
    }
}

/// Why seeding cannot use an entry.
pub(super) enum Rejected {
    /// A symlink, which seeding never follows.
    Symlink,
    /// Neither a regular file nor a directory, such as a socket or a FIFO.
    Special,
    /// A name that is not UTF-8.
    NonUtf8Name,
    /// A path below a bucket that is no valid object name.
    ObjectName,
    /// A bucket directory whose name is no valid bucket name.
    BucketName(AdminError),
}

/// The failure a startup replay reports for an entry it cannot use.
impl From<Rejected> for SeedError {
    fn from(rejected: Rejected) -> Self {
        match rejected {
            Rejected::Symlink | Rejected::Special => Self::UnsupportedFile,
            Rejected::NonUtf8Name => Self::NonUtf8Name,
            // Startup and the reseed route report a fixed message, so the rule it broke is dropped.
            Rejected::ObjectName => {
                Self::ObjectName(ApiError::invalid_parameter("Invalid object name"))
            }
            Rejected::BucketName(error) => Self::BucketName(error),
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

/// A fixture file opened for reading, with what its handle reported.
pub(super) struct Opened {
    pub(super) file: File,
    pub(super) metadata: Metadata,
    /// What the handle reported when the walk opened the file, which describes the file this
    /// handle reads even where another replaced the one the walk inspected. It decides only
    /// whether a pass reads the file again; a record keeps the fingerprint of the read that
    /// staged its bytes, since the file may change between the two.
    #[cfg_attr(
        any(not(test), not(unix)),
        expect(
            dead_code,
            reason = "the pass while the server runs that decides from it whether to read a file again is not wired yet, and only the Unix walk tests read it"
        )
    )]
    pub(super) fingerprint: Fingerprint,
}

/// What the handle of an opened fixture file reports.
fn inspect(file: File) -> Source {
    let reported = fingerprint(&file).and_then(|fingerprint| Ok((fingerprint, file.metadata()?)));
    match reported {
        Ok((fingerprint, metadata)) => Source::File(Box::new(Opened {
            file,
            metadata,
            fingerprint,
        })),
        Err(error) => Source::Unknown(Unknown::Io(error)),
    }
}

/// The fingerprint of `file`, asked of its filesystem past any attribute cache, which a rename
/// made on a Docker Desktop host does not invalidate. The time is read from the system wall clock
/// before the stat, as [`Fingerprint::new`] requires.
fn fingerprint(file: &File) -> io::Result<Fingerprint> {
    let observed = SystemTime::now();
    Ok(Fingerprint::new(
        crate::fsutil::uncached_identity(file)?,
        observed,
    ))
}

/// The fixture tree below a canonical root, opened through the root's pinned descriptor on Unix.
///
/// The walk lists directories by path, and a lookup proves a name absent in the directory its
/// descriptor holds, so the two agree only while each path still names the directory a
/// descriptor holds. The root's path is compared with the pinned root before the walk lists it
/// and after the walk ends, and each directory a lookup descends through with what the walk
/// inspected at that path just before it listed it. Any difference makes the name, or the
/// whole walk for the root, unknown. A directory replaced between that inspection and its
/// listing, then put back before the lookup, still passes.
pub(super) struct Tree {
    root: PathBuf,
    #[cfg(unix)]
    pinned: std::sync::Arc<File>,
    /// One lookup for the whole walk, so that all its retries share one bounded wait.
    #[cfg(unix)]
    lookup: crate::fsutil::Lookup,
    /// The directories above the entry being walked, outermost first, as the walk inspected
    /// them before listing them.
    #[cfg(unix)]
    listed: Vec<NodeId>,
}

impl Tree {
    /// A tree whose lookups wait on `clock` for a name that fails to open for a moment.
    #[cfg(unix)]
    pub(super) fn new(
        root: PathBuf,
        pinned: std::sync::Arc<File>,
        clock: std::sync::Arc<dyn crate::clock::Clock>,
    ) -> Self {
        Self {
            root,
            pinned,
            lookup: crate::fsutil::Lookup::unprobed_on(clock),
            listed: Vec::new(),
        }
    }

    #[cfg(not(unix))]
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Walks the tree on a thread of its own, sending each entry over a two-slot channel.
    pub(super) fn walk(self, limits: TreeLimits) -> io::Result<mpsc::Receiver<Walked>> {
        let (send, receive) = mpsc::channel(2);
        crate::task::spawn_thread("seed-walk", move || self.send_entries(limits, &send))?;
        Ok(receive)
    }

    fn send_entries(mut self, limits: TreeLimits, send: &mpsc::Sender<Walked>) {
        let started = self
            .check_root()
            .map_err(SeedError::from)
            .and_then(|()| Ok(Walker::new(&self.root, limits)?));
        let walk = match started {
            Ok(walk) => walk,
            Err(error) => {
                let _ = send.blocking_send(Walked::Entry(Box::new(Err(error))));
                return;
            }
        };
        #[cfg(test)]
        let walk = {
            let root = self.root.clone();
            walk.enumerate().map(move |(index, entry)| {
                super::tests::injected_walk_panic(&root, index);
                entry
            })
        };
        for entry in walk {
            let seen = self.see(entry);
            if send.blocking_send(Walked::Entry(Box::new(seen))).is_err() {
                return;
            }
        }
        let end = match self.check_root() {
            Ok(()) => Walked::Done,
            Err(unknown) => Walked::Entry(Box::new(Err(unknown.into()))),
        };
        let _ = send.blocking_send(end);
    }

    /// Fails unless the root's path still names the pinned root, which the walk then lists.
    #[cfg(unix)]
    fn check_root(&self) -> Result<(), Unknown> {
        let listed = std::fs::symlink_metadata(&self.root).map_err(Unknown::Io)?;
        let pinned = self.pinned.metadata().map_err(Unknown::Io)?;
        if NodeId::from(&listed) == NodeId::from(&pinned) {
            Ok(())
        } else {
            Err(Unknown::Replaced)
        }
    }

    /// The walk opens fixtures by the paths it lists, so the two cannot disagree.
    #[cfg(not(unix))]
    #[expect(
        clippy::unused_self,
        clippy::unnecessary_wraps,
        reason = "the Unix walk compares the root's path with its pinned descriptor, which can fail, and both targets share one signature"
    )]
    fn check_root(&self) -> Result<(), Unknown> {
        Ok(())
    }

    /// What one walked entry is. Only a failure that ends the walk, such as a limit, is an error.
    fn see(&mut self, entry: Result<WalkEntry, WalkError>) -> Result<Seen, SeedError> {
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
        // A depth-first walk lists every directory above an entry before the entry.
        #[cfg(unix)]
        self.listed
            .truncate(relative.iter().count().saturating_sub(1));
        let fixture = match found {
            Ok((metadata, depth)) => {
                let fixture = self
                    .fixture(&path, relative, &metadata, depth)
                    .unwrap_or_else(Fixture::Unusable);
                #[cfg(unix)]
                if metadata.is_dir() {
                    self.listed.push(NodeId::from(&metadata));
                }
                fixture
            }
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
    /// type, the bucket component, a bucket directory's name, the object name, then the bucket
    /// name of an object. A bucket directory with an invalid name is unusable here, before a
    /// replay counts it against the bucket limit.
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
            .ok_or(Rejected::NonUtf8Name)?;
        let kind = metadata.file_type();
        if kind.is_symlink() {
            return Err(Rejected::Symlink);
        }
        if !kind.is_file() && !kind.is_dir() {
            return Err(Rejected::Special);
        }
        if kind.is_file() && is_os_metadata(leaf) {
            return Ok(Fixture::OsMetadata);
        }
        let mut components = relative.iter();
        let bucket = components
            .next()
            .and_then(OsStr::to_str)
            .ok_or(Rejected::NonUtf8Name)?;
        if depth == 0 {
            if kind.is_file() {
                return Ok(Fixture::TopLevelFile);
            }
            return BucketName::new(leaf)
                .map(Fixture::Bucket)
                .map_err(Rejected::BucketName);
        }
        let name = components
            .map(|part| part.to_str().ok_or(Rejected::NonUtf8Name))
            .collect::<Result<Vec<_>, _>>()?
            .join("/");
        let name = ObjectName::try_from(name).map_err(|_| Rejected::ObjectName)?;
        if kind.is_dir() {
            return Ok(Fixture::Directory);
        }
        let bucket = BucketName::new(bucket).map_err(Rejected::BucketName)?;
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
        self.open_file(relative).map_or_else(Source::from, inspect)
    }

    #[cfg(unix)]
    fn open_file(&self, relative: &Path) -> Result<File, Unopened> {
        let mut parent = None;
        let mut names = relative.iter().enumerate().peekable();
        while let Some((depth, name)) = names.next() {
            let directory = parent.as_ref().unwrap_or(self.pinned.as_ref());
            if names.peek().is_none() {
                return opened(self.lookup.regular_unconfirmed(directory, name));
            }
            let child = opened(self.lookup.directory_unconfirmed(directory, name))?;
            self.check_listed(depth, &child)?;
            parent = Some(child);
        }
        Err(Unknown::Io(io::ErrorKind::InvalidInput.into()).into())
    }

    /// Fails unless `directory`, opened `depth` levels below the root, is the directory the walk
    /// inspected at that path before it listed it: a lookup in any other proves nothing about a
    /// name the walk listed.
    #[cfg(unix)]
    fn check_listed(&self, depth: usize, directory: &File) -> Result<(), Unopened> {
        let opened = directory.metadata().map_err(Unknown::Io)?;
        if self.listed.get(depth) == Some(&NodeId::from(&opened)) {
            Ok(())
        } else {
            Err(Unknown::Replaced.into())
        }
    }

    /// Opens the regular file `relative` by its path below the trusted root.
    #[cfg(not(unix))]
    fn open(&self, relative: &Path) -> Source {
        File::open(self.root.join(relative))
            .map_or_else(|error| Source::Unknown(Unknown::Io(error)), inspect)
    }
}

/// What a fixture path that did not open is to the replay.
#[cfg(unix)]
enum Unopened {
    /// A lookup proved a name on the path empty.
    Absent(Vacant),
    Unknown(Unknown),
}

#[cfg(unix)]
impl From<Unknown> for Unopened {
    fn from(unknown: Unknown) -> Self {
        Self::Unknown(unknown)
    }
}

#[cfg(unix)]
impl From<Unopened> for Source {
    fn from(unopened: Unopened) -> Self {
        match unopened {
            Unopened::Absent(vacant) => Self::Absent(vacant),
            Unopened::Unknown(unknown) => Self::Unknown(unknown),
        }
    }
}

/// The entry a lookup opened, or what a name that did not open is to the replay.
#[cfg(unix)]
fn opened(opened: io::Result<crate::fsutil::Opened<File>>) -> Result<File, Unopened> {
    match opened {
        Ok(crate::fsutil::Opened::Entry(file)) => Ok(file),
        Ok(crate::fsutil::Opened::Missing(vacant)) => Err(Unopened::Absent(vacant)),
        Ok(crate::fsutil::Opened::Unopenable) => Err(Unknown::Unopenable.into()),
        Err(error) => Err(Unknown::Io(error).into()),
    }
}

#[cfg(test)]
mod tests;
