//! The calls only Apple platforms make: `F_BARRIERFSYNC`, and on macOS `fclonefileat`.

use std::{fs::File, io, os::fd::AsRawFd};

/// The Apple [`super::sync_ordered`]: `F_BARRIERFSYNC`, or the full flush of `sync_all` on a
/// filesystem without barriers.
#[expect(
    unsafe_code,
    reason = "std has no F_BARRIERFSYNC, so it is only reachable through fcntl"
)]
pub(super) fn sync_ordered(file: &File) -> io::Result<()> {
    loop {
        // SAFETY: the descriptor stays open for the whole call, which takes no pointers.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_BARRIERFSYNC) } != -1 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => {}
            Some(libc::ENOTSUP | libc::EOPNOTSUPP | libc::EINVAL) => return file.sync_all(),
            _ => return Err(error),
        }
    }
}

/// The macOS [`super::clone_into`]: `fclonefileat` creates the entry only when it clones.
#[cfg(target_os = "macos")]
#[expect(unsafe_code, reason = "fclonefileat has no safe std equivalent")]
pub(super) fn clone_into(
    src: &File,
    directory: &File,
    name: &std::ffi::OsStr,
) -> Result<(), super::CloneFailure> {
    use super::{
        CloneFailure, FsError,
        unix::{FileOpen, component, copy_fallback, open_at},
    };
    let name = component(name)?;
    #[cfg(test)]
    if super::CLONE_UNSUPPORTED.get() {
        return Err(CloneFailure::NotCreated(FsError::Unsupported));
    }
    // SAFETY: live descriptors and a validated destination component; clone creates a new entry.
    if unsafe { libc::fclonefileat(src.as_raw_fd(), directory.as_raw_fd(), name.as_ptr(), 0) } == 0
    {
        #[cfg(test)]
        if super::FAIL_CLONE_SYNC.replace(false) {
            return Err(CloneFailure::Created(
                io::Error::from(io::ErrorKind::Other).into(),
            ));
        }
        open_at(directory, &name, FileOpen::Existing)
            .and_then(|file| file.sync_all())
            .map_err(|error| CloneFailure::Created(error.into()))?;
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if !copy_fallback(&error) {
        return Err(error.into());
    }
    Err(CloneFailure::NotCreated(FsError::Unsupported))
}
