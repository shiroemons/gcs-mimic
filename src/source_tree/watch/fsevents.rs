use super::*;
use crate::fsutil::FileIdentity;
use crate::sync::tree::EntryError;
use std::{
    collections::HashMap,
    ffi::{CStr, OsStr, c_void},
    os::{
        fd::{FromRawFd, OwnedFd, RawFd},
        unix::ffi::OsStrExt,
    },
    ptr::NonNull,
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
};

// FSEventStreamEventFlags, as named in FSEvents.h.
const EVENT_MUST_SCAN_SUB_DIRS: u32 = 0x0000_0001;
const EVENT_USER_DROPPED: u32 = 0x0000_0002;
const EVENT_KERNEL_DROPPED: u32 = 0x0000_0004;
const EVENT_IDS_WRAPPED: u32 = 0x0000_0008;
const EVENT_HISTORY_DONE: u32 = 0x0000_0010;
const EVENT_ROOT_CHANGED: u32 = 0x0000_0020;
const EVENT_ITEM_INODE_META_MOD: u32 = 0x0000_0400;
const EVENT_ITEM_CHANGE_OWNER: u32 = 0x0000_4000;
const EVENT_ITEM_XATTR_MOD: u32 = 0x0000_8000;
const EVENT_ITEM_IS_FILE: u32 = 0x0001_0000;
const EVENT_ITEM_IS_DIR: u32 = 0x0002_0000;
const EVENT_ITEM_CLONED: u32 = 0x0040_0000;
/// History this stream can no longer replay in order, or a changed root: both need a fresh stream.
const STREAM_RESET: u32 = EVENT_MUST_SCAN_SUB_DIRS
    | EVENT_USER_DROPPED
    | EVENT_KERNEL_DROPPED
    | EVENT_IDS_WRAPPED
    | EVENT_ROOT_CHANGED;
/// Directory metadata changes that can revoke access without another event.
const DIRECTORY_ACCESS: u32 =
    EVENT_ITEM_INODE_META_MOD | EVENT_ITEM_CHANGE_OWNER | EVENT_ITEM_XATTR_MOD;
// FSEventStreamCreateFlags, as named in FSEvents.h.
const CREATE_NO_DEFER: u32 = 0x0000_0002;
const CREATE_WATCH_ROOT: u32 = 0x0000_0004;
const CREATE_FILE_EVENTS: u32 = 0x0000_0010;
/// kFSEventStreamEventIdSinceNow: report only events after the stream starts.
const SINCE_NOW: u64 = u64::MAX;
/// kCFStringEncodingUTF8 from CFString.h.
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

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
// SAFETY: shared references never mutate the stream, and callbacks touch only atomic state, a
// mutex and a pipe.
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
    reported: Mutex<Reported>,
}

/// How many files `Reported` remembers before it starts over. Forgetting one costs at most one
/// more notification when it is cloned again.
const REPORTED_FILES: usize = 1024;

/// The identity each regular file had when the stream last reported an event for it.
///
/// Staging a sync source on APFS clones it, and FSEvents reports that clone as an event of the
/// source although nothing in it changed. The observer then ran another ingestion pass, which
/// staged a file it could not take in, such as an unresolved conflict, again, and so on while
/// nothing changed. The flags cannot tell such an event from a change: FSEvents keeps the cloned
/// flag on later events of the path, and earlier flags on the clone's. The identity can, because
/// a clone leaves it, while a write, a metadata change, a replacement or a removal changes it.
#[derive(Default)]
struct Reported(HashMap<Vec<u8>, FileIdentity>);

