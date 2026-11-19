use super::*;
use crate::fsutil;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
pub(crate) struct Native {
    queue: OwnedFd,
    pub(super) count: usize,
    max_watches: Option<usize>,
}
impl AsRawFd for Native {
    fn as_raw_fd(&self) -> RawFd {
        self.queue.as_raw_fd()
    }
}

pub(super) fn build(root: &File, limits: TreeLimits) -> Result<Native, Error> {
    // A watch holds no descriptor, only kernel memory, so nothing but the tree itself bounds them:
    // each watched directory below the root is an entry that the scan counts too. Without an entry
    // limit the kernel's own budget decides, and `register` explains how to raise it.
    let mut native = Native::new(
        limits
            .entries
            .map(|entries| entries.get().saturating_add(1)),
    )?;
    native.register(root)?;
    let mut stack = vec![WatchFrame::new(root.try_clone()?)];
    loop {
        let depth = stack.len();
        let Some(frame) = stack.last_mut() else {
            break;
        };
        let next = frame
            .names
            .next_name(&frame.directory, limits.directory_names())
            .map_err(|error| {
                if error.kind() == io::ErrorKind::InvalidData {
                    Error::Tree(limits.too_many_entries())
                } else {
                    Error::Io(error)
                }
            })?;
        let Some(name) = next else {
            stack.pop();
            continue;
        };
        if depth == 1 && crate::sync::mapping::is_reserved_root_entry(&name) {
            continue;
        }
        match fsutil::Lookup::unprobed().directory(
            &frame.directory,
            &name,
            fsutil::DirectoryOpen::Existing,
        ) {
            Ok(fsutil::Child::Directory { file, .. }) => {
                if depth > limits.deepest_directory() {
                    return Err(Error::Tree(limits.too_deep()));
                }
                native.register(&file)?;
                stack.push(WatchFrame::new(file));
            }
            Ok(fsutil::Child::Missing | fsutil::Child::Alias) => {}
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || crate::sync::tree::structural_entry_error(&error) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(native)
}

/// One open directory of the watch walk and the names in it still to visit.
struct WatchFrame {
    directory: File,
    names: fsutil::DirectoryNames,
}

impl WatchFrame {
    fn new(directory: File) -> Self {
        Self {
            directory,
            names: fsutil::DirectoryNames::default(),
        }
    }
}
impl Native {
    #[cfg(test)]
    pub(super) fn duplicate_queue(&self) -> io::Result<OwnedFd> {
        self.queue.try_clone()
    }
    #[expect(
        unsafe_code,
        reason = "inotify has no standard-library queue constructor"
    )]
    pub(super) fn new(max_watches: Option<usize>) -> io::Result<Self> {
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
            max_watches,
        })
    }
    #[expect(
        unsafe_code,
        reason = "inotify watches must be registered through libc"
    )]
    pub(super) fn register(&mut self, file: &File) -> Result<(), Error> {
        if let Some(directories) = self.max_watches
            && self.count == directories
        {
            return Err(Error::Limit(WatchLimit::Configured { directories }));
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
            let error = io::Error::last_os_error();
            return Err(if error.raw_os_error() == Some(libc::ENOSPC) {
                Error::Limit(WatchLimit::Kernel(error))
            } else {
                Error::Io(error)
            });
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
            let count = usize::try_from(count).map_err(io::Error::other)?;
            let mut offset = 0;
            while offset + size_of::<libc::inotify_event>() <= count {
                // SAFETY: bounds above cover the complete fixed header; kernel records may be unaligned.
                let event = unsafe {
                    std::ptr::read_unaligned(
                        bytes.as_ptr().add(offset).cast::<libc::inotify_event>(),
                    )
                };
                let length = size_of::<libc::inotify_event>() + event.len as usize;
                if length > count - offset {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                if structural(event.mask) {
                    change = Some(Change::Structure)
                } else {
                    change.get_or_insert(Change::Content);
                }
                offset += length;
            }
            if offset != count {
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
