#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Bounded, sorted source traversal shared by offline adapters.

use std::{
    fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

pub(crate) fn paths_overlap(left: &Path, right: &Path) -> io::Result<bool> {
    let left = fs::canonicalize(left)?;
    let right = canonical_destination(right)?;
    Ok(paths_overlap_canonical(&left, &right))
}

pub(crate) fn paths_overlap_canonical(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn canonical_destination(path: &Path) -> io::Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let leaf = path.file_name().ok_or(error)?;
            Ok(fs::canonicalize(parent)?.join(leaf))
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn is_os_metadata(name: &str) -> bool {
    name == ".DS_Store" || name.starts_with("._")
}

pub(crate) fn is_editor_temporary(name: &str) -> bool {
    name == "4913" || name.ends_with(".swp") || name.ends_with('~')
}

pub(crate) fn is_ignored_regular_file(name: &str) -> bool {
    is_os_metadata(name) || is_editor_temporary(name)
}

/// What a walked tree may hold. Cloud Storage documents no limit on objects or buckets, so a limit
/// exists only where an operator names one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TreeLimits {
    pub(crate) entries: Option<NonZeroUsize>,
    /// Directory levels. How they are counted belongs to each kind of tree.
    pub(crate) depth: Option<NonZeroUsize>,
}

/// Past this many entries, a tree that no limit stops gets one warning: it is allowed, but a tree
/// this large is often a mistaken path, and nothing else would show why startup takes so long.
pub(crate) const LARGE_TREE_ENTRIES: usize = 10_000;

/// Warns that a tree passed [`LARGE_TREE_ENTRIES`]; the caller makes sure it warns only once.
pub(crate) fn warn_large_tree(event: crate::diagnostics::Event, directory: &str, limit: &str) {
    crate::diagnostics::emit(
        crate::diagnostics::LogLevel::Warn,
        event,
        serde_json::json!({
            "entries": LARGE_TREE_ENTRIES,
            "message": format!(
                "{directory} holds more than {LARGE_TREE_ENTRIES} files and directories and all of them are being processed; check that it names the intended directory, or set {limit} to stop at a chosen size"
            ),
        }),
    );
}

#[derive(Debug)]
pub(crate) enum WalkError {
    Io(io::Error),
    /// What the entry at `path` is, or what that directory lists, could not be read. Nothing
    /// below the entry is walked, and the walk goes on with the entries after it, so that a
    /// caller that tells an unreadable entry apart from an absent one still sees the rest.
    Entry {
        path: PathBuf,
        error: io::Error,
    },
    EntryLimit {
        limit: NonZeroUsize,
    },
    DepthLimit {
        limit: NonZeroUsize,
    },
    NotDirectory,
}

impl WalkError {
    /// This failure as one met listing the root, which is no entry of the walk: an entry that
    /// could not be read there is I/O that ends the walk.
    fn at_root(self) -> Self {
        match self {
            Self::Entry { error, .. } => Self::Io(error),
            error @ (Self::Io(_)
            | Self::EntryLimit { .. }
            | Self::DepthLimit { .. }
            | Self::NotDirectory) => error,
        }
    }

    /// Whether the walk stops at this failure: it goes on only past an entry it could not read.
    fn ends_walk(&self) -> bool {
        match self {
            Self::Entry { .. } => false,
            Self::Io(_)
            | Self::EntryLimit { .. }
            | Self::DepthLimit { .. }
            | Self::NotDirectory => true,
        }
    }
}

impl std::fmt::Display for WalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) | Self::Entry { error, .. } => error.fmt(f),
            Self::EntryLimit { .. } => f.write_str("source exceeds the configured entry limit"),
            Self::DepthLimit { .. } => f.write_str("source exceeds the configured directory depth"),
            Self::NotDirectory => f.write_str("source must be a directory, not a symlink"),
        }
    }
}

