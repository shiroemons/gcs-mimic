use super::{DirectoryOpen, FileOpen, clear_errno, component, open_at, validate_name};
mod spelling;
pub(crate) use spelling::{child_directory, child_regular};
use std::{
    ffi::{CStr, OsStr, OsString},
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

/// Returns an exact-spelling keyset page without following directory entries.
pub fn directory_names_after(
    directory: &File,
    after: Option<&OsStr>,
    limit: usize,
) -> io::Result<Vec<OsString>> {
    directory_names_within(directory, after, limit, usize::MAX)
}

/// Like [`directory_names_after`], but fails with `InvalidData` past `max_inspected` names. Each
/// page reads the whole directory, so a tree limit stops an oversized directory on its first page
/// instead of after all the pages the limit admits.
#[expect(
    unsafe_code,
    reason = "openat, fdopendir and readdir have no safe std equivalent for a pinned descriptor"
)]
pub(crate) fn directory_names_within(
    directory: &File,
    after: Option<&OsStr>,
    limit: usize,
    max_inspected: usize,
) -> io::Result<Vec<OsString>> {
    if !(1..=256).contains(&limit) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // SAFETY: the descriptor is live and the literal is a terminated relative directory name.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is newly owned; fdopendir takes it only on success.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: fdopendir failed, leaving this descriptor owned here.
        unsafe {
            libc::close(fd);
        }
        return Err(error);
    }
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            // SAFETY: this guard uniquely owns the live directory stream.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let stream = Directory(stream);
    let mut names = std::collections::BTreeSet::new();
    let mut inspected = 0;
    loop {
        clear_errno();
        // SAFETY: stream remains live and iteration is confined to this thread.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error().is_some_and(|code| code != 0) {
                return Err(error);
            }
            break;
        }
        // SAFETY: readdir returned a live entry with a NUL-terminated name until the next call.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        inspected += 1;
        if inspected > max_inspected || bytes.len() > 4096 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let name = OsStr::from_bytes(bytes);
        if after.is_none_or(|after| name > after) {
            names.insert(OsString::from(name));
            if names.len() > limit {
                names.pop_last();
            }
        }
    }
    Ok(names.into_iter().collect())
}

/// Checks effective write access to the pinned directory without creating an entry.
#[expect(
    unsafe_code,
    reason = "faccessat on a pinned descriptor has no safe std equivalent"
)]
pub fn directory_writable(directory: &File) -> io::Result<()> {
    // SAFETY: the live descriptor and terminated dot name select the pinned directory.
    if unsafe {
        libc::faccessat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::W_OK,
            libc::AT_EACCESS,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Removes one descriptor-relative name without following a symlink destination.
#[expect(
    unsafe_code,
    reason = "unlinkat on a pinned descriptor has no safe std equivalent"
)]
pub(crate) fn unlink_file_at(directory: &File, name: &OsStr) -> io::Result<()> {
    validate_name(name)?;
    let name = component(name)?;
    // SAFETY: a validated single component is relative to the live directory descriptor.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    directory.sync_all()
}

/// Pins the selected directory without following a replacement symlink.
pub fn open_directory_nofollow(path: &Path) -> io::Result<File> {
    // Trailing separators/dots otherwise make the OS follow a final symlink.
    let path: PathBuf = path.components().collect();
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

/// Opens only a regular single-component entry under the pinned directory.
pub fn open_regular_at(directory: &File, name: &OsStr) -> io::Result<File> {
    open_at(directory, &component(name)?, FileOpen::Existing)
}

pub(crate) fn create_regular_at(directory: &File, name: &OsStr) -> io::Result<File> {
    open_at(directory, &component(name)?, FileOpen::CreateNew)
}

/// Opens a single directory component, creating it only when explicitly requested.
#[expect(
    unsafe_code,
    reason = "mkdirat and openat on a pinned descriptor have no safe std equivalent"
)]
pub(crate) fn directory_at(
    directory: &File,
    name: &OsStr,
    open: DirectoryOpen,
) -> io::Result<File> {
    let name = component(name)?;
    if open == DirectoryOpen::CreateIfMissing {
        // SAFETY: name is one validated component under the live directory descriptor.
        let result = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
        if result != 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
    }
    // SAFETY: name is terminated and the parent descriptor remains live.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat transfers exactly one owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}
