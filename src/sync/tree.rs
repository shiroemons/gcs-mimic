//! Descriptor-relative traversal shared by synchronization and catalog write-back.

use super::mapping::Reason;
use crate::fsutil::{self, Child, DirectoryOpen};
use crate::{fsutil::FileIdentity, storage::admin::AdminError};
use std::{collections::VecDeque, ffi::OsString, path::PathBuf};
use std::{fs::File, io, path::Path};
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

pub(crate) struct TreeScan {
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
    pub(crate) fn new() -> Self {
        Self {
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
                match fsutil::directory_names_after(directory, frame.after.as_deref(), 256) {
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
                    Err(error) => return Err(error.into()),
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
            if self.visited > 10_000 {
                return Err(AdminError::Invalid("sync tree exceeds 10000 inspected entries; preserve the tree and reduce its size before rescanning".into()));
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
                    if self.frames.len() >= 128 {
                        return Err(AdminError::Invalid(
                            "sync tree exceeds 128 levels; reduce its depth".into(),
                        ));
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
    for (depth, component) in path.iter().enumerate() {
        if depth >= 128 || component.as_encoded_bytes().len() > 255 {
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
