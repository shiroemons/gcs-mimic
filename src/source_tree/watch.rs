//! Native notifications trigger bounded descriptor-relative rescans.
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::fd::AsRawFd;
use std::{fs::File, io, path::Path};
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
pub(crate) struct Watch {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    ready: AsyncFd<Native>,
}
impl Watch {
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
pub(crate) fn build(root: &File, path: &Path) -> io::Result<Native> {
    #[cfg(target_os = "macos")]
    {
        fsevents::build(root, path)
    }
    #[cfg(target_os = "linux")]
    {
        let _ = path;
        inotify::build(root)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (root, path);
        Err(io::ErrorKind::Unsupported.into())
    }
}
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
