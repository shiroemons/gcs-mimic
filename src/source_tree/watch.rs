//! Native notifications trigger bounded descriptor-relative rescans.
use super::TreeLimits;
use crate::storage::admin::AdminError;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::AsRawFd;
use std::{fmt, fs::File, io, path::Path};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use tokio::io::unix::AsyncFd;
#[cfg(target_os = "macos")]
mod fsevents;
#[cfg(target_os = "macos")]
pub(crate) use fsevents::Native;
#[cfg(target_os = "linux")]
mod inotify;
#[cfg(target_os = "linux")]
pub(crate) use inotify::Native;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) struct Native;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    Content,
    Structure,
}

/// Why a native watch could not be built.
#[derive(Debug)]
pub(crate) enum Error {
    /// A native setup step failed: a descriptor, a system call or the event service.
    Io(io::Error),
    /// Listing the tree failed, or the tree passes a limit; the retry decision reads the failure
    /// itself, such as exhausted descriptors or an invalid tree.
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(dead_code, reason = "only the Linux and macOS watches list the tree")
    )]
    Tree(AdminError),
    /// The tree has more directories than the watch may add.
    #[cfg_attr(
        all(not(target_os = "linux"), not(test)),
        expect(
            dead_code,
            reason = "only the Linux watch adds one watch per directory"
        )
    )]
    Limit(WatchLimit),
}

/// The limit that refused a directory's watch, so the directory would go unobserved. No retry
/// can lift either one: the operator raises the limit or reduces the tree.
#[derive(Debug)]
#[cfg_attr(
    all(not(target_os = "linux"), not(test)),
    expect(
        dead_code,
        reason = "only the Linux watch adds one watch per directory"
    )
)]
pub(crate) enum WatchLimit {
    /// `max_sync_entries` allows this many watched directories, and all of them are in use.
    Configured { directories: usize },
    /// The kernel refused the watch with `ENOSPC`: its inotify watch budget is spent, not the
    /// disk.
    Kernel(io::Error),
}

impl fmt::Display for WatchLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configured { directories } => write!(
                f,
                "sync native watch limit ({directories} directories) exceeded; raise max_sync_entries or reduce the tree instead of leaving directories unobserved"
            ),
            Self::Kernel(_) => f.write_str(
                "the kernel's inotify watch limit is exhausted; raise fs.inotify.max_user_watches on the host or reduce the tree",
            ),
        }
    }
}

impl std::error::Error for WatchLimit {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Configured { .. } => None,
            Self::Kernel(error) => Some(error),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => fmt::Display::fmt(error, f),
            Self::Tree(error) => fmt::Display::fmt(error, f),
            Self::Limit(limit) => fmt::Display::fmt(limit, f),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Tree(error) => Some(error),
            Self::Limit(limit) => Some(limit),
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}
pub(crate) struct Watch {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    ready: AsyncFd<Native>,
}
impl Watch {
    pub(crate) fn try_changed(&self) -> io::Result<Option<Change>> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        match self.ready.get_ref().drain() {
            Ok(change) => Ok(Some(change)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(error),
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(io::ErrorKind::Unsupported.into())
        }
    }
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    #[expect(
        unsafe_code,
        reason = "the witness owns an immutable duplicate descriptor until its readiness registration is dropped"
    )]
    pub(crate) fn notification_witness(&self) -> io::Result<AsyncFd<std::os::fd::OwnedFd>> {
        let queue = self.ready.get_ref().duplicate_queue()?;
        // SAFETY: OwnedFd keeps the duplicate descriptor unchanged for the registration's lifetime.
        unsafe { AsyncFd::register_with_interest(queue, tokio::io::Interest::READABLE) }
            .map_err(io::Error::from)
    }
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(
            clippy::needless_pass_by_value,
            reason = "supported hosts transfer Native ownership into AsyncFd"
        )
    )]
    #[cfg_attr(
        any(target_os = "linux", target_os = "macos"),
        expect(
            unsafe_code,
            reason = "Native keeps its registered descriptor immutable until AsyncFd is dropped"
        )
    )]
    pub(crate) fn new(native: Native) -> io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            // SAFETY: Native uniquely owns its descriptor and never replaces it while registered.
            let ready =
                unsafe { AsyncFd::register_with_interest(native, tokio::io::Interest::READABLE) }
                    .map_err(io::Error::from)?;
            Ok(Self { ready })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = native;
            Err(io::ErrorKind::Unsupported.into())
        }
    }
    pub(crate) async fn changed(&self) -> io::Result<Change> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        loop {
            let mut ready = self.ready.readable().await?;
            match ready.try_io(|fd| fd.get_ref().drain()) {
                Ok(result) => return result,
                Err(_) => continue,
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(io::ErrorKind::Unsupported.into())
        }
    }
}
pub(crate) fn build(root: &File, path: &Path, limits: TreeLimits) -> Result<Native, Error> {
    #[cfg(target_os = "macos")]
    {
        fsevents::build(root, path, limits)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = path;
        inotify::build(root, limits)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (root, path, limits);
        Err(Error::Io(io::ErrorKind::Unsupported.into()))
    }
}
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
