#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Descriptor-based filesystem operations; OS-specific unsafe stays in this module.

use std::{fs::File, io};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyMethod {
    Clone,
    #[cfg(target_os = "linux")]
    CopyFileRange,
    ReadWrite,
}

/// Returns an exact-spelling keyset page without following directory entries.
#[cfg_attr(
    unix,
    expect(
        unsafe_code,
        reason = "openat, fdopendir and readdir have no safe std equivalent for a pinned descriptor"
    )
)]
pub fn directory_names_after(
    directory: &File,
    after: Option<&std::ffi::OsStr>,
    limit: usize,
) -> io::Result<Vec<std::ffi::OsString>> {
    if !(1..=256).contains(&limit) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    #[cfg(unix)]
    {
        use std::{
            ffi::{CStr, OsString},
            os::{fd::AsRawFd, unix::ffi::OsStrExt},
        };
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
            if inspected > 10_000 || bytes.len() > 4096 {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let name = std::ffi::OsStr::from_bytes(bytes);
            if after.is_none_or(|after| name > after) {
                names.insert(OsString::from(name));
                if names.len() > limit {
                    names.pop_last();
                }
            }
        }
        Ok(names.into_iter().collect())
    }
    #[cfg(not(unix))]
    {
        let _ = (directory, after);
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// `readdir` signals both the end of the stream and a failure with a null entry and sets
/// errno only on failure, so errno must be cleared before each call; a stale value would
/// turn a clean end into an error. Each libc names its thread-local errno accessor
/// differently, and a Unix target without a known accessor must not build.
#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "errno is reachable only through the libc accessor"
)]
fn clear_errno() {
    #[cfg(any(target_os = "solaris", target_os = "illumos"))]
    use libc::___errno as errno;
    #[cfg(any(target_os = "android", target_os = "openbsd", target_os = "netbsd"))]
    use libc::__errno as errno;
    #[cfg(any(target_os = "linux", target_os = "dragonfly"))]
    use libc::__errno_location as errno;
    #[cfg(any(target_vendor = "apple", target_os = "freebsd"))]
    use libc::__error as errno;
    #[cfg(not(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "android",
        target_os = "openbsd",
        target_os = "netbsd",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "solaris",
        target_os = "illumos",
    )))]
    compile_error!("directory listing needs this target's errno accessor; add it to clear_errno");
    // SAFETY: the accessor returns a valid pointer to the calling thread's errno.
    unsafe {
        *errno() = 0;
    }
}

/// Checks effective write access to the pinned directory without creating an entry.
#[cfg_attr(
    unix,
    expect(
        unsafe_code,
        reason = "faccessat on a pinned descriptor has no safe std equivalent"
    )
)]
pub fn directory_writable(directory: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
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
    #[cfg(not(unix))]
    {
        let _ = directory;
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Removes one descriptor-relative name without following a symlink destination.
#[cfg_attr(
    unix,
    expect(
        unsafe_code,
        reason = "unlinkat on a pinned descriptor has no safe std equivalent"
    )
)]
pub(crate) fn unlink_file_at(directory: &File, name: &std::ffi::OsStr) -> io::Result<()> {
    validate_name(name)?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let name = component(name)?;
        // SAFETY: a validated single component is relative to the live directory descriptor.
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        directory.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Err(io::ErrorKind::Unsupported.into())
    }
}

pub(crate) fn open_directory(path: &std::path::Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS);
    }
    options.open(path)
}

/// Pins the selected directory without following a replacement symlink.
pub fn open_directory_nofollow(path: &std::path::Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Trailing separators/dots otherwise make the OS follow a final symlink.
        let path: std::path::PathBuf = path.components().collect();
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Opens only a regular single-component entry under the pinned directory.
pub fn open_regular_at(directory: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    #[cfg(unix)]
    {
        open_at(directory, &component(name)?, FileOpen::Existing)
    }
    #[cfg(not(unix))]
    {
        let _ = (directory, name);
        Err(io::ErrorKind::Unsupported.into())
    }
}

pub(crate) fn create_regular_at(directory: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    #[cfg(unix)]
    {
        open_at(directory, &component(name)?, FileOpen::CreateNew)
    }
    #[cfg(not(unix))]
    {
        let _ = (directory, name);
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Whether a directory helper may create a missing directory or must find an existing one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DirectoryOpen {
    Existing,
    CreateIfMissing,
}

/// Opens a single directory component, creating it only when explicitly requested.
#[cfg_attr(
    unix,
    expect(
        unsafe_code,
        reason = "mkdirat and openat on a pinned descriptor have no safe std equivalent"
    )
)]
pub(crate) fn directory_at(
    directory: &File,
    name: &std::ffi::OsStr,
    open: DirectoryOpen,
) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};
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
    #[cfg(not(unix))]
    {
        let _ = (directory, name, open);
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Walks an exact relative spelling without following redirects; creation syncs each parent.
#[cfg(all(test, unix))]
pub(crate) fn directory_path_at(
    directory: &File,
    path: &std::path::Path,
    open: DirectoryOpen,
) -> io::Result<File> {
    let text = path.to_str().ok_or(io::ErrorKind::InvalidInput)?;
    if text.is_empty() || text.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut current = directory.try_clone()?;
    for part in text.split('/') {
        validate_name(part.as_ref())?;
        let next = directory_at(&current, part.as_ref(), open)?;
        if open == DirectoryOpen::CreateIfMissing {
            next.sync_all()?;
            current.sync_all()?;
        }
        current = next;
    }
    Ok(current)
}

#[derive(Debug)]
pub(crate) enum FsError {
    Io(io::Error),
    Unsupported,
}

impl From<io::Error> for FsError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(f),
            Self::Unsupported => f.write_str("filesystem operation is unsupported"),
        }
    }
}