impl Reported {
    /// Whether an event with `flags` for the regular file at `path` may report a change since the
    /// stream last reported one. Only a clone of a file whose identity is unchanged reports none.
    fn changed(&mut self, path: &[u8], flags: u32) -> bool {
        // A file that cannot be inspected counts as changed, so its ingestion pass still runs.
        let Ok(metadata) = std::fs::symlink_metadata(OsStr::from_bytes(path)) else {
            self.0.remove(path);
            return true;
        };
        let identity = FileIdentity::from(&metadata);
        if flags & EVENT_ITEM_CLONED != 0 && self.0.get(path) == Some(&identity) {
            return false;
        }
        if self.0.len() >= REPORTED_FILES && !self.0.contains_key(path) {
            self.0.clear();
        }
        self.0.insert(path.to_vec(), identity);
        true
    }

    /// Adds the records of the stream this one replaces, keeping its own newer ones. That stream
    /// reported each recorded identity to the same observer.
    fn inherit(&mut self, earlier: Self) {
        for (path, identity) in earlier.0 {
            if self.0.len() >= REPORTED_FILES {
                break;
            }
            self.0.entry(path).or_insert(identity);
        }
    }
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

fn needs_access_check(flags: u32) -> bool {
    // Ownership may revoke access without a separate inode-metadata notification.
    flags & STREAM_RESET != 0 || flags & EVENT_ITEM_IS_DIR != 0 && flags & DIRECTORY_ACCESS != 0
}

#[test]
fn directory_ownership_changes_require_access_revalidation() {
    assert!(needs_access_check(0x20000 | 0x4000));
    assert!(needs_access_check(0x20000 | 0x400));
    assert!(needs_access_check(0x20000 | 0x8000));
    assert!(needs_access_check(0x1));
    assert!(!needs_access_check(0x10000 | 0x4000));
    assert!(!needs_access_check(0x20000 | 0x1000));
}

/// Each way FSEvents says it lost events, or that the root moved, makes the next pass rebuild
/// the stream and rescan the tree. Treated as a file event instead, a change the stream dropped
/// would go unnoticed until something else changed.
#[test]
fn every_lost_history_or_root_change_rebuilds_the_watch() {
    let root = b"/watched/root";
    for flag in [
        EVENT_MUST_SCAN_SUB_DIRS,
        EVENT_USER_DROPPED,
        EVENT_KERNEL_DROPPED,
        EVENT_IDS_WRAPPED,
        EVENT_ROOT_CHANGED,
    ] {
        assert!(needs_access_check(flag), "{flag:#x}");
        assert!(needs_access_check(flag | EVENT_ITEM_IS_FILE), "{flag:#x}");
    }
    // A dropped or wrapped event names "/" or the root rather than a file below it, and only an
    // entry the root reserves for synchronization is skipped.
    assert!(!ignored(root, b"/"));
    assert!(!ignored(root, root));
    assert!(!ignored(root, b"/watched/root/bucket"));
}

#[test]
fn only_a_clone_of_an_unchanged_file_reports_no_change() {
    use std::os::unix::fs::PermissionsExt;
    let root = crate::test_support::TestDir::new();
    let file = root.0.join("body");
    std::fs::write(&file, b"initial").unwrap();
    let path = file.as_os_str().as_bytes();
    // FSEvents keeps a path's earlier flags on its later events, so the cloned flag marks no
    // clone alone.
    let cloned = EVENT_ITEM_IS_FILE | EVENT_ITEM_CLONED | EVENT_ITEM_INODE_META_MOD;
    let mut reported = Reported::default();
    // The stream may have missed a change before the first event it reports for a file.
    assert!(reported.changed(path, cloned));
    assert!(!reported.changed(path, cloned));
    // Only an event with the cloned flag can report no change.
    assert!(reported.changed(path, EVENT_ITEM_IS_FILE));
    // The same size rewritten, a mode change and a replacement each change the identity.
    std::fs::write(&file, b"changed").unwrap();
    assert!(reported.changed(path, cloned));
    assert!(!reported.changed(path, cloned));
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(reported.changed(path, cloned));
    let replacement = root.0.join("replacement");
    std::fs::write(&replacement, b"changed").unwrap();
    std::fs::rename(&replacement, &file).unwrap();
    assert!(reported.changed(path, cloned));
    assert!(!reported.changed(path, cloned));
    std::fs::remove_file(&file).unwrap();
    assert!(reported.changed(path, cloned));
    std::fs::write(&file, b"restored").unwrap();
    assert!(reported.changed(path, cloned));
    // Past the bound the record starts over, and a clone it forgot is reported once more.
    for index in 1..REPORTED_FILES {
        let other = root.0.join(format!("other-{index}"));
        std::fs::write(&other, b"other").unwrap();
        assert!(reported.changed(other.as_os_str().as_bytes(), cloned));
    }
    assert!(!reported.changed(path, cloned));
    let last = root.0.join("last");
    std::fs::write(&last, b"last").unwrap();
    assert!(reported.changed(last.as_os_str().as_bytes(), cloned));
    assert_eq!(reported.0.len(), 1);
    assert!(reported.changed(path, cloned));
    assert!(!reported.changed(path, cloned));
    // A rebuilt stream keeps what the one it replaces reported, besides its own records.
    let mut rebuilt = Reported::default();
    assert!(rebuilt.changed(last.as_os_str().as_bytes(), EVENT_ITEM_IS_FILE));
    rebuilt.inherit(reported);
    assert!(!rebuilt.changed(path, cloned));
    assert!(!rebuilt.changed(last.as_os_str().as_bytes(), cloned));
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
        if flags & EVENT_HISTORY_DONE != 0 || ignored(&context.root, path) {
            continue;
        }
        // Overflow/root changes need a fresh stream; directory permission changes require a fresh access check.
        if needs_access_check(flags) {
            context.structure.store(true, Ordering::Release);
        } else if flags & EVENT_ITEM_IS_FILE != 0
            && !context
                .reported
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .changed(path, flags)
        {
            continue;
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
pub(super) fn build(root: &File, path: &Path, limits: TreeLimits) -> Result<Native, Error> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: stat has enough writable space, and root remains open throughout setup.
    if unsafe { libc::fstatfs(root.as_raw_fd(), stat.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: fstatfs initialized every field on success.
    if unsafe { stat.assume_init() }.f_flags & libc::MNT_LOCAL as u32 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "sync FSEvents observation requires a local filesystem",
        )
        .into());
    }
    let mut scan = crate::sync::tree::TreeScan::new(limits, crate::fsutil::Lookup::unprobed());
    while !scan.is_empty() {
        let (next, page) = scan.page(root).map_err(Error::Tree)?;
        scan = next;
        match page.into_iter().find_map(|entry| entry.error) {
            None => {}
            Some(EntryError::Io(kind)) => return Err(io::Error::from(kind).into()),
            // Passed over, the directory's subtree would go unwatched, so the build fails and
            // the mirror builds the watch again.
            Some(EntryError::Unopenable) => {
                return Err(Error::Unopenable(crate::fsutil::Unopenable));
            }
        }
    }
    let bytes = path.as_os_str().as_bytes();
    let length = isize::try_from(bytes.len())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    // SAFETY: bytes is a live UTF-8 path; CoreFoundation copies this input into a newly retained string.
    let string = CfObject(allocated(
        unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                bytes.as_ptr(),
                length,
                CF_STRING_ENCODING_UTF8,
                0,
            )
        }
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
        reported: Mutex::default(),
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
            SINCE_NOW,
            0.05,
            CREATE_NO_DEFER | CREATE_WATCH_ROOT | CREATE_FILE_EVENTS,
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
            return Err(error.into());
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
        )
        .into());
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
    /// Takes over the file identities `previous` reported, which a rebuilt watch would otherwise
    /// report again on the next clone of each file.
    pub(super) fn inherit(&self, previous: &Self) {
        let earlier = std::mem::take(
            &mut *previous
                .context
                .reported
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        self.context
            .reported
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .inherit(earlier);
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
