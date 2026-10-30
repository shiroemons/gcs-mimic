#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Bounded, sorted source traversal shared by offline adapters.

use std::{
    fs, io,
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

#[derive(Debug)]
pub enum WalkError {
    Io(io::Error),
    EntryLimit,
    DepthLimit,
    NotDirectory,
}

impl From<io::Error> for WalkError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct Walker {
    pending: Vec<(PathBuf, usize)>,
    entries: usize,
    max_entries: usize,
    max_depth: usize,
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
    pub fn new(root: &Path, max_entries: usize, max_depth: usize) -> Result<Self, WalkError> {
        if !fs::symlink_metadata(root)?.is_dir() {
            return Err(WalkError::NotDirectory);
        }
        let mut walk = Self {
            pending: Vec::new(),
            entries: 0,
            max_entries,
            max_depth,
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
            if self.entries > self.max_entries {
                return Err(WalkError::EntryLimit);
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
                if depth > self.max_depth {
                    return Err(WalkError::DepthLimit);
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
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    #[test]
    fn root_snapshot_is_shared_with_the_walk() {
        let directory = crate::fsutil::test_support::SourceDir::new();
        fs::create_dir(directory.0.join("abc")).unwrap();
        let walk = Walker::new(&directory.0, 8, 2).unwrap();
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
}