impl std::error::Error for FsError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub modified_ns: i128,
    pub changed_ns: i128,
}

#[cfg(unix)]
impl FileIdentity {
    fn same_displaced_file(&self, other: &Self) -> bool {
        // Rename may change ctime, so compare the displaced inode and content observation.
        self.device == other.device
            && self.inode == other.inode
            && self.size == other.size
            && self.modified_ns == other.modified_ns
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReplaceOutcome {
    Created,
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(
            dead_code,
            reason = "only Linux and macOS renames can verify a displaced file"
        )
    )]
    Replaced {
        displaced: FileIdentity,
    },
    Conflict {
        displaced: Option<FileIdentity>,
    },
}

#[cfg(unix)]
pub(crate) fn sync_dir(path: &std::path::Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg_attr(
    windows,
    expect(
        unsafe_code,
        reason = "the volume serial and file index come only from GetFileInformationByHandle"
    )
)]
pub(crate) fn stat_identity(file: &File) -> Result<FileIdentity, FsError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified_ns: i128::from(metadata.mtime()) * 1_000_000_000
                + i128::from(metadata.mtime_nsec()),
            changed_ns: i128::from(metadata.ctime()) * 1_000_000_000
                + i128::from(metadata.ctime_nsec()),
        })
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
        // SAFETY: the live file handle and writable output allocation are valid for this call.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) }
            == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: a successful call initialized every field of the output structure.
        let information = unsafe { information.assume_init() };
        let modified = (u64::from(information.ftLastWriteTime.dwHighDateTime) << 32)
            | u64::from(information.ftLastWriteTime.dwLowDateTime);
        Ok(FileIdentity {
            device: u64::from(information.dwVolumeSerialNumber),
            inode: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
            size: (u64::from(information.nFileSizeHigh) << 32)
                | u64::from(information.nFileSizeLow),
            modified_ns: i128::from(modified) * 100 - 11_644_473_600_000_000_000,
            changed_ns: windows_change_time(file)?,
        })
    }
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "the change time comes only from GetFileInformationByHandleEx"
)]
fn windows_change_time(file: &File) -> Result<i128, FsError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
    };
    let mut basic = std::mem::MaybeUninit::<FILE_BASIC_INFO>::uninit();
    // SAFETY: the output buffer matches FileBasicInfo and the file handle remains live.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            basic.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: successful FileBasicInfo initialized the structure.
    Ok(i128::from(unsafe { basic.assume_init() }.ChangeTime) * 100 - 11_644_473_600_000_000_000)
}

