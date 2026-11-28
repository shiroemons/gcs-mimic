use super::FileIdentity;
mod spelling;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use spelling::{child_directory, child_regular};
pub(crate) use spelling::{directory_unconfirmed, regular_unconfirmed};
use std::{
    ffi::{CStr, OsStr, OsString},
    fs::File,
    io,
    ops::ControlFlow,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

/// Returns an exact-spelling keyset page without following directory entries.
pub(crate) fn directory_names_after(
    directory: &File,
    after: Option<&OsStr>,
    limit: usize,
) -> io::Result<Vec<OsString>> {
    directory_names_within(directory, after, limit, usize::MAX)
}

/// Like [`directory_names_after`], but fails with `InvalidData` past `max_inspected` names. Each
/// page reads the whole directory, so a tree limit stops an oversized directory on its first page
/// instead of after all the pages the limit admits.
pub(crate) fn directory_names_within(
    directory: &File,
    after: Option<&OsStr>,
    limit: usize,
    max_inspected: usize,
) -> io::Result<Vec<OsString>> {
    if !(1..=256).contains(&limit) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut names = std::collections::BTreeSet::new();
    let mut inspected = 0;
    each_name(directory, |name| {
        inspected += 1;
        if inspected > max_inspected || name.len() > 4096 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if after.is_none_or(|after| name > after) {
            names.insert(OsString::from(name));
            if names.len() > limit {
                names.pop_last();
            }
        }
        Ok(ControlFlow::Continue(()))
    })?;
    Ok(names.into_iter().collect())
}

/// Reads the names of `directory` in one pass, in the order the filesystem returns them, without
/// `.` and `..`, and hands each to `visit` until it breaks or fails.
#[expect(
    unsafe_code,
    reason = "openat, fdopendir and readdir have no safe std equivalent for a pinned descriptor"
)]
pub(crate) fn each_name(
    directory: &File,
    mut visit: impl FnMut(&OsStr) -> io::Result<ControlFlow<()>>,
) -> io::Result<()> {
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
    #[must_use = "dropping the guard closes the directory stream"]
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
    loop {
        clear_errno();
        // SAFETY: stream remains live and iteration is confined to this thread.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error().is_some_and(|code| code != 0) {
                return Err(error);
            }
            return Ok(());
        }
        // SAFETY: readdir returned a live entry with a NUL-terminated name until the next call.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        match visit(OsStr::from_bytes(bytes))? {
            ControlFlow::Continue(()) => {}
            ControlFlow::Break(()) => return Ok(()),
        }
    }
}

/// Pins the selected directory without following a replacement symlink.
pub(crate) fn open_directory_nofollow(path: &Path) -> io::Result<File> {
    // Trailing separators/dots otherwise make the OS follow a final symlink.
    let path: PathBuf = path.components().collect();
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

/// Opens only a regular single-component entry under the pinned directory.
pub(crate) fn open_regular_at(directory: &File, name: &OsStr) -> io::Result<File> {
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    super::failing_open(directory, name)?;
    open_at(directory, &component(name)?, FileOpen::Existing)
}

/// Opens an existing single directory component without following a symlink.
#[expect(
    unsafe_code,
    reason = "openat on a pinned descriptor has no safe std equivalent"
)]
pub(crate) fn directory_at(directory: &File, name: &OsStr) -> io::Result<File> {
    let name = component(name)?;
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    super::failing_open(directory, OsStr::from_bytes(name.as_bytes()))?;
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

/// `readdir` signals both the end of the stream and a failure with a null entry and sets
/// errno only on failure, so errno must be cleared before each call; a stale value would
/// turn a clean end into an error. Each libc names its thread-local errno accessor
/// differently, and a Unix target without a known accessor must not build.
#[expect(
    unsafe_code,
    reason = "errno is reachable only through the libc accessor"
)]
fn clear_errno() {
    cfg_select! {
        any(target_os = "solaris", target_os = "illumos") => { use libc::___errno as errno; }
        any(target_os = "android", target_os = "openbsd", target_os = "netbsd") => {
            use libc::__errno as errno;
        }
        any(target_os = "linux", target_os = "dragonfly") => {
            use libc::__errno_location as errno;
        }
        any(target_vendor = "apple", target_os = "freebsd") => { use libc::__error as errno; }
        _ => {
            compile_error!(
                "directory listing needs this target's errno accessor; add it to clear_errno"
            );
        }
    }
    // SAFETY: the accessor returns a valid pointer to the calling thread's errno.
    unsafe {
        *errno() = 0;
    }
}

/// The file-system object a path or handle resolved to: equal values name the same inode on the
/// same device, whatever its size or times.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NodeId {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

impl FileIdentity {
    pub(crate) fn node(&self) -> NodeId {
        NodeId {
            device: self.device,
            inode: self.inode,
        }
    }
}

impl From<&std::fs::Metadata> for FileIdentity {
    fn from(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified_ns: i128::from(metadata.mtime()) * 1_000_000_000
                + i128::from(metadata.mtime_nsec()),
            changed_ns: i128::from(metadata.ctime()) * 1_000_000_000
                + i128::from(metadata.ctime_nsec()),
        }
    }
}

impl From<&std::fs::Metadata> for NodeId {
    fn from(metadata: &std::fs::Metadata) -> Self {
        FileIdentity::from(metadata).node()
    }
}

