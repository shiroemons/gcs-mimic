//! Descriptor-relative traversal shared by synchronization and catalog write-back.

use super::mapping::Reason;
use crate::fsutil::{self, Child, DirectoryOpen};
pub(crate) use crate::source_tree::TreeLimits;
use crate::source_tree::{LARGE_TREE_ENTRIES, warn_large_tree};
use crate::{config::MAX_TREE_DEPTH, fsutil::FileIdentity, storage::admin::AdminError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{collections::VecDeque, ffi::OsString, path::PathBuf};
use std::{fs::File, io, path::Path};
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

/// Rescans repeat for as long as the server runs, and one warning says all there is to say.
static WARNED_LARGE_TREE: AtomicBool = AtomicBool::new(false);

/// Every walker of a synchronized tree applies the same numbers, because a tree that one of them
/// accepts and another refuses would stop the root only after it was exported. `depth` counts
/// directory levels below the root, the bucket included.
impl TreeLimits {
    /// Whether a file at this root-relative path lies within the depth limit.
    pub(crate) fn admits_file(self, path: &Path) -> bool {
        self.depth
            .is_none_or(|depth| path.iter().count() <= depth.get().saturating_add(1))
    }

    /// The names one directory can hold in a tree that fits. Reserved root entries are not
    /// entries of the tree, so they must not take a place from one.
    pub(crate) fn directory_names(self) -> usize {
        self.entries.map_or(usize::MAX, |entries| {
            entries
                .get()
                .saturating_add(super::mapping::RESERVED_ROOT_ENTRIES)
        })
    }

    /// The deepest directory a walk that holds one descriptor per level descends to. No file below
    /// `MAX_TREE_DEPTH` can name an object, so stopping there bounds those descriptors without
    /// refusing anything that could have synchronized.
    pub(crate) fn deepest_directory(self) -> usize {
        self.depth.map_or(MAX_TREE_DEPTH, |depth| depth.get())
    }

    pub(crate) fn too_many_entries(self) -> AdminError {
        AdminError::Invalid(match self.entries {
            Some(entries) => format!(
                "sync tree exceeds {entries} inspected entries; preserve the tree, then raise max_sync_entries or reduce its size before rescanning"
            ),
            // Without an entry limit, only a name no filesystem should hold reaches this.
            None => "sync tree holds a directory entry name past 4096 bytes; preserve the tree, then rename or remove that entry before rescanning".to_owned(),
        })
    }

    pub(crate) fn too_deep(self) -> AdminError {
        AdminError::Invalid(match self.depth {
            Some(depth) => {
                format!(
                    "sync tree exceeds {depth} levels; raise max_sync_depth or reduce its depth"
                )
            }
            None => format!(
                "sync tree exceeds {MAX_TREE_DEPTH} levels, deeper than any object name can nest; reduce its depth"
            ),
        })
    }
}

pub(crate) struct TreeScan {
    limits: TreeLimits,
    frames: Vec<Frame>,
    pub(crate) visited: usize,
}
struct Frame {
    path: PathBuf,
    identity: Option<(u64, u64)>,
    after: Option<OsString>,
    buffered: VecDeque<OsString>,
    exhausted: bool,
}
pub(crate) struct TreeEntry {
    pub(crate) path: PathBuf,
    pub(crate) identity: Option<FileIdentity>,
    pub(crate) directory: bool,
    pub(crate) error: Option<io::ErrorKind>,
}
fn identity(file: &File) -> Result<FileIdentity, AdminError> {
    fsutil::stat_identity(file).map_err(|error| match error {
        fsutil::FsError::Io(error) => error.into(),
        fsutil::FsError::Unsupported => AdminError::Io(io::ErrorKind::Unsupported.into()),
    })
}
impl Frame {
    fn new(path: PathBuf, identity: Option<(u64, u64)>) -> Self {
        Self {
            path,
            identity,
            after: None,
            buffered: VecDeque::new(),
            exhausted: false,
        }
    }
}
impl TreeScan {
    pub(crate) fn new(limits: TreeLimits) -> Self {
        Self {
            limits,
            frames: vec![Frame::new(PathBuf::new(), None)],
            visited: 0,
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
    pub(crate) fn skip(&mut self, path: &Path) {
        self.frames.retain(|frame| !frame.path.starts_with(path));
    }

    fn reopen(
        &mut self,
        root: &File,
        entries: &mut Vec<TreeEntry>,
    ) -> Result<Vec<File>, AdminError> {
        let mut directories: Vec<File> = Vec::with_capacity(self.frames.len());
        for (index, frame) in self.frames.iter_mut().enumerate() {
            let opened = if index == 0 {
                root.try_clone()
            } else {
                let directory = directories.last().ok_or(AdminError::Precondition)?;
                match fsutil::child_directory(
                    directory,
                    frame.path.file_name().ok_or(AdminError::Precondition)?,
                    DirectoryOpen::Existing,
                ) {
                    Ok(Child::Directory { file, .. }) => Ok(file),
                    Ok(_) => Err(io::ErrorKind::ResourceBusy.into()),
                    Err(error) => Err(error),
                }
            };
            let file = match opened {
                Ok(file) => file,
                Err(error) => {
                    entries.push(TreeEntry {
                        path: frame.path.clone(),
                        identity: None,
                        directory: true,
                        error: Some(error.kind()),
                    });
                    self.frames.truncate(index);
                    break;
                }
            };
            let stat = identity(&file)?;
            if frame
                .identity
                .is_some_and(|expected| expected != (stat.device, stat.inode))
            {
                entries.push(TreeEntry {
                    path: frame.path.clone(),
                    identity: None,
                    directory: true,
                    error: Some(io::ErrorKind::ResourceBusy),
                });
                self.frames.truncate(index);
                break;
            }
            frame.identity = Some((stat.device, stat.inode));
            directories.push(file);
        }
        Ok(directories)
    }

    pub(crate) fn page(mut self, root: &File) -> Result<(Self, Vec<TreeEntry>), AdminError> {
        let mut entries = Vec::new();
        let mut directories = self.reopen(root, &mut entries)?;
        while entries.len() < 256 {
            let Some(frame) = self.frames.last_mut() else {
                break;
            };
            let directory = directories.last().ok_or(AdminError::Precondition)?;
            if frame.buffered.is_empty() && !frame.exhausted {
                match fsutil::directory_names_within(
                    directory,
                    frame.after.as_deref(),
                    256,
                    self.limits.directory_names(),
                ) {
                    Ok(page) => {
                        frame.exhausted = page.len() < 256;
                        frame.after = page.last().cloned().or(frame.after.take());
                        frame.buffered = page.into();
                    }
                    Err(error) if error.kind() != io::ErrorKind::InvalidData => {
                        entries.push(TreeEntry {
                            path: frame.path.clone(),
                            identity: None,
                            directory: true,
                            error: Some(error.kind()),
                        });
                        self.frames.pop();
                        directories.pop();
                        continue;
                    }
                    Err(_) => return Err(self.limits.too_many_entries()),
                }
            }
            let Some(name) = frame.buffered.pop_front() else {
                self.frames.pop();
                directories.pop();
                continue;
            };
            if frame.path.as_os_str().is_empty() && super::mapping::is_reserved_root_entry(&name) {
                continue;
            }
            self.visited += 1;
            match self.limits.entries {
                Some(limit) if self.visited > limit.get() => {
                    return Err(self.limits.too_many_entries());
                }
                // A chosen limit already says how large a tree may be.
                Some(_) => {}
                None => {
                    if self.visited > LARGE_TREE_ENTRIES
                        && !WARNED_LARGE_TREE.swap(true, Ordering::Relaxed)
                    {
                        warn_large_tree("sync_tree_large", "sync_dir", "max_sync_entries");
                    }
                }
            }
            let path = frame.path.join(&name);
            let mut entry = TreeEntry {
                path: path.clone(),
                identity: None,
                directory: false,
                error: None,
            };
            match fsutil::child_directory(directory, &name, DirectoryOpen::Existing) {
                Ok(Child::Directory { file, .. }) => {
                    // The root's frame is level 0, so this count is the level of the child.
                    if self.frames.len() > self.limits.deepest_directory() {
                        return Err(self.limits.too_deep());
                    }
                    let stat = identity(&file)?;
                    self.frames
                        .push(Frame::new(path, Some((stat.device, stat.inode))));
                    directories.push(file);
                    entry.directory = true;
                }
                Ok(Child::Missing) => continue,
                Ok(Child::Alias) => {}
                Err(error) if !structural_entry_error(&error) => entry.error = Some(error.kind()),
                Err(_) => match fsutil::child_regular(directory, &name) {
                    Ok(fsutil::RegularChild::File(file)) => entry.identity = Some(identity(&file)?),
                    Ok(fsutil::RegularChild::Missing) => continue,
                    Ok(fsutil::RegularChild::Alias) => {}
                    Err(error) if !structural_entry_error(&error) => {
                        entry.error = Some(error.kind())
                    }
                    Err(_) => {}
                },
            }
            entries.push(entry);
        }
        Ok((self, entries))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParentOpen {
    Existing,
    Create,
}

#[derive(Debug)]
pub(crate) enum ParentError {
    Finding(Reason),
    Missing,
    Io(io::Error),
}
impl From<io::Error> for ParentError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) fn structural_entry_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotADirectory | io::ErrorKind::IsADirectory | io::ErrorKind::InvalidInput
    ) || matches!(
        error.raw_os_error(),
        Some(libc::ELOOP | libc::ENXIO | libc::ENODEV)
    )
}

pub(crate) fn parent(root: &File, path: &Path, open: ParentOpen) -> Result<File, ParentError> {
    let mut directory = root.try_clone()?;
    let bytes = path.as_os_str().as_encoded_bytes();
    if path.is_absolute()
        || !bytes.is_empty()
            && bytes
                .split(|byte| *byte == b'/')
                .any(|part| matches!(part, b"" | b"." | b".."))
    {
        return Err(ParentError::Finding(Reason::InvalidName));
    }
    // Export checks the configured depth with `TreeLimits::admits_file` before a path gets here.
    // This only refuses what no configuration could allow, and does so before creating anything.
    if path.iter().count() > MAX_TREE_DEPTH {
        return Err(ParentError::Finding(Reason::HostRestriction));
    }
    for component in path {
        if component.as_encoded_bytes().len() > 255 {
            return Err(ParentError::Finding(Reason::HostRestriction));
        }
        if matches!(component.to_str(), Some("." | "..")) {
            return Err(ParentError::Finding(Reason::InvalidName));
        }
        let mode = match open {
            ParentOpen::Existing => DirectoryOpen::Existing,
            ParentOpen::Create => DirectoryOpen::CreateIfMissing,
        };
        match fsutil::child_directory(&directory, component, mode) {
            Ok(Child::Directory {
                file,
                created: _created,
            }) => {
                if open == ParentOpen::Create {
                    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
                    let (root_path, path, parent_path) = {
                        use crate::storage::fault;
                        let root_path = fault::descriptor_path(root)?;
                        let path = fault::descriptor_path(&file)?;
                        let parent_path = fault::descriptor_path(&directory)?;
                        if _created == fsutil::Created::Yes {
                            fault::dir_created(&path);
                        }
                        fault::hit_io(&root_path, fault::Step::SyncDirectorySync)?;
                        (root_path, path, parent_path)
                    };
                    file.sync_all()?;
                    directory.sync_all()?;
                    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
                    {
                        use crate::storage::fault;
                        fault::dir_synced(&path);
                        fault::dir_synced(&parent_path);
                        fault::hit_io(&root_path, fault::Step::SyncDirectoryDurable)?;
                    }
                }
                directory = file;
            }
            Ok(Child::Alias) => return Err(ParentError::Finding(Reason::Collision)),
            Ok(Child::Missing) => return Err(ParentError::Missing),
            Err(error) if !structural_entry_error(&error) => return Err(error.into()),
            Err(_) if fsutil::open_regular_at(&directory, component).is_ok() => {
                return Err(ParentError::Finding(Reason::Collision));
            }
            Err(_) => return Err(ParentError::Finding(Reason::UnsafeEntry)),
        }
    }
    Ok(directory)
}