fn validate_name(name: &std::ffi::OsStr) -> io::Result<()> {
    let mut components = std::path::Path::new(name).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
        || name.is_empty()
        || name.as_encoded_bytes().contains(&0)
        || name.as_encoded_bytes().contains(&b'/')
        || (cfg!(windows) && name.as_encoded_bytes().contains(&b'\\'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected one path component",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn component(name: &std::ffi::OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    validate_name(name)?;
    std::ffi::CString::new(name.as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

/// Whether `open_at` opens an existing regular file read-only or creates a new one.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileOpen {
    Existing,
    CreateNew,
}

#[cfg(unix)]
#[expect(
    unsafe_code,
    reason = "openat on a pinned descriptor has no safe std equivalent"
)]
fn open_at(directory: &File, name: &std::ffi::CStr, open: FileOpen) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let flags = libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if open == FileOpen::CreateNew {
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL
        } else {
            libc::O_RDONLY
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

/// Creates a clone without copying; Unsupported leaves no new entry.
/// Mutable-source staging can then fall back to its own single-pass copy and hash.
#[cfg_attr(
    any(target_os = "linux", target_os = "macos"),
    expect(
        unsafe_code,
        reason = "fclonefileat, FICLONE and unlinkat have no safe std equivalent"
    )
)]
pub(crate) fn clone_into(
    src: &File,
    directory: &File,
    name: &std::ffi::OsStr,
) -> Result<(), FsError> {
    validate_name(name)?;
    if !src.metadata()?.is_file() {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "expected a regular source").into(),
        );
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::fd::AsRawFd;
        let name = component(name)?;
        #[cfg(target_os = "macos")]
        {
            // SAFETY: live descriptors and a validated destination component; clone creates a new entry.
            if unsafe {
                libc::fclonefileat(src.as_raw_fd(), directory.as_raw_fd(), name.as_ptr(), 0)
            } == 0
            {
                open_at(directory, &name, FileOpen::Existing)?.sync_all()?;
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if !copy_fallback(&error) {
                return Err(error.into());
            }
            Err(FsError::Unsupported)
        }
        #[cfg(target_os = "linux")]
        {
            let dst = open_at(directory, &name, FileOpen::CreateNew)?;
            // SAFETY: FICLONE receives the two live regular-file descriptors, with a new empty target.
            if unsafe { libc::ioctl(dst.as_raw_fd(), libc::FICLONE as _, src.as_raw_fd()) } == 0 {
                dst.sync_all()?;
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if !copy_fallback(&error) {
                return Err(error.into());
            }
            // SAFETY: only this call owns the newly created target; the component and directory are live.
            if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error().into());
            }
            Err(FsError::Unsupported)
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = directory;
        Err(FsError::Unsupported)
    }
}

/// Copies an immutable open blob. Mutable-source staging must hash its own read/write pass.
/// The source offset is unchanged; a failed copy leaves its new entry for caller cleanup.
#[cfg_attr(
    target_os = "linux",
    expect(unsafe_code, reason = "copy_file_range has no safe std equivalent")
)]
pub(crate) fn clone_or_copy_into(
    src: &File,
    directory: &File,
    name: &std::ffi::OsStr,
) -> Result<CopyMethod, FsError> {
    match clone_into(src, directory, name) {
        Ok(()) => return Ok(CopyMethod::Clone),
        Err(FsError::Unsupported) => (),
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        let name = component(name)?;
        let dst = open_at(directory, &name, FileOpen::CreateNew)?;
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let mut offset: libc::loff_t = 0;
            loop {
                // SAFETY: descriptors remain live, input offset is writable, and no userspace buffer is passed.
                let copied = unsafe {
                    libc::copy_file_range(
                        src.as_raw_fd(),
                        &mut offset,
                        dst.as_raw_fd(),
                        std::ptr::null_mut(),
                        1024 * 1024,
                        0,
                    )
                };
                if copied == 0 {
                    dst.sync_all()?;
                    return Ok(CopyMethod::CopyFileRange);
                }
                if copied < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    if !copy_fallback(&error) {
                        return Err(error.into());
                    }
                    dst.set_len(0)?;
                    break;
                }
            }
        }
        copy_read_write(src, &dst)?;
        dst.sync_all()?;
        Ok(CopyMethod::ReadWrite)
    }
    #[cfg(windows)]
    {
        let path = directory_path(directory)?.join(name);
        let dst = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        copy_read_write(src, &dst)?;
        dst.sync_all()?;
        Ok(CopyMethod::ReadWrite)
    }
}

#[cfg(unix)]
fn copy_fallback(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EXDEV | libc::EINVAL | libc::ENOSYS | libc::ENOTSUP | libc::ENOTTY)
    )
}

pub(crate) fn read_at(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_at(file, bytes, offset)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::FileExt::seek_read(file, bytes, offset)
    }
}

fn copy_read_write(src: &File, dst: &File) -> io::Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::FileExt;
    #[cfg(windows)]
    use std::os::windows::fs::FileExt;
    let mut bytes = [0u8; 65_536];
    let mut offset = 0;
    loop {
        let read = read_at(src, &mut bytes, offset);
        let count = match read {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Ok(());
        }
        let mut written = 0;
        while written < count {
            #[cfg(unix)]
            let result = dst.write_at(&bytes[written..count], offset + written as u64);
            #[cfg(windows)]
            let result = dst.seek_write(&bytes[written..count], offset + written as u64);
            match result {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "file copy made no progress",
                    ));
                }
                Ok(count) => written += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
                Err(error) => return Err(error),
            }
        }
        offset += count as u64;
    }
}