pub(super) fn component(name: &OsStr) -> io::Result<std::ffi::CString> {
    super::validate_name(name)?;
    std::ffi::CString::new(name.as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

/// Whether `open_at` opens an existing regular file read-only or creates a new one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileOpen {
    Existing,
    #[cfg_attr(
        not(target_os = "linux"),
        expect(dead_code, reason = "only a Linux clone creates its entry first")
    )]
    CreateNew,
}

#[expect(
    unsafe_code,
    reason = "openat on a pinned descriptor has no safe std equivalent"
)]
pub(super) fn open_at(directory: &File, name: &CStr, open: FileOpen) -> io::Result<File> {
    let flags = libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | match open {
            FileOpen::Existing => libc::O_RDONLY,
            FileOpen::CreateNew => libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
        };
    // SAFETY: directory is live and name is a validated terminated single component.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat transfers a new descriptor to exactly one File owner.
    let file = unsafe { File::from_raw_fd(fd) };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    Ok(file)
}

/// Whether a clone failed only because the filesystem cannot clone, so the caller copies instead.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn copy_fallback(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EXDEV | libc::EINVAL | libc::ENOSYS | libc::ENOTSUP | libc::ENOTTY)
    )
}

/// Renames `source` to `destination` unless an entry already has that name.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[expect(unsafe_code, reason = "an exclusive rename has no safe std equivalent")]
fn rename_at(
    source_directory: libc::c_int,
    source: &CStr,
    destination_directory: libc::c_int,
    destination: &CStr,
) -> Result<(), super::FsError> {
    // SAFETY: the C strings remain live and the descriptors belong to the caller or AT_FDCWD.
    let result = unsafe {
        cfg_select! {
            target_os = "linux" => {
                // musl 1.2.5 has no renameat2 wrapper.
                libc::syscall(
                    libc::SYS_renameat2,
                    source_directory,
                    source.as_ptr(),
                    destination_directory,
                    destination.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            }
            target_os = "macos" => {
                libc::renameatx_np(
                    source_directory,
                    source.as_ptr(),
                    destination_directory,
                    destination.as_ptr(),
                    libc::RENAME_EXCL,
                )
            }
        }
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(libc::ENOSYS | libc::ENOTSUP | libc::EINVAL)
    ) {
        return Err(super::FsError::Unsupported);
    }
    Err(error.into())
}

/// Refuses to replace an existing entry. A fallback crash may leave both names, which
/// startup's staging recovery handles before admitting requests.
pub(crate) fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    cfg_select! {
        any(target_os = "linux", target_os = "macos") => {
            let source = std::ffi::CString::new(from.as_os_str().as_bytes())?;
            let destination = std::ffi::CString::new(to.as_os_str().as_bytes())?;
            match rename_at(libc::AT_FDCWD, &source, libc::AT_FDCWD, &destination) {
                Ok(()) => return Ok(()),
                Err(super::FsError::Unsupported) => {}
                Err(super::FsError::Io(error)) => return Err(error),
            }
        }
        _ => {}
    }
    link_then_unlink(from, to)
}

fn link_then_unlink(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::hard_link(from, to)?;
    std::fs::remove_file(from)
}

/// What a failed `fgetxattr` of the source metadata means: no attribute, no support for one, one
/// too large for the buffer, or an I/O failure.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn classify(error: io::Error) -> Result<Option<usize>, super::SourceMetadataError> {
    use super::SourceMetadataError;
    let errno = error.raw_os_error();
    let missing = cfg_select! {
        target_os = "linux" => { libc::ENODATA }
        target_os = "macos" => { libc::ENOATTR }
    };
    if errno == Some(missing) {
        Ok(None)
    } else if errno == Some(libc::ENOTSUP) || errno == Some(libc::EOPNOTSUPP) {
        Err(SourceMetadataError::Unsupported)
    } else if errno == Some(libc::ERANGE) || errno == Some(libc::E2BIG) {
        Err(SourceMetadataError::TooLarge)
    } else {
        Err(SourceMetadataError::Io(error))
    }
}

/// The Linux and macOS [`super::read_source_metadata`]: `fgetxattr` of `user.metadata` into the
/// whole buffer.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[expect(
    unsafe_code,
    reason = "descriptor xattrs are reachable only through fgetxattr"
)]
pub(super) fn read_source_metadata<'a>(
    file: &File,
    buffer: &'a mut super::SourceMetadataBuffer,
) -> Result<Option<&'a [u8]>, super::SourceMetadataError> {
    // SAFETY: the live file descriptor and literal NUL-terminated key are valid;
    // the exclusive buffer pointer covers exactly the supplied allocation length.
    let result = unsafe {
        cfg_select! {
            target_os = "linux" => {
                libc::fgetxattr(
                    file.as_raw_fd(),
                    c"user.metadata".as_ptr(),
                    buffer.0.as_mut_ptr().cast(),
                    buffer.0.len(),
                )
            }
            target_os = "macos" => {
                libc::fgetxattr(
                    file.as_raw_fd(),
                    c"user.metadata".as_ptr(),
                    buffer.0.as_mut_ptr().cast(),
                    buffer.0.len(),
                    0,
                    0,
                )
            }
        }
    };
    let length = if result < 0 {
        classify(io::Error::last_os_error())?
    } else {
        super::checked_length(result.unsigned_abs())?
    };
    length
        .map(|length| {
            buffer
                .0
                .get(..length)
                .ok_or(super::SourceMetadataError::TooLarge)
        })
        .transpose()
}