// Entry carries the path alongside the I/O error, but forwards the latter's source.
impl std::error::Error for WalkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) | Self::Entry { error, .. } => std::error::Error::source(error),
            Self::EntryLimit { .. } | Self::DepthLimit { .. } | Self::NotDirectory => None,
        }
    }
}

impl From<io::Error> for WalkError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Walks with an explicit stack and one open directory at a time, so depth costs neither call stack
/// nor descriptors. Paths are absolute, so a path past the platform's `PATH_MAX` fails as I/O.
pub(crate) struct Walker {
    pending: Vec<(PathBuf, usize)>,
    entries: usize,
    limits: TreeLimits,
    failed: bool,
    started: bool,
}

impl Walker {
    /// Call before the first `next()` to inspect the complete root snapshot.
    pub(crate) fn root_entries(&self) -> impl Iterator<Item = &Path> {
        debug_assert!(
            !self.started,
            "root snapshot must be inspected before walking"
        );
        self.pending
            .iter()
            .filter_map(|(path, depth)| (*depth == 0).then_some(path.as_path()))
    }
    pub(crate) fn new(root: &Path, limits: TreeLimits) -> Result<Self, WalkError> {
        if !fs::symlink_metadata(root)?.is_dir() {
            return Err(WalkError::NotDirectory);
        }
        let mut walk = Self {
            pending: Vec::new(),
            entries: 0,
            limits,
            failed: false,
            started: false,
        };
        // The root is the tree itself, so failing to list it ends the walk instead of skipping one
        // entry.
        walk.children(root, 0).map_err(WalkError::at_root)?;
        Ok(walk)
    }

    /// Queues the names `path` lists. A listing that fails queues none of them.
    fn children(&mut self, path: &Path, depth: usize) -> Result<(), WalkError> {
        let unread = |error| WalkError::Entry {
            path: path.to_owned(),
            error,
        };
        let mut children = Vec::new();
        for entry in fs::read_dir(path).map_err(unread)? {
            self.entries += 1;
            if let Some(limit) = self.limits.entries
                && self.entries > limit.get()
            {
                return Err(WalkError::EntryLimit { limit });
            }
            children.push(entry.map_err(unread)?.path());
        }
        children.sort();
        self.pending
            .extend(children.into_iter().rev().map(|path| (path, depth)));
        Ok(())
    }

    /// Reads one popped entry and queues a directory's children below it.
    fn visit(&mut self, path: PathBuf, depth: usize) -> Result<WalkEntry, WalkError> {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => return Err(WalkError::Entry { path, error }),
        };
        if metadata.is_dir() {
            if let Some(limit) = self.limits.depth
                && depth > limit.get()
            {
                return Err(WalkError::DepthLimit { limit });
            }
            self.children(&path, depth + 1)?;
        }
        Ok(WalkEntry {
            path,
            metadata,
            depth,
        })
    }
}

/// One entry of a walk: its absolute path, its metadata, and how many directories below the root it is.
#[derive(Debug)]
pub(crate) struct WalkEntry {
    pub(crate) path: PathBuf,
    pub(crate) metadata: fs::Metadata,
    pub(crate) depth: usize,
}