#[cfg(unix)]
fn identity_at(directory: &File, name: &std::ffi::CStr) -> Result<Option<FileIdentity>, FsError> {
    match open_at(directory, name, FileOpen::Existing) {
        Ok(file) => Ok(Some(stat_identity(&file)?)),
        Err(error)
            if matches!(error.raw_os_error(), Some(libc::ELOOP))
                || matches!(
                    error.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::NotFound
                ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

/// Leaves the displaced entry at staged_name, including on an identity conflict.
/// An error after rename may have applied the operation; callers must reconcile both names.
#[cfg(all(test, unix))]
pub(crate) fn durable_replace(
    directory: &File,
    staged_name: &std::ffi::OsStr,
    destination_name: &std::ffi::OsStr,
    expected: Option<&FileIdentity>,
) -> Result<ReplaceOutcome, FsError> {
    durable_replace_between(
        directory,
        staged_name,
        directory,
        destination_name,
        expected,
    )
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) static FAIL_REPLACE_SYNC: std::sync::Mutex<Option<(u64, u64)>> =
    std::sync::Mutex::new(None);

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sync_replace_parent(directory: &File) -> io::Result<()> {
    #[cfg(test)]
    {
        let identity = stat_identity(directory).map_err(|e| io::Error::other(e.to_string()))?;
        if *FAIL_REPLACE_SYNC
            .lock()
            .map_err(|_| io::Error::other("sync injection poisoned"))?
            == Some((identity.device, identity.inode))
        {
            return Err(io::Error::other("injected replacement parent sync failure"));
        }
    }
    directory.sync_all()
}

/// Retains displaced bytes under the source name; errors after rename require reconciliation.
#[cfg_attr(
    any(target_os = "linux", target_os = "macos"),
    expect(
        unsafe_code,
        reason = "renameat2 and renameatx_np exchanges have no safe std equivalent"
    )
)]
pub(crate) fn durable_replace_between(
    source_directory: &File,
    staged_name: &std::ffi::OsStr,
    destination_directory: &File,
    destination_name: &std::ffi::OsStr,
    expected: Option<&FileIdentity>,
) -> Result<ReplaceOutcome, FsError> {
    validate_name(staged_name)?;
    validate_name(destination_name)?;
    let source_identity = stat_identity(source_directory)?;
    let destination_identity = stat_identity(destination_directory)?;
    let same_directory = source_identity.device == destination_identity.device
        && source_identity.inode == destination_identity.inode;
    if same_directory && staged_name == destination_name {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "replacement names must differ").into(),
        );
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let staged = component(staged_name)?;
        let destination = component(destination_name)?;
        open_at(source_directory, &staged, FileOpen::Existing)?.sync_all()?;
        let before = if expected.is_some() {
            identity_at(destination_directory, &destination)?
        } else {
            None
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            // SAFETY: validated components are relative to their respective live directory descriptors.
            let result = unsafe {
                // musl 1.2.5 has no renameat2 wrapper, so the release musl archives call the syscall directly.
                #[cfg(target_os = "linux")]
                {
                    libc::syscall(
                        libc::SYS_renameat2,
                        source_directory.as_raw_fd(),
                        staged.as_ptr(),
                        destination_directory.as_raw_fd(),
                        destination.as_ptr(),
                        if expected.is_some() {
                            libc::RENAME_EXCHANGE
                        } else {
                            libc::RENAME_NOREPLACE
                        },
                    )
                }
                #[cfg(target_os = "macos")]
                {
                    libc::renameatx_np(
                        source_directory.as_raw_fd(),
                        staged.as_ptr(),
                        destination_directory.as_raw_fd(),
                        destination.as_ptr(),
                        if expected.is_some() {
                            libc::RENAME_SWAP
                        } else {
                            libc::RENAME_EXCL
                        },
                    )
                }
            };
            if result != 0 {
                let error = io::Error::last_os_error();
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENOSYS | libc::ENOTSUP | libc::EINVAL)
                ) {
                    return Err(FsError::Unsupported);
                }
                return Err(error.into());
            }
            sync_replace_parent(source_directory)?;
            if !same_directory {
                sync_replace_parent(destination_directory)?;
            }
            let Some(expected) = expected else {
                return Ok(ReplaceOutcome::Created);
            };
            let displaced = identity_at(source_directory, &staged)?;
            if let Some(displaced) = displaced
                && before.as_ref() == Some(expected)
                && expected.same_displaced_file(&displaced)
            {
                return Ok(ReplaceOutcome::Replaced { displaced });
            }
            Ok(ReplaceOutcome::Conflict { displaced })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (destination, expected);
            Err(FsError::Unsupported)
        }
    }
    #[cfg(windows)]
    {
        if !same_directory {
            return Err(FsError::Unsupported);
        }
        let source = directory_path(source_directory)?.join(staged_name);
        let target = directory_path(destination_directory)?.join(destination_name);
        File::open(&source)?.sync_all()?;
        if let Some(expected) = expected {
            let observed = stat_identity(&File::open(&target)?)?;
            if expected != &observed {
                return Ok(ReplaceOutcome::Conflict { displaced: None });
            }
            // Sync must preserve the old target before selecting its checked-replace fallback.
            return Err(FsError::Unsupported);
        }
        move_write_through(&source, &target)?;
        Ok(ReplaceOutcome::Created)
    }
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "a handle's path comes only from GetFinalPathNameByHandleW"
)]
fn directory_path(directory: &File) -> io::Result<std::path::PathBuf> {
    use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
    let mut buffer = vec![0u16; 32_768];
    // SAFETY: the live handle and writable UTF-16 allocation cover the supplied length.
    let count = unsafe {
        GetFinalPathNameByHandleW(
            directory.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    };
    if count == 0 {
        return Err(io::Error::last_os_error());
    }
    if count as usize >= buffer.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory path exceeds limit",
        ));
    }
    Ok(std::ffi::OsString::from_wide(&buffer[..count as usize]).into())
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "MOVEFILE_WRITE_THROUGH is reachable only through MoveFileExW"
)]
pub(crate) fn move_write_through(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let wide = |path: &std::path::Path| -> io::Result<Vec<u16>> {
        let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in file path",
            ));
        }
        wide.push(0);
        Ok(wide)
    };
    let source = wide(source)?;
    let destination = wide(destination)?;
    // SAFETY: terminated UTF-16 buffers remain live for the synchronous call.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) const SOURCE_METADATA_CAP: usize = 65_536;

