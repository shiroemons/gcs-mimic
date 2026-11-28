//! The calls only Linux makes: `statx` past an attribute cache, `FICLONE` and `preadv2`.

use super::{
    CloneFailure, FileIdentity, FsError,
    unix::{FileOpen, component, copy_fallback, open_at},
};
use std::{ffi::OsStr, fs::File, io, os::fd::AsRawFd};

/// The identity of `file` as its filesystem reports it now. A Linux FUSE mount, virtiofs and
/// therefore Docker Desktop's bind mounts included, and an NFS mount answer `fstat` from an
/// attribute cache for a while, which a rename made on the host or another client does not
/// invalidate; `AT_STATX_FORCE_SYNC` asks the filesystem instead, at the cost of one attribute
/// request. A kernel or seccomp profile without `statx` (`ENOSYS`, `EPERM`), or a filesystem
/// that leaves a field out of its answer, is answered by `fstat` as before.
#[expect(
    unsafe_code,
    reason = "statx with AT_STATX_FORCE_SYNC has no safe std equivalent"
)]
pub(super) fn uncached_identity(file: &File) -> io::Result<FileIdentity> {
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
    Ok(FileIdentity {
        device: libc::makedev(status.stx_dev_major, status.stx_dev_minor),
        inode: status.stx_ino,
        size: status.stx_size,
        modified_ns: nanos(status.stx_mtime),
        changed_ns: nanos(status.stx_ctime),
    })
}

/// Clones `src` into `dst`, an empty regular file open for writing, and syncs it. `Ok(false)`
/// means the filesystem cannot clone; `dst` is then still empty.
#[expect(unsafe_code, reason = "FICLONE has no safe std equivalent")]
fn clone_onto(src: &File, dst: &File) -> io::Result<bool> {
    #[cfg(test)]
    if super::FAIL_CLONE_SYNC.replace(false) {
        return Err(io::ErrorKind::Other.into());
    }
    #[cfg(test)]
    if super::CLONE_UNSUPPORTED.get() {
        return Ok(false);
    }
    // SAFETY: FICLONE receives the two live regular-file descriptors, with an empty target.
    if unsafe { libc::ioctl(dst.as_raw_fd(), libc::FICLONE as _, src.as_raw_fd()) } == 0 {
        dst.sync_all()?;
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if copy_fallback(&error) {
        Ok(false)
    } else {
        Err(error)
    }
}

/// The Linux [`super::clone_into`]: creates the entry and clones into it, removing the entry
/// again when the filesystem cannot clone.
#[expect(unsafe_code, reason = "unlinkat has no safe std equivalent")]
pub(super) fn clone_into(src: &File, directory: &File, name: &OsStr) -> Result<(), CloneFailure> {
    let name = component(name)?;
    let dst = open_at(directory, &name, FileOpen::CreateNew)?;
    if clone_onto(src, &dst).map_err(|error| CloneFailure::Created(error.into()))? {
        return Ok(());
    }
    // SAFETY: only this call owns the newly created target; the component and directory are live.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(CloneFailure::Created(io::Error::last_os_error().into()));
    }
    Err(CloneFailure::NotCreated(FsError::Unsupported))
}

/// The Linux [`super::read_cached_at`]: `preadv2` with `RWF_NOWAIT`, which glibc and musl wrap.
#[cfg(any(target_env = "gnu", target_env = "musl"))]
#[expect(unsafe_code, reason = "std has no wrapper for preadv2")]
pub(super) fn read_cached_at(file: &File, bytes: &mut [u8], offset: u64) -> Option<usize> {
    let vector = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let offset = libc::off_t::try_from(offset).ok()?;
    // SAFETY: the one vector describes `bytes`, which stays exclusively borrowed for the call and
    // receives at most its length, and `file` keeps the descriptor open.
    let read = unsafe {
        libc::preadv2(
            file.as_raw_fd(),
            &raw const vector,
            1,
            offset,
            libc::RWF_NOWAIT,
        )
    };
    usize::try_from(read).ok().filter(|read| *read > 0)
}