impl Iterator for Walker {
    type Item = Result<WalkEntry, WalkError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.started = true;
        if self.failed {
            return None;
        }
        let (path, depth) = self.pending.pop()?;
        let result = self.visit(path, depth);
        self.failed = result.as_ref().is_err_and(WalkError::ends_walk);
        Some(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_snapshot_is_shared_with_the_walk() {
        let directory = crate::test_support::TestDir::new();
        fs::create_dir(directory.0.join("abc")).unwrap();
        let walk = Walker::new(&directory.0, TreeLimits::default()).unwrap();
        let roots: Vec<_> = walk.root_entries().map(Path::to_owned).collect();
        fs::create_dir(directory.0.join("a%62c")).unwrap();
        assert_eq!(
            walk.root_entries().map(Path::to_owned).collect::<Vec<_>>(),
            roots
        );
        let observed: Vec<_> = walk
            .map(Result::unwrap)
            .filter(|entry| entry.depth == 0)
            .map(|entry| entry.path)
            .collect();
        assert_eq!(observed, roots);
    }

    #[test]
    fn a_walk_stops_only_past_a_named_limit() {
        let directory = crate::test_support::TestDir::new();
        fs::create_dir_all(directory.0.join("a/b/c")).unwrap();
        fs::write(directory.0.join("a/b/c/file"), b"").unwrap();
        let walked = |limits| {
            Walker::new(&directory.0, limits)
                .unwrap()
                .collect::<Vec<_>>()
        };
        let limit = |value| NonZeroUsize::new(value);
        assert_eq!(walked(TreeLimits::default()).len(), 4);
        let exact = TreeLimits {
            entries: limit(4),
            depth: limit(2),
        };
        assert!(walked(exact).iter().all(Result::is_ok));
        let entries = walked(TreeLimits {
            entries: limit(3),
            depth: None,
        });
        std::assert_matches!(
            entries.last(),
            Some(Err(WalkError::EntryLimit { limit })) if limit.get() == 3
        );
        let depth = walked(TreeLimits {
            entries: None,
            depth: limit(1),
        });
        std::assert_matches!(
            depth.last(),
            Some(Err(WalkError::DepthLimit { limit })) if limit.get() == 1
        );
    }

    #[test]
    fn os_detritus_and_paths_inside_the_source_are_recognized() {
        let directory = crate::test_support::TestDir::new();
        let source = directory.0.join("source");
        fs::create_dir_all(source.join("bucket-one")).unwrap();
        assert!(paths_overlap(&source, &source.join("new")).unwrap());
        assert!(!paths_overlap(&source, &directory.0.join("destination")).unwrap());
        assert!(is_os_metadata("._file") && is_editor_temporary(".file.swp"));
        assert!(is_ignored_regular_file("4913"));
        assert!(!is_os_metadata("normal"));
    }

    /// A directory that cannot be listed fails as its own entry, with nothing below it walked,
    /// and the walk goes on with the entries after it.
    #[cfg(unix)]
    #[test]
    fn a_walk_goes_on_past_a_directory_it_cannot_list() {
        use std::os::unix::fs::PermissionsExt;
        let directory = crate::test_support::TestDir::new();
        fs::create_dir_all(directory.0.join("a/locked")).unwrap();
        fs::write(directory.0.join("a/locked/hidden"), b"").unwrap();
        fs::write(directory.0.join("a/z-after"), b"").unwrap();
        let locked = directory.0.join("a/locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&locked).is_ok() {
            eprintln!(
                "SKIP: a mode 000 directory stays listable (for example when running as root)"
            );
            return;
        }
        let walked: Vec<_> = Walker::new(&directory.0, TreeLimits::default())
            .unwrap()
            .collect();
        // A root the walk cannot list is no entry it can go on past.
        let root = Walker::new(&locked, TreeLimits::default());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
        let [Ok(a), Err(WalkError::Entry { path, error }), Ok(after)] = walked.as_slice() else {
            panic!("{walked:?}");
        };
        assert_eq!(a.path, directory.0.join("a"));
        assert_eq!(*path, locked);
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(after.path, directory.0.join("a/z-after"));
        std::assert_matches!(root.as_ref().map(|_| ()), Err(WalkError::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied);
    }
}

#[cfg(test)]
#[test]
fn retains_the_underlying_diagnostic_source() {
    #[derive(Debug, thiserror::Error)]
    #[error("entry read failed")]
    struct ReadFailure(#[source] std::fmt::Error);
    let error = WalkError::Entry {
        path: PathBuf::from("entry"),
        error: io::Error::new(
            io::ErrorKind::PermissionDenied,
            ReadFailure(std::fmt::Error),
        ),
    };
    assert_eq!(error.to_string(), "entry read failed");
    assert!(
        std::error::Error::source(&error)
            .unwrap()
            .is::<std::fmt::Error>()
    );
}
