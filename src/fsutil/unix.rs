use super::{DirectoryOpen, FileOpen, clear_errno, component, open_at, validate_name};
mod spelling;
pub(crate) use spelling::{child_directory, child_regular, existing_directory, existing_regular};
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

/// The longest file name, in bytes, that the filesystem holding `directory` accepts, or `None`
/// when it reports no limit.
#[expect(
    unsafe_code,
    reason = "fpathconf on a pinned descriptor has no safe std equivalent"
)]
pub(crate) fn name_max(directory: &File) -> io::Result<Option<usize>> {
    // fpathconf reports both "no limit" and a failure as -1 and sets errno only on failure.
    clear_errno();
    // SAFETY: the live descriptor is only queried; fpathconf neither keeps nor writes through it.
    let value = unsafe { libc::fpathconf(directory.as_raw_fd(), libc::_PC_NAME_MAX) };
    if value < 0 {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(0) | None => Ok(None),
            Some(_) => Err(error),
        };
    }
    usize::try_from(value)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// The identity of `file` as its filesystem reports it now. A Linux FUSE mount, virtiofs and
/// therefore Docker Desktop's bind mounts included, and an NFS mount answer `fstat` from an
/// attribute cache for a while, which a rename made on the host or another client does not
/// invalidate; `AT_STATX_FORCE_SYNC` asks the filesystem instead, at the cost of one attribute
/// request. A kernel or seccomp profile without `statx` (`ENOSYS`, `EPERM`), or a filesystem
/// that leaves a field out of its answer, is answered by `fstat` as before.
#[cfg(target_os = "linux")]
#[expect(
    unsafe_code,
    reason = "statx with AT_STATX_FORCE_SYNC has no safe std equivalent"
)]
pub(crate) fn uncached_identity(file: &File) -> io::Result<super::FileIdentity> {
    const NEEDED: libc::c_uint =
        libc::STATX_INO | libc::STATX_SIZE | libc::STATX_MTIME | libc::STATX_CTIME;
    let mut buffer = std::mem::MaybeUninit::<libc::statx>::zeroed();
    // SAFETY: the live descriptor, the terminated empty path that AT_EMPTY_PATH names it by, and
    // the writable buffer are valid for the call, which keeps none of them.
    let result = unsafe {
        libc::statx(
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_STATX_FORCE_SYNC,
            libc::STATX_BASIC_STATS,
            buffer.as_mut_ptr(),
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ENOSYS | libc::EPERM) => Ok(super::stat_identity(file)?),
            Some(_) | None => Err(error),
        };
    }
    // SAFETY: statx succeeded, and the zeroed buffer was valid for every field before it wrote.
    let status = unsafe { buffer.assume_init() };
    if status.stx_mask & NEEDED != NEEDED {
        return Ok(super::stat_identity(file)?);
    }
    let nanos = |time: libc::statx_timestamp| {
        i128::from(time.tv_sec) * 1_000_000_000 + i128::from(time.tv_nsec)
    };
    Ok(super::FileIdentity {
        device: libc::makedev(status.stx_dev_major, status.stx_dev_minor),
        inode: status.stx_ino,
        size: status.stx_size,
        modified_ns: nanos(status.stx_mtime),
        changed_ns: nanos(status.stx_ctime),
    })
}

/// The identity of `file` as `fstat` reports it. macOS has no request past a network mount's
/// attribute cache and needs none: its directory observer accepts only a root on a local
/// filesystem, which answers `fstat` from the filesystem itself.
#[cfg(not(target_os = "linux"))]
pub(crate) fn uncached_identity(file: &File) -> io::Result<super::FileIdentity> {
    Ok(super::stat_identity(file)?)
}

/// Removes one descriptor-relative name without following a symlink destination.
#[expect(
    unsafe_code,
    reason = "unlinkat on a pinned descriptor has no safe std equivalent"
)]
pub(crate) fn unlink_file_at(directory: &File, name: &OsStr) -> io::Result<()> {
    validate_name(name)?;
    let name = component(name)?;
    #[cfg(test)]
    super::count_unlink();
    // SAFETY: a validated single component is relative to the live directory descriptor.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    directory.sync_all()
}

/// Removes the empty directory `name` under the pinned directory. `unlinkat(AT_REMOVEDIR)` never
/// follows a symlink: it fails with `ENOTDIR` on one, and with `ENOTEMPTY` or `EEXIST` on a
/// directory that holds any entry, so it never removes what a directory holds. It does not sync
/// the parent: the caller does, after the step the fault model checks between the two.
#[expect(
    unsafe_code,
    reason = "unlinkat on a pinned descriptor has no safe std equivalent"
)]
pub(crate) fn remove_empty_directory_at(directory: &File, name: &OsStr) -> io::Result<()> {
    let name = component(name)?;
    // SAFETY: a validated single component is relative to the live directory descriptor.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    super::vanishing_open(directory, name)?;
    open_at(directory, &component(name)?, FileOpen::Existing)
}

/// Opens only an existing regular single-component entry under the pinned directory, for writing.
pub(crate) fn open_regular_writable_at(directory: &File, name: &OsStr) -> io::Result<File> {
    open_at(directory, &component(name)?, FileOpen::ExistingWritable)
}

pub(crate) fn create_regular_at(directory: &File, name: &OsStr) -> io::Result<File> {
    open_at(directory, &component(name)?, FileOpen::CreateNew)
}

/// Whether `file` is a regular file that no other name links, so changing its bytes changes
/// what no other path shows.
pub(crate) fn sole_regular_link(file: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(metadata.is_file() && metadata.nlink() == 1)
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
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    if open == DirectoryOpen::Existing {
        super::vanishing_open(directory, OsStr::from_bytes(name.as_bytes()))?;
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