pub(crate) struct SourceMetadataBuffer(Box<[u8; SOURCE_METADATA_CAP + 1]>);

impl Default for SourceMetadataBuffer {
    fn default() -> Self {
        Self(Box::new([0; SOURCE_METADATA_CAP + 1]))
    }
}

#[derive(Debug)]
pub(crate) enum SourceMetadataError {
    Unsupported,
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos", test)),
        expect(
            dead_code,
            reason = "other platforms report every source as unsupported"
        )
    )]
    TooLarge,
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(
            dead_code,
            reason = "other platforms report every source as unsupported"
        )
    )]
    Io(io::Error),
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn classify(error: io::Error) -> Result<Option<usize>, SourceMetadataError> {
    let errno = error.raw_os_error();
    #[cfg(target_os = "linux")]
    let missing = libc::ENODATA;
    #[cfg(target_os = "macos")]
    let missing = libc::ENOATTR;
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

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn checked_length(length: usize) -> Result<Option<usize>, SourceMetadataError> {
    if length > SOURCE_METADATA_CAP {
        Err(SourceMetadataError::TooLarge)
    } else {
        Ok(Some(length))
    }
}

/// The dedicated buffer prevents callers from supplying a smaller allocation.
#[cfg_attr(
    any(target_os = "linux", target_os = "macos"),
    expect(
        unsafe_code,
        reason = "descriptor xattrs are reachable only through fgetxattr"
    )
)]
pub(crate) fn read_source_metadata<'a>(
    file: &File,
    buffer: &'a mut SourceMetadataBuffer,
) -> Result<Option<&'a [u8]>, SourceMetadataError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: the live file descriptor and literal NUL-terminated key are valid;
        // the exclusive buffer pointer covers exactly the supplied allocation length.
        let result = unsafe {
            #[cfg(target_os = "linux")]
            {
                libc::fgetxattr(
                    file.as_raw_fd(),
                    c"user.metadata".as_ptr(),
                    buffer.0.as_mut_ptr().cast(),
                    buffer.0.len(),
                )
            }
            #[cfg(target_os = "macos")]
            {
                libc::fgetxattr(
                    file.as_raw_fd(),
                    c"user.metadata".as_ptr(),
                    buffer.0.as_mut_ptr().cast(),
                    buffer.0.len(),
                    0,
                    0,
                )
            }
        };
        let length = if result < 0 {
            classify(io::Error::last_os_error())?
        } else {
            checked_length(result as usize)?
        };
        Ok(length.map(|length| &buffer.0[..length]))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (file, &mut buffer.0);
        Err(SourceMetadataError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn source_metadata_reads_only_into_the_dedicated_buffer() {
        // The signature is the guarantee: a caller cannot pass a byte slice or a smaller
        // array, so the kernel never writes past the capped allocation.
        type Read = for<'a> fn(
            &File,
            &'a mut SourceMetadataBuffer,
        ) -> Result<Option<&'a [u8]>, SourceMetadataError>;
        let _: Read = read_source_metadata;
        assert_eq!(
            SourceMetadataBuffer::default().0.len(),
            SOURCE_METADATA_CAP + 1
        );
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_copy_preserves_bytes_offsets_and_exclusive_names() {
        use std::io::{Seek, SeekFrom};
        let directory = super::test_support::SourceDir::new();
        let bytes: Vec<u8> = (0..150_003).map(|i| (i % 251) as u8).collect();
        std::fs::write(directory.0.join("source"), &bytes).unwrap();
        let mut source = File::open(directory.0.join("source")).unwrap();
        source.seek(SeekFrom::Start(17)).unwrap();
        let root = File::open(&directory.0).unwrap();
        clone_or_copy_into(&source, &root, "copy".as_ref()).unwrap();
        assert_eq!(std::fs::read(directory.0.join("copy")).unwrap(), bytes);
        assert_eq!(source.stream_position().unwrap(), 17);
        assert!(
            matches!(clone_or_copy_into(&source, &root, "copy".as_ref()), Err(FsError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "nested/file",
            "copy/",
            "bad\0name",
        ] {
            assert!(
                matches!(clone_or_copy_into(&source, &root, name.as_ref()), Err(FsError::Io(error)) if error.kind() == io::ErrorKind::InvalidInput)
            );
        }
        let fallback = File::create(directory.0.join("fallback")).unwrap();
        copy_read_write(&source, &fallback).unwrap();
        assert_eq!(std::fs::read(directory.0.join("fallback")).unwrap(), bytes);
        assert_eq!(source.stream_position().unwrap(), 17);
        let empty = File::create(directory.0.join("empty")).unwrap();
        drop(empty);
        clone_or_copy_into(
            &File::open(directory.0.join("empty")).unwrap(),
            &root,
            "empty-copy".as_ref(),
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(directory.0.join("empty-copy"))
                .unwrap()
                .len(),
            0
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn conditional_replace_preserves_displaced_and_conflicting_bytes() {
        let directory = super::test_support::SourceDir::new();
        let root = File::open(&directory.0).unwrap();
        std::fs::write(directory.0.join("stage"), b"first").unwrap();
        assert_eq!(
            durable_replace(&root, "stage".as_ref(), "target".as_ref(), None).unwrap(),
            ReplaceOutcome::Created
        );
        std::fs::write(directory.0.join("stage"), b"second").unwrap();
        assert!(
            matches!(durable_replace(&root, "stage".as_ref(), "target".as_ref(), None), Err(FsError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        assert_eq!(std::fs::read(directory.0.join("target")).unwrap(), b"first");
        let expected = stat_identity(&File::open(directory.0.join("target")).unwrap()).unwrap();
        assert!(matches!(
            durable_replace(&root, "stage".as_ref(), "target".as_ref(), Some(&expected)).unwrap(),
            ReplaceOutcome::Replaced { .. }
        ));
        assert_eq!(std::fs::read(directory.0.join("stage")).unwrap(), b"first");
        assert_eq!(
            std::fs::read(directory.0.join("target")).unwrap(),
            b"second"
        );
        std::fs::write(directory.0.join("stage"), b"third").unwrap();
        assert!(matches!(
            durable_replace(&root, "stage".as_ref(), "target".as_ref(), Some(&expected)).unwrap(),
            ReplaceOutcome::Conflict { displaced: Some(_) }
        ));
        assert_eq!(std::fs::read(directory.0.join("stage")).unwrap(), b"second");
        assert_eq!(std::fs::read(directory.0.join("target")).unwrap(), b"third");
        std::fs::remove_file(directory.0.join("target")).unwrap();
        std::os::unix::fs::symlink("missing", directory.0.join("target")).unwrap();
        assert_eq!(
            durable_replace(&root, "stage".as_ref(), "target".as_ref(), Some(&expected)).unwrap(),
            ReplaceOutcome::Conflict { displaced: None }
        );
        assert!(
            std::fs::symlink_metadata(directory.0.join("stage"))
                .unwrap()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn export_directory_walk_rejects_redirects_and_unsafe_components() {
        let directory = super::test_support::SourceDir::new();
        let root = open_directory_nofollow(&directory.0).unwrap();
        let nested = directory_path_at(
            &root,
            "bucket/nested".as_ref(),
            DirectoryOpen::CreateIfMissing,
        )
        .unwrap();
        assert_eq!(
            stat_identity(&nested).unwrap(),
            stat_identity(&open_directory_nofollow(&directory.0.join("bucket/nested")).unwrap())
                .unwrap()
        );
        std::os::unix::fs::symlink("bucket", directory.0.join("alias")).unwrap();
        std::os::unix::fs::symlink("nested", directory.0.join("bucket/link")).unwrap();
        std::fs::write(directory.0.join("regular"), b"not a directory").unwrap();
        for path in [
            "/absolute",
            "../outside",
            "bucket/./child",
            "bucket//child",
            "bucket/",
            "alias/child",
            "bucket/link/child",
            "regular/child",
        ] {
            assert!(
                directory_path_at(&root, path.as_ref(), DirectoryOpen::CreateIfMissing).is_err(),
                "{path}"
            );
        }
        assert!(!directory.0.join("bucket/nested/child").exists());
        assert!(!directory.0.join("bucket/child").exists());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn export_replace_between_directories_retains_displaced_entries() {
        let directory = super::test_support::SourceDir::new();
        let root = open_directory_nofollow(&directory.0).unwrap();
        let workspace =
            directory_path_at(&root, "work".as_ref(), DirectoryOpen::CreateIfMissing).unwrap();
        let target = directory_path_at(
            &root,
            "bucket/nested".as_ref(),
            DirectoryOpen::CreateIfMissing,
        )
        .unwrap();
        std::fs::write(directory.0.join("work/body"), b"first").unwrap();
        assert_eq!(
            durable_replace_between(&workspace, "body".as_ref(), &target, "body".as_ref(), None)
                .unwrap(),
            ReplaceOutcome::Created
        );
        assert!(!directory.0.join("work/body").exists());
        std::fs::write(directory.0.join("work/body"), b"second").unwrap();
        assert!(
            matches!(durable_replace_between(&workspace, "body".as_ref(), &target, "body".as_ref(), None), Err(FsError::Io(e)) if e.kind()==io::ErrorKind::AlreadyExists)
        );
        let expected = stat_identity(&open_regular_at(&target, "body".as_ref()).unwrap()).unwrap();
        assert!(matches!(
            durable_replace_between(
                &workspace,
                "body".as_ref(),
                &target,
                "body".as_ref(),
                Some(&expected)
            )
            .unwrap(),
            ReplaceOutcome::Replaced { .. }
        ));
        assert_eq!(
            std::fs::read(directory.0.join("work/body")).unwrap(),
            b"first"
        );
        assert_eq!(
            std::fs::read(directory.0.join("bucket/nested/body")).unwrap(),
            b"second"
        );
        std::fs::write(directory.0.join("work/body"), b"third").unwrap();
        assert!(matches!(
            durable_replace_between(
                &workspace,
                "body".as_ref(),
                &target,
                "body".as_ref(),
                Some(&expected)
            )
            .unwrap(),
            ReplaceOutcome::Conflict { displaced: Some(_) }
        ));
        assert_eq!(
            std::fs::read(directory.0.join("work/body")).unwrap(),
            b"second"
        );
        assert_eq!(
            std::fs::read(directory.0.join("bucket/nested/body")).unwrap(),
            b"third"
        );
    }

    #[cfg(unix)]
    #[test]
    fn identity_observes_nanosecond_metadata_changes() {
        use std::os::unix::fs::MetadataExt;
        let directory = super::test_support::SourceDir::new();
        std::fs::write(directory.0.join("body"), b"a").unwrap();
        let file = File::open(directory.0.join("body")).unwrap();
        let identity = stat_identity(&file).unwrap();
        let metadata = file.metadata().unwrap();
        assert_eq!(
            identity.changed_ns,
            i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec())
        );
        std::fs::write(directory.0.join("body"), b"changed").unwrap();
        assert_ne!(identity, stat_identity(&file).unwrap());
    }

    #[test]
    fn metadata_lengths_include_exact_cap_and_empty() {
        assert_eq!(checked_length(0).unwrap(), Some(0));
        assert_eq!(
            checked_length(SOURCE_METADATA_CAP).unwrap(),
            Some(SOURCE_METADATA_CAP)
        );
        assert!(matches!(
            checked_length(SOURCE_METADATA_CAP + 1),
            Err(SourceMetadataError::TooLarge)
        ));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn metadata_errno_retains_platform_distinctions() {
        #[cfg(target_os = "linux")]
        assert_eq!(
            classify(io::Error::from_raw_os_error(libc::ENODATA)).unwrap(),
            None
        );
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                classify(io::Error::from_raw_os_error(libc::ENOATTR)).unwrap(),
                None
            );
            assert!(matches!(
                classify(io::Error::from_raw_os_error(libc::ENODATA)),
                Err(SourceMetadataError::Io(_))
            ));
        }
        for errno in [libc::ENOTSUP, libc::EOPNOTSUPP] {
            assert!(matches!(
                classify(io::Error::from_raw_os_error(errno)),
                Err(SourceMetadataError::Unsupported)
            ));
        }
        for errno in [libc::EACCES, libc::EPERM, libc::EBADF] {
            let Err(SourceMetadataError::Io(error)) = classify(io::Error::from_raw_os_error(errno))
            else {
                panic!("must preserve I/O error")
            };
            assert_eq!(error.raw_os_error(), Some(errno));
        }
        for errno in [libc::ERANGE, libc::E2BIG] {
            assert!(matches!(
                classify(io::Error::from_raw_os_error(errno)),
                Err(SourceMetadataError::TooLarge)
            ));
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn descriptor_xattr_reads_are_bounded_and_read_only() {
        let directory = super::test_support::SourceDir::new();
        let path = directory.0.join("body");
        std::fs::write(&path, b"hello").unwrap();
        let file = File::open(path).unwrap();
        let mut buffer = SourceMetadataBuffer::default();
        match read_source_metadata(&file, &mut buffer) {
            Ok(None) => (),
            Err(SourceMetadataError::Unsupported) => {
                eprintln!(
                    "xattr execution unavailable: source filesystem does not support user.metadata"
                );
                return;
            }
            other => panic!("unexpected empty fixture attribute: {other:?}"),
        }
        for size in [0, 37, SOURCE_METADATA_CAP, SOURCE_METADATA_CAP + 1] {
            let value = vec![b'x'; size];
            match super::test_support::set_metadata(&file, &value) {
                Ok(true) => (),
                Ok(false) => panic!("filesystem xattr capability changed"),
                Err(error) => {
                    assert!(size >= SOURCE_METADATA_CAP);
                    assert!(matches!(
                        error.raw_os_error(),
                        Some(libc::E2BIG | libc::ENOSPC | libc::ERANGE)
                    ));
                    eprintln!("xattr write ceiling for {size} bytes: {error}");
                    continue;
                }
            }
            let before = file.metadata().unwrap();
            if size <= SOURCE_METADATA_CAP {
                assert_eq!(
                    read_source_metadata(&file, &mut buffer).unwrap(),
                    Some(value.as_slice())
                );
            } else {
                assert!(matches!(
                    read_source_metadata(&file, &mut buffer),
                    Err(SourceMetadataError::TooLarge)
                ));
            }
            assert_eq!(
                before.modified().unwrap(),
                file.metadata().unwrap().modified().unwrap()
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod test_support {
    //! Test-only snapshot creation; production source readers never call this module.
    use std::{fs, io, path::PathBuf};

    pub(crate) struct SourceDir(pub PathBuf);
    impl SourceDir {
        pub(crate) fn new() -> Self {
            let mut random = [0; 8];
            getrandom::fill(&mut random).expect("test directory randomness");
            let path = std::env::temp_dir().join(format!(
                "gcs-mimic-source-{}-{}",
                std::process::id(),
                u64::from_ne_bytes(random)
            ));
            fs::create_dir(&path).expect("test source directory");
            Self(path)
        }
    }
    impl Drop for SourceDir {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_dir_all(&self.0) {
                if std::thread::panicking() {
                    eprintln!("source fixture cleanup failed: {error}");
                } else {
                    panic!("source fixture cleanup failed: {error}");
                }
            }
        }
    }

    #[cfg_attr(
        any(target_os = "linux", target_os = "macos"),
        expect(
            unsafe_code,
            reason = "descriptor xattrs are writable only through fsetxattr"
        )
    )]
    pub(crate) fn set_metadata(file: &fs::File, bytes: &[u8]) -> io::Result<bool> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: the test owns the live descriptor; the literal key and input
            // allocation remain valid for the synchronous Linux/macOS ABI call.
            let result = unsafe {
                #[cfg(target_os = "linux")]
                {
                    libc::fsetxattr(
                        file.as_raw_fd(),
                        c"user.metadata".as_ptr(),
                        bytes.as_ptr().cast(),
                        bytes.len(),
                        0,
                    )
                }
                #[cfg(target_os = "macos")]
                {
                    libc::fsetxattr(
                        file.as_raw_fd(),
                        c"user.metadata".as_ptr(),
                        bytes.as_ptr().cast(),
                        bytes.len(),
                        0,
                        0,
                    )
                }
            };
            if result == 0 {
                return Ok(true);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOTSUP)
                || error.raw_os_error() == Some(libc::EOPNOTSUPP)
            {
                Ok(false)
            } else {
                Err(error)
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (file, bytes);
            Ok(false)
        }
    }
}
