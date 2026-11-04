use super::*;
use crate::fsutil;
use std::{
    collections::VecDeque,
    ffi::OsString,
    os::fd::{FromRawFd, OwnedFd, RawFd},
};
pub(super) const MAX_WATCHES: usize = 1022;
pub(crate) struct Native {
    queue: OwnedFd,
    pub(super) count: usize,
}
impl AsRawFd for Native {
    fn as_raw_fd(&self) -> RawFd {
        self.queue.as_raw_fd()
    }
}

pub(super) fn build(root: &File) -> io::Result<Native> {
    let mut native = Native::new()?;
    native.register(root)?;
    let mut stack = vec![(
        root.try_clone()?,
        None::<OsString>,
        VecDeque::<OsString>::new(),
        false,
    )];
    while let Some((directory, after, buffered, exhausted)) = stack.last_mut() {
        if buffered.is_empty() && !*exhausted {
            let page = fsutil::directory_names_after(directory, after.as_deref(), 256)?;
            *exhausted = page.len() < 256;
            *after = page.last().cloned().or(after.take());
            *buffered = page.into();
        }
        let Some(name) = buffered.pop_front() else {
            stack.pop();
            continue;
        };
        let depth = stack.len();
        if depth == 1 && crate::sync::mapping::is_reserved_root_entry(&name) {
            continue;
        }
        let directory = &stack
            .last()
            .ok_or_else(|| io::Error::other("missing watch frame"))?
            .0;
        match fsutil::child_directory(directory, &name, fsutil::DirectoryOpen::Existing) {
            Ok(fsutil::Child::Directory { file, .. }) => {
                if depth >= 128 {
                    return Err(io::Error::other(
                        "sync watch tree exceeds 128 directory levels",
                    ));
                }
                native.register(&file)?;
                stack.push((file, None, VecDeque::new(), false));
            }
            Ok(_) => {}
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || crate::sync::tree::structural_entry_error(&error) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(native)
}
impl Native {
    #[expect(
        unsafe_code,
        reason = "inotify has no standard-library queue constructor"
    )]
    pub(super) fn new() -> io::Result<Self> {
        // SAFETY: the syscall returns a fresh owned descriptor and takes no pointers.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            // SAFETY: inotify_init1 above returned a valid, unowned fd; this transfers
            // ownership exactly once to the value responsible for closing it.
            queue: unsafe { OwnedFd::from_raw_fd(fd) },
            count: 0,
        })
    }
    #[expect(
        unsafe_code,
        reason = "inotify watches must be registered through libc"
    )]
    pub(super) fn register(&mut self, file: &File) -> io::Result<()> {
        if self.count == MAX_WATCHES {
            return Err(io::Error::other(
                "sync native watch limit (1022 directories) exceeded; reduce the tree instead of leaving directories unobserved",
            ));
        }
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
        // SAFETY: the terminated proc path refers to the descriptor kept live through this call.
        if unsafe { libc::inotify_add_watch(self.queue.as_raw_fd(), path.as_ptr(), mask) } < 0 {
            return Err(io::Error::last_os_error());
        }
        self.count += 1;
        Ok(())
    }
    #[expect(
        unsafe_code,
        reason = "nonblocking reads and unaligned inotify records require libc"
    )]
    pub(super) fn drain(&self) -> io::Result<Change> {
        let mut change = None;
        let mut bytes = [0u8; 8192];
        loop {
            // SAFETY: bytes is a writable bounded allocation and the queue is nonblocking.
            let count = unsafe {
                libc::read(
                    self.queue.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if count < 0 {
                let error = io::Error::last_os_error();
                return if error.kind() == io::ErrorKind::WouldBlock {
                    change.ok_or(error)
                } else {
                    Err(error)
                };
            }
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let mut offset = 0;
            while offset + size_of::<libc::inotify_event>() <= count as usize {
                // SAFETY: bounds above cover the complete fixed header; kernel records may be unaligned.
                let event = unsafe {
                    std::ptr::read_unaligned(
                        bytes.as_ptr().add(offset).cast::<libc::inotify_event>(),
                    )
                };
                let length = size_of::<libc::inotify_event>() + event.len as usize;
                if length > count as usize - offset {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                if structural(event.mask) {
                    change = Some(Change::Structure)
                } else {
                    change.get_or_insert(Change::Content);
                }
                offset += length;
            }
            if offset != count as usize {
                return Err(io::ErrorKind::InvalidData.into());
            }
        }
    }
}
fn structural(mask: u32) -> bool {
    mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_IGNORED | libc::IN_Q_OVERFLOW) != 0
        || mask & libc::IN_ISDIR != 0
            && mask
                & (libc::IN_CREATE
                    | libc::IN_MOVED_TO
                    | libc::IN_MOVED_FROM
                    | libc::IN_DELETE
                    | libc::IN_ATTRIB)
                != 0
}
