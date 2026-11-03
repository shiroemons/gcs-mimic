use super::*;
use std::{
    ffi::{CStr, c_void},
    os::{
        fd::{FromRawFd, OwnedFd, RawFd},
        unix::ffi::OsStrExt,
    },
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

type Callback =
    unsafe extern "C" fn(*const c_void, *mut c_void, usize, *mut c_void, *const u32, *const u64);
#[repr(C)]
struct Context {
    version: isize,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    description: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
}
#[expect(
    unsafe_code,
    reason = "FSEvents and CoreFoundation expose C framework functions without a standard-library wrapper"
)]
#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(
        allocator: *const c_void,
        callback: Callback,
        context: *mut Context,
        paths: *const c_void,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> *mut c_void;
    fn FSEventStreamSetDispatchQueue(stream: *mut c_void, queue: *mut c_void);
    fn FSEventStreamStart(stream: *mut c_void) -> u8;
    fn FSEventStreamStop(stream: *mut c_void);
    fn FSEventStreamInvalidate(stream: *mut c_void);
    fn FSEventStreamRelease(stream: *mut c_void);
}
#[expect(
    unsafe_code,
    reason = "CoreFoundation owns the path strings passed to FSEvents"
)]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        allocator: *const c_void,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: u8,
    ) -> *const c_void;
    fn CFArrayCreate(
        allocator: *const c_void,
        values: *const *const c_void,
        count: isize,
        callbacks: *const c_void,
    ) -> *const c_void;
    fn CFRelease(object: *const c_void);
    static kCFTypeArrayCallBacks: c_void;
}
#[expect(
    unsafe_code,
    reason = "the serial dispatch queue must drain callbacks before their Rust context is released"
)]
unsafe extern "C" {
    fn dispatch_queue_create(label: *const libc::c_char, attributes: *const c_void) -> *mut c_void;
    fn dispatch_sync_f(
        queue: *mut c_void,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
    fn dispatch_release(object: *mut c_void);
}

struct Stream {
    stream: NonNull<c_void>,
    queue: NonNull<c_void>,
}
#[expect(
    unsafe_code,
    reason = "FSEventStream references and dispatch queues may be transferred between threads"
)]
// SAFETY: stream operations are thread-safe; only its exclusive owner stops it during Drop.
unsafe impl Send for Stream {}
#[expect(
    unsafe_code,
    reason = "shared stream ownership has no mutable Rust data accessed by callbacks"
)]
// SAFETY: shared references never mutate the stream, and callbacks touch only atomic state and a pipe.
unsafe impl Sync for Stream {}
impl Drop for Stream {
    #[expect(
        unsafe_code,
        reason = "stopping, invalidating and draining the dispatch queue ends every callback before context destruction"
    )]
    fn drop(&mut self) {
        unsafe extern "C" fn barrier(_: *mut c_void) {}
        // SAFETY: this owner retains the scheduled stream and serial queue until the barrier completes.
        unsafe {
            FSEventStreamStop(self.stream.as_ptr());
            FSEventStreamInvalidate(self.stream.as_ptr());
            dispatch_sync_f(self.queue.as_ptr(), std::ptr::null_mut(), barrier);
            FSEventStreamRelease(self.stream.as_ptr());
            dispatch_release(self.queue.as_ptr());
        }
    }
}
struct CallbackContext {
    notify: OwnedFd,
    root: Vec<u8>,
    structure: AtomicBool,
    failed: AtomicBool,
}
pub(crate) struct Native {
    // Stop and drain callbacks before either end of their pipe closes.
    _stream: Stream,
    context: Box<CallbackContext>,
    queue: OwnedFd,
}
impl AsRawFd for Native {
    fn as_raw_fd(&self) -> RawFd {
        self.queue.as_raw_fd()
    }
}

fn ignored(root: &[u8], path: &[u8]) -> bool {
    let Some(relative) = path
        .strip_prefix(root)
        .and_then(|rest| rest.strip_prefix(b"/"))
    else {
        return false;
    };
    let name = relative
        .split(|byte| *byte == b'/')
        .next()
        .unwrap_or_default();
    crate::sync::mapping::is_reserved_root_entry(std::ffi::OsStr::from_bytes(name))
}

#[expect(
    unsafe_code,
    reason = "FSEvents supplies valid path and flag arrays for the duration of each callback"
)]
unsafe extern "C" fn callback(
    _: *const c_void,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    _: *const u64,
) {
    // SAFETY: Native keeps the boxed context live through stream invalidation and its queue barrier.
    let context = unsafe { &*info.cast::<CallbackContext>() };
    let paths = paths.cast::<*const libc::c_char>();
    let mut notify = false;
    for index in 0..count {
        // SAFETY: FSEvents gives count valid NUL-terminated path pointers and count flag values.
        let (path, flags) = unsafe {
            (
                CStr::from_ptr(*paths.add(index)).to_bytes(),
                *flags.add(index),
            )
        };
        if flags & 0x10 != 0 || ignored(&context.root, path) {
            continue;
        }
        // Overflow/root changes need a fresh stream; directory permission changes require a fresh access check.
        if flags & 0x2f != 0 || flags & 0x20000 != 0 && flags & 0x8400 != 0 {
            context.structure.store(true, Ordering::Release);
        }
        notify = true;
    }
    if notify {
        let byte = 1u8;
        // SAFETY: notify is a live nonblocking pipe; the read end outlives all callbacks.
        let written =
            unsafe { libc::write(context.notify.as_raw_fd(), (&raw const byte).cast(), 1) };
        if written < 0 && io::Error::last_os_error().kind() != io::ErrorKind::WouldBlock {
            context.failed.store(true, Ordering::Release);
        }
    }
}

