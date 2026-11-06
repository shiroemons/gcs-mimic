#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Bounded, sorted source traversal shared by offline adapters.

pub(crate) mod watch;

use std::{
    fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

pub fn paths_overlap(left: &Path, right: &Path) -> io::Result<bool> {
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
                .unwrap_or(Path::new("."));
            let leaf = path.file_name().ok_or(error)?;
            Ok(fs::canonicalize(parent)?.join(leaf))
        }
        Err(error) => Err(error),
    }
}

pub fn is_os_metadata(name: &str) -> bool {
    name == ".DS_Store" || name.starts_with("._")
}

pub fn is_editor_temporary(name: &str) -> bool {
    name == "4913" || name.ends_with(".swp") || name.ends_with('~')
}

pub fn is_ignored_regular_file(name: &str) -> bool {
    is_os_metadata(name) || is_editor_temporary(name)
}

/// What a walked tree may hold. Cloud Storage documents no limit on objects or buckets, so a limit
/// exists only where an operator names one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TreeLimits {
    pub entries: Option<NonZeroUsize>,
    /// Directory levels. How they are counted belongs to each kind of tree.
    pub depth: Option<NonZeroUsize>,
}

/// Past this many entries, a tree that no limit stops gets one warning: it is allowed, but a tree
/// this large is often a mistaken path, and nothing else would show why startup takes so long.
pub(crate) const LARGE_TREE_ENTRIES: usize = 10_000;

/// Warns that a tree passed [`LARGE_TREE_ENTRIES`]; the caller makes sure it warns only once.
pub(crate) fn warn_large_tree(event: &'static str, directory: &str, limit: &str) {
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
pub enum WalkError {
    Io(io::Error),
    EntryLimit { limit: NonZeroUsize },
    DepthLimit { limit: NonZeroUsize },
    NotDirectory,
}

impl std::fmt::Display for WalkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::EntryLimit { .. } => f.write_str("source exceeds the configured entry limit"),
            Self::DepthLimit { .. } => f.write_str("source exceeds the configured directory depth"),
            Self::NotDirectory => f.write_str("source must be a directory, not a symlink"),
        }
    }
}

impl std::error::Error for WalkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => std::error::Error::source(error),
            _ => None,
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
pub struct Walker {
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
    pub fn new(root: &Path, limits: TreeLimits) -> Result<Self, WalkError> {
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
        walk.children(root, 0)?;
        Ok(walk)
    }

    fn children(&mut self, path: &Path, depth: usize) -> Result<(), WalkError> {
        let mut children = Vec::new();
        for entry in fs::read_dir(path)? {
            self.entries += 1;
            if let Some(limit) = self.limits.entries
                && self.entries > limit.get()
            {
                return Err(WalkError::EntryLimit { limit });
            }
            children.push(entry?.path());
        }
        children.sort();
        self.pending
            .extend(children.into_iter().rev().map(|path| (path, depth)));
        Ok(())
    }
}

impl Iterator for Walker {
    type Item = Result<(PathBuf, fs::Metadata, usize), WalkError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.started = true;
        if self.failed {
            return None;
        }
        let (path, depth) = self.pending.pop()?;
        let result = (|| {
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                if let Some(limit) = self.limits.depth
                    && depth > limit.get()
                {
                    return Err(WalkError::DepthLimit { limit });
                }
                self.children(&path, depth + 1)?;
            }
            Ok((path, metadata, depth))
        })();
        self.failed = result.is_err();
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
            .filter(|(_, _, depth)| *depth == 0)
            .map(|(path, _, _)| path)
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
        assert!(matches!(
            entries.last(),
            Some(Err(WalkError::EntryLimit { limit })) if limit.get() == 3
        ));
        let depth = walked(TreeLimits {
            entries: None,
            depth: limit(1),
        });
        assert!(matches!(
            depth.last(),
            Some(Err(WalkError::DepthLimit { limit })) if limit.get() == 1
        ));
    }
}
