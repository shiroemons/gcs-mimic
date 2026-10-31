//! Descriptor-relative native observation; every notification requests a complete bounded rescan.

use std::{fs::File, io};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use {
    crate::fsutil,
    std::{
        ffi::OsString,
        os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    },
    tokio::io::unix::AsyncFd,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
// Two queues overlap during rebuild; leave room for both and a candidate descriptor.
const MAX_WATCHES: usize = 1022;

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

pub(crate) struct Native {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    queue: OwnedFd,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    files: Vec<File>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    count: usize,
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
            reason = "Supported targets move Native into AsyncFd; the unsupported constructor keeps the same ownership contract"
        )
    )]
    #[cfg_attr(
        any(target_os = "linux", target_os = "macos"),
        expect(
            unsafe_code,
            reason = "Native owns an immutable queue descriptor for the entire AsyncFd lifetime"
        )
    )]
    pub(crate) fn new(native: Native) -> io::Result<Self> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            // SAFETY: Native uniquely owns the queue and never changes its descriptor while registered.
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

    pub(crate) async fn changed(&self) -> io::Result<()> {
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

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl AsRawFd for Native {
    fn as_raw_fd(&self) -> RawFd {
        self.queue.as_raw_fd()
    }
}

pub(crate) fn build(root: &File) -> io::Result<Native> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut native = Native::new()?;
        native.register(root.try_clone()?)?;
        let mut stack = vec![(0usize, None::<OsString>)];
        while let Some((index, after)) = stack.last_mut() {
            let Some(name) =
                fsutil::directory_names_after(&native.files[*index], after.as_deref(), 1)?
                    .into_iter()
                    .next()
            else {
                stack.pop();
                continue;
            };
            *after = Some(name.clone());
            if stack.len() == 1
                && matches!(
                    name.to_str(),
                    Some(crate::sync::mapping::MARKER | crate::sync::mapping::WORKSPACE)
                )
            {
                continue;
            }
            let index = stack
                .last()
                .ok_or_else(|| io::Error::other("watch traversal lost its directory"))?
                .0;
            match fsutil::directory_at(&native.files[index], &name, fsutil::DirectoryOpen::Existing)
            {
                Ok(child) => {
                    if stack.len() >= 128 {
                        return Err(io::Error::other(
                            "sync watch tree exceeds 128 directory levels",
                        ));
                    }
                    let index = native.files.len();
                    native.register(child)?;
                    stack.push((index, None));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) if structural(&error) => {
                    #[cfg(target_os = "macos")]
                    match fsutil::open_regular_at(&native.files[index], &name) {
                        Ok(file) => native.register(file)?,
                        Err(error)
                            if error.kind() == io::ErrorKind::NotFound || structural(&error) => {}
                        Err(error) => return Err(error),
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(native)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = root;
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn structural(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotADirectory | io::ErrorKind::InvalidInput
    ) || error.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Native {
    #[expect(
        unsafe_code,
        reason = "native event queues have no standard-library constructor"
    )]
    fn new() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        // SAFETY: this constructor takes no pointers and returns a newly owned descriptor.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        #[cfg(target_os = "macos")]
        // SAFETY: kqueue takes no arguments and returns a newly owned descriptor.
        let fd = unsafe { libc::kqueue() };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful syscall returned a unique descriptor owned by this value.
        let queue = unsafe { OwnedFd::from_raw_fd(fd) };
        #[cfg(target_os = "macos")]
        // SAFETY: queue owns this live descriptor; this command changes only its exec flag.
        if unsafe { libc::fcntl(queue.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            queue,
            files: Vec::new(),
            count: 0,
        })
    }

    #[expect(
        unsafe_code,
        reason = "registering descriptor-bound native watches requires libc"
    )]
    fn register(&mut self, file: File) -> io::Result<()> {
        if self.count == MAX_WATCHES {
            return Err(io::Error::other(
                "sync native watch limit (1022 entries, 2048 descriptors during rebuild) exceeded; reduce the tree instead of leaving entries unobserved",
            ));
        }
        #[cfg(target_os = "linux")]
        {
            let path = std::ffi::CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            let mask = libc::IN_ONLYDIR
                | libc::IN_ATTRIB
                | libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_MODIFY
                | libc::IN_CLOSE_WRITE
                | libc::IN_MOVED_FROM
                | libc::IN_MOVED_TO
                | libc::IN_DELETE_SELF
                | libc::IN_MOVE_SELF;
            // SAFETY: the terminated proc path refers to a descriptor kept live through this call.
            if unsafe { libc::inotify_add_watch(self.queue.as_raw_fd(), path.as_ptr(), mask) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(target_os = "macos")]
        {
            let event = libc::kevent {
                ident: file.as_raw_fd() as usize,
                filter: libc::EVFILT_VNODE,
                flags: libc::EV_ADD | libc::EV_CLEAR,
                fflags: libc::NOTE_DELETE
                    | libc::NOTE_WRITE
                    | libc::NOTE_EXTEND
                    | libc::NOTE_ATTRIB
                    | libc::NOTE_LINK
                    | libc::NOTE_RENAME
                    | libc::NOTE_REVOKE,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            let timeout = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: event is a valid single change; no output array or wait is requested.
            if unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    &event,
                    1,
                    std::ptr::null_mut(),
                    0,
                    &timeout,
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        self.files.push(file);
        self.count += 1;
        Ok(())
    }

    #[expect(
        unsafe_code,
        reason = "bounded nonblocking native queue reads have no std equivalent"
    )]
    fn drain(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            let mut bytes = [0u8; 8192];
            // SAFETY: bytes is a writable bounded buffer and the descriptor is nonblocking.
            let count = unsafe {
                libc::read(
                    self.queue.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            // A full rescan also reconciles IN_Q_OVERFLOW and coalesced or stale names.
            Ok(())
        }
        #[cfg(target_os = "macos")]
        {
            let blank = libc::kevent {
                ident: 0,
                filter: 0,
                flags: 0,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            };
            let mut events = [blank; 64];
            let timeout = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            // SAFETY: events has 64 writable entries; a zero timeout makes this nonblocking.
            let count = unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    events.len() as i32,
                    &timeout,
                )
            };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            if count == 0 {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            for event in &events[..count as usize] {
                if event.flags & libc::EV_ERROR != 0 {
                    return Err(io::Error::from_raw_os_error(event.data as i32));
                }
            }
            Ok(())
        }
    }
}