#[expect(
    unsafe_code,
    reason = "a nonblocking CLOEXEC pipe is the AsyncFd notification bridge"
)]
fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: fds has space for both newly allocated descriptors.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe transferred one read descriptor and one distinct write descriptor.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    for fd in [&read, &write] {
        // SAFETY: each uniquely owned pipe descriptor remains live while its flags are configured.
        if unsafe {
            libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) < 0
                || libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) < 0
        } {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((read, write))
}

struct CfObject(NonNull<c_void>);
impl Drop for CfObject {
    #[expect(
        unsafe_code,
        reason = "CoreFoundation Create ownership must be balanced by CFRelease"
    )]
    fn drop(&mut self) {
        // SAFETY: this guard owns exactly one Create reference.
        unsafe {
            CFRelease(self.0.as_ptr());
        }
    }
}
fn allocated(pointer: *mut c_void) -> io::Result<NonNull<c_void>> {
    NonNull::new(pointer).ok_or_else(|| io::Error::from_raw_os_error(libc::ENOMEM))
}

#[expect(
    unsafe_code,
    reason = "FSEvents setup requires CoreFoundation, dispatch and filesystem locality syscalls"
)]
pub(super) fn build(root: &File, path: &Path) -> io::Result<Native> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: stat has enough writable space, and root remains open throughout setup.
    if unsafe { libc::fstatfs(root.as_raw_fd(), stat.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstatfs initialized every field on success.
    if unsafe { stat.assume_init() }.f_flags & libc::MNT_LOCAL as u32 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "sync FSEvents observation requires a local filesystem",
        ));
    }
    let mut scan = crate::sync::tree::TreeScan::new();
    while !scan.is_empty() {
        let (next, page) = scan.page(root).map_err(io::Error::other)?;
        scan = next;
        if let Some(kind) = page.into_iter().find_map(|entry| entry.error) {
            return Err(kind.into());
        }
    }
    let bytes = path.as_os_str().as_bytes();
    let length = isize::try_from(bytes.len())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    // SAFETY: bytes is a live UTF-8 path; CoreFoundation copies this input into a newly retained string.
    let string = CfObject(allocated(
        unsafe { CFStringCreateWithBytes(std::ptr::null(), bytes.as_ptr(), length, 0x08000100, 0) }
            .cast_mut(),
    )?);
    let value = string.0.as_ptr().cast_const();
    // SAFETY: the single live CFString and standard retaining callbacks define a valid one-element array.
    let array = CfObject(allocated(
        unsafe {
            CFArrayCreate(
                std::ptr::null(),
                &value,
                1,
                (&raw const kCFTypeArrayCallBacks).cast(),
            )
        }
        .cast_mut(),
    )?);
    let (queue, notify) = pipe()?;
    let mut context = Box::new(CallbackContext {
        notify,
        root: bytes.to_vec(),
        structure: AtomicBool::new(false),
        failed: AtomicBool::new(false),
    });
    let mut info = Context {
        version: 0,
        info: (&raw mut *context).cast(),
        retain: None,
        release: None,
        description: None,
    };
    // SAFETY: callback context remains boxed until after Stream::drop drains the queue; FSEvents retains paths.
    let stream = allocated(unsafe {
        FSEventStreamCreate(
            std::ptr::null(),
            callback,
            &mut info,
            array.0.as_ptr(),
            u64::MAX,
            0.05,
            0x16,
        )
    })?;
    // SAFETY: a null attributes value creates a new serial queue with this static label.
    let dispatch =
        unsafe { dispatch_queue_create(c"gcs-mimic-sync-events".as_ptr(), std::ptr::null()) };
    let dispatch = match allocated(dispatch) {
        Ok(queue) => queue,
        Err(error) => {
            // SAFETY: the newly created unscheduled stream has no callbacks and one retained reference.
            unsafe {
                FSEventStreamRelease(stream.as_ptr());
            }
            return Err(error);
        }
    };
    // SAFETY: both new owned references remain live; scheduling precedes start and eventual invalidation.
    unsafe {
        FSEventStreamSetDispatchQueue(stream.as_ptr(), dispatch.as_ptr());
    }
    let stream = Stream {
        stream,
        queue: dispatch,
    };
    // SAFETY: the stream is scheduled and its context and paths are live.
    if unsafe { FSEventStreamStart(stream.stream.as_ptr()) } == 0 {
        return Err(io::Error::other(
            "Cannot start FSEvents observation; verify filesystem access and the local event service",
        ));
    }
    Ok(Native {
        _stream: stream,
        context,
        queue,
    })
}
impl Native {
    #[cfg(test)]
    pub(super) fn duplicate_queue(&self) -> io::Result<OwnedFd> {
        self.queue.try_clone()
    }
    #[expect(
        unsafe_code,
        reason = "draining the native notification pipe requires nonblocking read"
    )]
    pub(super) fn drain(&self) -> io::Result<Change> {
        let mut bytes = [0; 256];
        let mut observed = false;
        loop {
            // SAFETY: the bounded buffer is writable and Native retains its nonblocking read descriptor.
            let count = unsafe {
                libc::read(
                    self.queue.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                )
            };
            if count > 0 {
                observed = true;
                continue;
            }
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::WouldBlock || !observed {
                return Err(error);
            }
            if self.context.failed.swap(false, Ordering::AcqRel) {
                return Err(io::Error::other("FSEvents notification delivery failed"));
            }
            return Ok(if self.context.structure.swap(false, Ordering::AcqRel) {
                Change::Structure
            } else {
                Change::Content
            });
        }
    }
}
