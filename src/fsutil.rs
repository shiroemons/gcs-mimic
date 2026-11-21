#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Descriptor-based filesystem operations; OS-specific unsafe stays in this module.

use std::{collections::VecDeque, ffi::OsString, fs::File, io};
mod hash;
mod lookup;
pub(crate) use hash::{HashOutcome, hash_exact};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(not(unix))]
mod unsupported;
#[cfg(not(unix))]
use unsupported as imp;

use imp::directory_names_within;
pub(crate) use imp::remove_empty_directory_at;
pub(crate) use imp::{
    create_regular_at, directory_at, name_max, open_regular_writable_at, sole_regular_link,
    unlink_file_at,
};
pub use imp::{
    directory_names_after, directory_writable, open_directory_nofollow, open_regular_at,
};
pub(crate) use lookup::{Lookup, Matching};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyMethod {
    Clone,
    #[cfg(target_os = "linux")]
    CopyFileRange,
    ReadWrite,
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

/// Whether a directory helper may create a missing directory or must find an existing one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DirectoryOpen {
    Existing,
    CreateIfMissing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "descriptor-relative directory creation is unsupported on this platform"
    )
)]
pub(crate) enum Created {
    Yes,
    No,
}

#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "descriptor-relative child lookup is unsupported on this platform"
    )
)]
pub(crate) enum Child {
    Directory {
        file: File,
        created: Created,
    },
    Alias,
    Missing(Vacant),
    /// The name kept failing to open while an entry stood under it ([`Opened::Transient`]).
    Transient,
}

#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "descriptor-relative regular-file lookup is unsupported on this platform"
    )
)]
pub(crate) enum RegularChild {
    File(File),
    /// A regular file that the name reached under another letter case or normalization of it,
    /// with the spelling the directory lists it under, or the error the listing failed with. A
    /// caller that only needs to know that the spelling differs is not failed by the listing.
    Respelled(io::Result<ListedSpelling>),
    Missing(Vacant),
    /// The name kept failing to open while an entry stood under it ([`Opened::Transient`]).
    Transient,
}

/// What opening a name found, under the rule that only `fstatat` finding nothing shows that the
/// name holds nothing. A name the mirror reads as empty makes a tracked file a deletion
/// candidate, and a Docker Desktop bind mount fails an open with `ENOENT` for a moment after the
/// host replaced a file while `fstatat` still finds it
/// (`docs/en/evidence/sync-bind-mount-stale-reads.md`), so such an open is tried again rather
/// than taken for an empty name.
#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "descriptor-relative lookup is unsupported on this platform"
    )
)]
pub(crate) enum Opened<T> {
    /// The entry the name holds.
    Entry(T),
    /// Nothing: `fstatat`, not following a symlink, found nothing under the name.
    Missing(Vacant),
    /// The open kept failing with `ENOENT` while `fstatat` still found an entry under the name,
    /// until the retries ran out. Whether the name holds anything is unknown, and a later read
    /// decides. No error comes with it: the answer is made only that way, so the open's error
    /// tells nothing the variant does not, and a caller that must fail uses [`Unsettled`].
    Transient,
}

/// A name that kept failing to open while `fstatat` still found an entry under it
/// ([`Opened::Transient`]), as the error of a step that fails rather than waits for a later read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Unsettled;

impl std::fmt::Display for Unsettled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the entry kept failing to open while its name still held one")
    }
}

impl std::error::Error for Unsettled {}

impl From<Unsettled> for io::Error {
    /// An error of the kind the open kept failing with, so that callers judge it as they judge
    /// a failed open.
    fn from(error: Unsettled) -> Self {
        Self::new(io::ErrorKind::NotFound, error)
    }
}

/// Proof that `fstatat`, not following a symlink, found nothing under a name. Only a lookup
/// makes one, so no other answer of a failed open passes for an empty name.
#[derive(Debug)]
pub(crate) struct Vacant {
    _proven: (),
}

impl Vacant {
    #[cfg_attr(
        not(unix),
        expect(
            dead_code,
            reason = "descriptor-relative lookup is unsupported on this platform"
        )
    )]
    fn proven() -> Self {
        Self { _proven: () }
    }
}

/// The spelling under which a directory lists a regular file that a name reached under another
/// one. Only a listing tells it, by matching each entry's device and inode with the opened
/// file's.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "descriptor-relative lookup is unsupported on this platform"
    )
)]
pub(crate) enum ListedSpelling {
    /// The one name the directory lists the file under.
    Listed(String),
    /// No one name: other names link the file, the listed name is not UTF-8, or the file left the
    /// directory before the listing reached it.
    Unnamed,
}

/// The names of one directory in sorted pages, so a large directory never holds more than one
/// page in memory and a walk can stop between pages. A walker keeps one per open level.
#[derive(Debug, Default)]
pub(crate) struct DirectoryNames {
    after: Option<OsString>,
    buffered: VecDeque<OsString>,
    exhausted: bool,
}

impl DirectoryNames {
    /// Names read per page.
    pub(crate) const PAGE: usize = 256;

    /// The next name of `directory`, reading another page when the buffer is empty; `None` once
    /// none is left. Fails with `InvalidData` once the directory holds more than `max_inspected`
    /// names; each walker maps that to its own limit.
    pub(crate) fn next_name(
        &mut self,
        directory: &File,
        max_inspected: usize,
    ) -> io::Result<Option<OsString>> {
        if self.buffered.is_empty() && !self.exhausted {
            let page = directory_names_within(
                directory,
                self.after.as_deref(),
                Self::PAGE,
                max_inspected,
            )?;
            self.exhausted = page.len() < Self::PAGE;
            self.after = page.last().cloned().or(self.after.take());
            self.buffered = page.into();
        }
        Ok(self.buffered.pop_front())
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

/// An unsupported operation reports the `Unsupported` kind, as `std` does.
impl From<FsError> for io::Error {
    fn from(error: FsError) -> Self {
        match error {
            FsError::Io(error) => error,
            FsError::Unsupported => io::ErrorKind::Unsupported.into(),
        }
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

impl std::error::Error for FsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => std::error::Error::source(error),
            Self::Unsupported => None,
        }
    }
}

// Windows write/change timestamps can repeat within a clock tick, so equality can miss
// same-size rewrites. Directory synchronization, which needs this check, is unsupported there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub modified_ns: i128,
    pub changed_ns: i128,
}

impl FileIdentity {
    /// Whether these timestamps, observed at `observed_ns`, would show any later change: the last
    /// change lies two seconds or more before the observation, which covers the coarsest supported
    /// HFS+/FAT timestamp granularity. Closer to it, a change in the same tick can leave every
    /// timestamp as it was, so the observation is racy.
    pub(crate) fn settled_at(&self, observed_ns: i128) -> bool {
        observed_ns.saturating_sub(self.modified_ns.max(self.changed_ns)) >= 2_000_000_000
    }

    /// The fields that differ in `after`.
    pub(crate) fn changed_fields(&self, after: &Self) -> IdentityChanges {
        IdentityField::ALL
            .into_iter()
            .filter(|field| match field {
                IdentityField::Device => self.device != after.device,
                IdentityField::Inode => self.inode != after.inode,
                IdentityField::Size => self.size != after.size,
                IdentityField::Mtime => self.modified_ns != after.modified_ns,
                IdentityField::Ctime => self.changed_ns != after.changed_ns,
            })
            .collect()
    }
}

/// A `FileIdentity` field that changed while a source was read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum IdentityField {
    Device,
    Inode,
    Size,
    Mtime,
    Ctime,
}

impl IdentityField {
    /// Every field, in declaration order.
    const ALL: [Self; 5] = [
        Self::Device,
        Self::Inode,
        Self::Size,
        Self::Mtime,
        Self::Ctime,
    ];

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Device => "device",
            Self::Inode => "inode",
            Self::Size => "size",
            Self::Mtime => "mtime_ns",
            Self::Ctime => "ctime_ns",
        }
    }

    fn bit(self) -> u8 {
        match self {
            Self::Device => 1,
            Self::Inode => 2,
            Self::Size => 4,
            Self::Mtime => 8,
            Self::Ctime => 16,
        }
    }
}

/// The set of `FileIdentity` fields that differ between two observations of a file.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct IdentityChanges(u8);

impl IdentityChanges {
    pub(crate) fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether the inode number is the only difference. Docker Desktop's shared folders can
    /// report one number for a file just created and another for it later, so a different
    /// number with the same size and times does not by itself show that the bytes changed.
    pub(crate) fn is_inode_only(self) -> bool {
        self.0 == IdentityField::Inode.bit()
    }

    /// The changed fields, in declaration order.
    pub(crate) fn iter(self) -> impl Iterator<Item = IdentityField> {
        IdentityField::ALL
            .into_iter()
            .filter(move |field| self.0 & field.bit() != 0)
    }
}

impl FromIterator<IdentityField> for IdentityChanges {
    fn from_iter<I: IntoIterator<Item = IdentityField>>(fields: I) -> Self {
        Self(fields.into_iter().fold(0, |bits, field| bits | field.bit()))
    }
}

/// Lists the quoted field names, as a `Vec<IdentityField>` would.
impl std::fmt::Debug for IdentityChanges {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

/// Formats as the quoted field name, so `AdminError`'s `Display` and `Debug` keep the text they
/// had when the names were strings: the import journal stores `SourceChanged { fields: ["size"] }`.
impl std::fmt::Debug for IdentityField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

/// The file-system object a path or handle resolved to: equal values name the same inode on the
/// same device, whatever its size or times.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NodeId {
    pub(crate) device: u64,
    pub(crate) inode: u64,
}

impl NodeId {
    /// Big-endian bytes, the order the catalog's `device` and `inode` BLOB columns store.
    pub(crate) fn to_be_bytes(self) -> ([u8; 8], [u8; 8]) {
        (self.device.to_be_bytes(), self.inode.to_be_bytes())
    }

    pub(crate) fn from_be_bytes(device: [u8; 8], inode: [u8; 8]) -> Self {
        Self {
            device: u64::from_be_bytes(device),
            inode: u64::from_be_bytes(inode),
        }
    }
}

impl FileIdentity {
    pub(crate) fn node(&self) -> NodeId {
        NodeId {
            device: self.device,
            inode: self.inode,
        }
    }
}

#[cfg(unix)]
impl From<&std::fs::Metadata> for FileIdentity {
    fn from(metadata: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified_ns: i128::from(metadata.mtime()) * 1_000_000_000
                + i128::from(metadata.mtime_nsec()),
            changed_ns: i128::from(metadata.ctime()) * 1_000_000_000
                + i128::from(metadata.ctime_nsec()),
        }
    }
}

/// How [`durable_replace_between`] gives the staged entry the destination name. Neither judges
/// what the destination held: an exchange keeps it under the staged name, for the caller to
/// compare with what it expected there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Replacement {
    /// The destination name must be empty; an entry there fails the call with `AlreadyExists`.
    NoReplace,
    /// The staged and destination entries exchange names, so what the destination held stays
    /// under the staged name. A missing destination fails the call with `NotFound`.
    Exchange,
}

#[cfg(unix)]
pub(crate) fn sync_dir(path: &std::path::Path) -> io::Result<()> {
    sync_ordered(&File::open(path)?)
}

/// Makes what was written to `file` reach storage before anything written after it.
///
/// Each caller orders a blob or a directory entry before the catalog commit that references
/// it. On Apple platforms `sync_all` is `F_FULLFSYNC`, a device cache flush of several
/// milliseconds that the drive runs one at a time, while SQLite's commit fsync there stops at
/// the device cache. `F_BARRIERFSYNC` keeps the blob ahead of that commit for a fraction of
/// the cost: a power loss can still drop the latest commits, as before, but cannot keep a
/// commit whose blob was lost. A filesystem without barriers gets the full flush instead.
/// Elsewhere fsync already reaches the device.
#[cfg(target_vendor = "apple")]
#[expect(
    unsafe_code,
    reason = "std has no F_BARRIERFSYNC, so it is only reachable through fcntl"
)]
pub(crate) fn sync_ordered(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
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

/// See the Apple version: fsync here already makes the bytes durable.
#[cfg(all(unix, not(target_vendor = "apple")))]
pub(crate) fn sync_ordered(file: &File) -> io::Result<()> {
    file.sync_all()
}

#[derive(Debug)]
pub(crate) enum CloneFailure {
    NotCreated(FsError),
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos")),
        expect(
            dead_code,
            reason = "only supported clone platforms can create a destination before failure"
        )
    )]
    Created(FsError),
}

impl From<io::Error> for CloneFailure {
    fn from(error: io::Error) -> Self {
        Self::NotCreated(error.into())
    }
}

impl std::fmt::Display for CloneFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCreated(error) | Self::Created(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CloneFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotCreated(error) | Self::Created(error) => Some(error),
        }
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
thread_local! {
    static FAIL_CLONE_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Reports that the filesystem cannot clone, as ext4 and Docker Desktop's shared folders do.
    static CLONE_UNSUPPORTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(all(test, unix))]
thread_local! {
    /// The entries this thread asked this module to remove.
    static UNLINKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(all(test, unix))]
fn count_unlink() {
    UNLINKS.set(UNLINKS.get() + 1);
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
        Ok(FileIdentity::from(&file.metadata()?))
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
            u32::try_from(std::mem::size_of::<FILE_BASIC_INFO>()).map_err(io::Error::other)?,
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

/// Whether `open_at` opens an existing regular file, read-only or for writing, or creates a new
/// one.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileOpen {
    Existing,
    ExistingWritable,
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
        | match open {
            FileOpen::Existing => libc::O_RDONLY,
            FileOpen::ExistingWritable => libc::O_RDWR,
            FileOpen::CreateNew => libc::O_RDWR | libc::O_CREAT | libc::O_EXCL,
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

fn regular_source(src: &File) -> io::Result<()> {
    if src.metadata()?.is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular source",
        ))
    }
}

/// Clones `src` into `dst`, an empty regular file open for writing, and syncs it. `Ok(false)`
/// means the filesystem cannot clone; `dst` is then still empty and ready for a copy.
#[cfg(target_os = "linux")]
#[expect(unsafe_code, reason = "FICLONE has no safe std equivalent")]
fn clone_onto(src: &File, dst: &File) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    #[cfg(test)]
    if FAIL_CLONE_SYNC.replace(false) {
        return Err(io::ErrorKind::Other.into());
    }
    #[cfg(test)]
    if CLONE_UNSUPPORTED.get() {
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

/// Creates a clone without copying; Unsupported leaves no new entry.
/// Mutable-source staging can then fall back to its own single-pass copy and hash.
#[cfg_attr(
    any(target_os = "linux", target_os = "macos"),
    expect(
        unsafe_code,
        reason = "fclonefileat and unlinkat have no safe std equivalent"
    )
)]
pub(crate) fn clone_into(
    src: &File,
    directory: &File,
    name: &std::ffi::OsStr,
) -> Result<(), CloneFailure> {
    validate_name(name)?;
    regular_source(src)?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::fd::AsRawFd;
        let name = component(name)?;
        #[cfg(target_os = "macos")]
        {
            #[cfg(test)]
            if CLONE_UNSUPPORTED.get() {
                return Err(CloneFailure::NotCreated(FsError::Unsupported));
            }
            // SAFETY: live descriptors and a validated destination component; clone creates a new entry.
            if unsafe {
                libc::fclonefileat(src.as_raw_fd(), directory.as_raw_fd(), name.as_ptr(), 0)
            } == 0
            {
                #[cfg(test)]
                if FAIL_CLONE_SYNC.replace(false) {
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
        #[cfg(target_os = "linux")]
        {
            let dst = open_at(directory, &name, FileOpen::CreateNew)?;
            if clone_onto(src, &dst).map_err(|error| CloneFailure::Created(error.into()))? {
                return Ok(());
            }
            #[cfg(test)]
            count_unlink();
            // SAFETY: only this call owns the newly created target; the component and directory are live.
            if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(CloneFailure::Created(io::Error::last_os_error().into()));
            }
            Err(CloneFailure::NotCreated(FsError::Unsupported))
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = directory;
        Err(CloneFailure::NotCreated(FsError::Unsupported))
    }
}

/// Copies an immutable open blob into the new entry `name`. Mutable-source staging must hash its
/// own read/write pass. The source offset is unchanged; a failed clone removes its new entry, and
/// a failed copy leaves it for caller cleanup.
pub(crate) fn clone_or_copy_into(
    src: &File,
    directory: &File,
    name: &std::ffi::OsStr,
) -> Result<CopyMethod, FsError> {
    #[cfg(not(target_os = "linux"))]
    match clone_into(src, directory, name) {
        Ok(()) => return Ok(CopyMethod::Clone),
        Err(CloneFailure::NotCreated(FsError::Unsupported)) => (),
        Err(CloneFailure::NotCreated(error)) => return Err(error),
        Err(CloneFailure::Created(error)) => return Err(discard_clone(directory, name, error)),
    }
    #[cfg(unix)]
    {
        #[cfg(target_os = "linux")]
        regular_source(src)?;
        let dst = open_at(directory, &component(name)?, FileOpen::CreateNew)?;
        // Linux has to create the entry before it can ask for a clone, so a filesystem that
        // cannot clone gets the copy in that same file. Removing it and creating the name again
        // would change the directory twice more, and Docker Desktop's shared folders can then
        // report one inode number for the new file and another later, which write-back's check
        // of its staged copy reads as an external change.
        #[cfg(target_os = "linux")]
        match clone_onto(src, &dst) {
            Ok(true) => return Ok(CopyMethod::Clone),
            Ok(false) => {}
            Err(error) => return Err(discard_clone(directory, name, error.into())),
        }
        copy_into_empty(src, &dst)
    }
    #[cfg(windows)]
    {
        let path = directory_path(directory)?.join(name);
        let dst = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        copy_into_empty(src, &dst)
    }
}

/// Fills `dst`, an empty regular file open for writing, with the immutable open blob `src` and
/// syncs it, as `clone_or_copy_into` fills a new entry. Write-back refills its own private copy
/// this way after a failed attempt instead of removing it and creating the name again.
pub(crate) fn clone_or_copy_onto(src: &File, dst: &File) -> Result<CopyMethod, FsError> {
    regular_source(src)?;
    if dst.metadata()?.len() != 0 {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "expected an empty destination").into(),
        );
    }
    #[cfg(target_os = "linux")]
    if clone_onto(src, dst)? {
        return Ok(CopyMethod::Clone);
    }
    copy_into_empty(src, dst)
}

/// Removes the entry a failed clone created and returns the clone's error; a failed removal is
/// logged, since the clone's error is the one the caller acts on.
fn discard_clone(directory: &File, name: &std::ffi::OsStr, error: FsError) -> FsError {
    if let Err(cleanup) = unlink_file_at(directory, name) {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Error,
            "clone_cleanup_failed",
            serde_json::json!({"message": cleanup.to_string()}),
        );
    }
    error
}

/// Copies `src` into `dst`, an empty regular file open for writing, and syncs it. Both offsets
/// are explicit, so neither file's position matters or changes.
#[cfg_attr(
    target_os = "linux",
    expect(unsafe_code, reason = "copy_file_range has no safe std equivalent")
)]
fn copy_into_empty(src: &File, dst: &File) -> Result<CopyMethod, FsError> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let mut offset: libc::loff_t = 0;
        let mut written: libc::loff_t = 0;
        loop {
            // SAFETY: descriptors remain live, both offsets are writable, and no userspace buffer is passed.
            let copied = unsafe {
                libc::copy_file_range(
                    src.as_raw_fd(),
                    &mut offset,
                    dst.as_raw_fd(),
                    &mut written,
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
    copy_read_write(src, dst)?;
    dst.sync_all()?;
    Ok(CopyMethod::ReadWrite)
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

/// Whether [`read_cached_at`] can ever return bytes on this platform.
pub(crate) const READS_CACHED_PAGES: bool = cfg!(all(
    target_os = "linux",
    any(target_env = "gnu", target_env = "musl")
));

/// Reads what the page cache already holds at `offset`, and never waits for storage: the kernel
/// fails `RWF_NOWAIT` rather than block, which makes this safe to call on an async worker. `None`
/// covers every other outcome, including a file system without support and the end of the file,
/// and sends the caller to [`read_at`], which also reports any real failure.
#[cfg(all(target_os = "linux", any(target_env = "gnu", target_env = "musl")))]
#[expect(unsafe_code, reason = "std has no wrapper for preadv2")]
pub(crate) fn read_cached_at(file: &File, bytes: &mut [u8], offset: u64) -> Option<usize> {
    use std::os::fd::AsRawFd;

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

#[cfg(not(all(target_os = "linux", any(target_env = "gnu", target_env = "musl"))))]
pub(crate) fn read_cached_at(_file: &File, _bytes: &mut [u8], _offset: u64) -> Option<usize> {
    None
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
            let remaining = bytes
                .get(written..count)
                .ok_or(io::ErrorKind::InvalidData)?;
            #[cfg(unix)]
            let result = dst.write_at(remaining, offset + written as u64);
            #[cfg(windows)]
            let result = dst.seek_write(remaining, offset + written as u64);
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

/// [`durable_replace_between`] within one directory.
#[cfg(all(test, unix))]
pub(crate) fn durable_replace(
    directory: &File,
    staged_name: &std::ffi::OsStr,
    destination_name: &std::ffi::OsStr,
    replacement: Replacement,
) -> Result<(), FsError> {
    durable_replace_between(
        directory,
        staged_name,
        directory,
        destination_name,
        replacement,
    )
}

/// The directories, by device and inode number, whose sync inside a replacement fails. Tests run
/// in parallel, so each adds its own directory and removes only that one again.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) static FAIL_REPLACE_SYNC: std::sync::Mutex<Vec<(u64, u64)>> =
    std::sync::Mutex::new(Vec::new());

/// The directories, by device and inode number, for which tests take the platform's answer to be
/// that an opened entry is spelled the way it was opened, as a clear Linux casefold flag says. On
/// a test filesystem that folds letter case, such a directory stands in for a Linux mount that
/// folds case without the casefold flag; a lookup below a root the probe found to fold names
/// does not believe that answer. Tests run in parallel, so each adds its own directory and
/// removes only that one again.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) static UNCHECKED_SPELLING: std::sync::Mutex<Vec<(u64, u64)>> =
    std::sync::Mutex::new(Vec::new());

/// Takes the entries of a directory as spelled the way they were opened while it lives
/// ([`UNCHECKED_SPELLING`]).
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) struct UncheckedSpelling((u64, u64));

// A separate `cfg(test)` lets Clippy see test code here and allow its unwraps.
#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl UncheckedSpelling {
    pub(crate) fn of(directory: &std::path::Path) -> Self {
        let identity = stat_identity(&File::open(directory).unwrap()).unwrap();
        let unchecked = (identity.device, identity.inode);
        UNCHECKED_SPELLING.lock().unwrap().push(unchecked);
        Self(unchecked)
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
impl Drop for UncheckedSpelling {
    fn drop(&mut self) {
        if let Ok(mut unchecked) = UNCHECKED_SPELLING.lock() {
            unchecked.retain(|directory| *directory != self.0);
        }
    }
}

/// The directories, by device and inode number, whose listing fails with `EIO`, as a listing of
/// a network mount can while opening a name there still works. Tests run in parallel, so each
/// adds its own directory and removes only that one again.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
static UNREADABLE_LISTINGS: std::sync::Mutex<Vec<(u64, u64)>> = std::sync::Mutex::new(Vec::new());

/// Fails each listing of a directory while it lives ([`UNREADABLE_LISTINGS`]).
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) struct UnreadableListing((u64, u64));

// A separate `cfg(test)` lets Clippy see test code here and allow its unwraps.
#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl UnreadableListing {
    pub(crate) fn of(directory: &std::path::Path) -> Self {
        let identity = stat_identity(&File::open(directory).unwrap()).unwrap();
        let unreadable = (identity.device, identity.inode);
        UNREADABLE_LISTINGS.lock().unwrap().push(unreadable);
        Self(unreadable)
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
impl Drop for UnreadableListing {
    fn drop(&mut self) {
        if let Ok(mut unreadable) = UNREADABLE_LISTINGS.lock() {
            unreadable.retain(|directory| *directory != self.0);
        }
    }
}

/// Fails with `EIO` when a listing of `directory` is due to fail ([`UNREADABLE_LISTINGS`]).
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn unreadable_listing(directory: &File) -> io::Result<()> {
    let unreadable = UNREADABLE_LISTINGS
        .lock()
        .map_err(|_| io::Error::other("listing failure injection poisoned"))?;
    if unreadable.is_empty() {
        return Ok(());
    }
    let identity = stat_identity(directory)?;
    if unreadable.contains(&(identity.device, identity.inode)) {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    Ok(())
}

/// Names whose opens fail while the entry stays: by the device and inode number of the
/// directory, the name, how many opens succeed first, how many fail after them, and how. Opening
/// an existing regular file or directory by name fails that way; `fstatat` still finds the
/// entry. Tests run in parallel, so each adds its own name and removes only that one again.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
static FAILING_OPENS: std::sync::Mutex<Vec<FailingOpen>> = std::sync::Mutex::new(Vec::new());

/// A name in [`FAILING_OPENS`].
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
struct FailingOpen {
    directory: (u64, u64),
    name: OsString,
    skip: usize,
    left: usize,
    failure: OpenFailure,
}

/// How an open in [`FAILING_OPENS`] fails.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
#[derive(Clone, Copy)]
enum OpenFailure {
    /// With `ENOENT`, as a Docker Desktop bind mount answers for a file it is still bringing
    /// over from the host ([`Opened::Transient`]).
    Vanishing,
    /// With this `errno`, as a file system answers that refuses the name.
    Refused(i32),
}

/// Fails opens of a name while it lives ([`FAILING_OPENS`]).
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) struct FailingOpens {
    directory: (u64, u64),
    name: OsString,
}

// A separate `cfg(test)` lets Clippy see test code here and allow its unwraps.
#[cfg(test)]
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl FailingOpens {
    /// Fails the next `opens` opens of `name` in `directory` with `ENOENT`.
    pub(crate) fn vanishing(directory: &std::path::Path, name: &str, opens: usize) -> Self {
        Self::vanishing_after(directory, name, 0, opens)
    }

    /// Lets the next `skip` opens of `name` in `directory` succeed, and fails the `opens` after
    /// them with `ENOENT`.
    pub(crate) fn vanishing_after(
        directory: &std::path::Path,
        name: &str,
        skip: usize,
        opens: usize,
    ) -> Self {
        Self::add(directory, name, skip, opens, OpenFailure::Vanishing)
    }

    /// Fails every open of `name` in `directory` with `errno`.
    pub(crate) fn refusing(directory: &std::path::Path, name: &str, errno: i32) -> Self {
        Self::add(directory, name, 0, usize::MAX, OpenFailure::Refused(errno))
    }

    fn add(
        directory: &std::path::Path,
        name: &str,
        skip: usize,
        left: usize,
        failure: OpenFailure,
    ) -> Self {
        let identity = stat_identity(&File::open(directory).unwrap()).unwrap();
        let directory = (identity.device, identity.inode);
        FAILING_OPENS.lock().unwrap().push(FailingOpen {
            directory,
            name: name.into(),
            skip,
            left,
            failure,
        });
        Self {
            directory,
            name: name.into(),
        }
    }

    /// How many of the failures are left.
    pub(crate) fn left(&self) -> usize {
        FAILING_OPENS
            .lock()
            .unwrap()
            .iter()
            .find(|open| self.names(open))
            .map_or(0, |open| open.left)
    }

    fn names(&self, open: &FailingOpen) -> bool {
        open.directory == self.directory && open.name == self.name
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
impl Drop for FailingOpens {
    fn drop(&mut self) {
        if let Ok(mut failing) = FAILING_OPENS.lock() {
            failing.retain(|open| open.directory != self.directory || open.name != self.name);
        }
    }
}

/// Fails when an open of `name` in `directory` is due to fail ([`FAILING_OPENS`]), using that
/// failure up.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
fn failing_open(directory: &File, name: &std::ffi::OsStr) -> io::Result<()> {
    let mut failing = FAILING_OPENS
        .lock()
        .map_err(|_| io::Error::other("open failure injection poisoned"))?;
    if failing.is_empty() {
        return Ok(());
    }
    let identity = stat_identity(directory)?;
    let Some(open) = failing.iter_mut().find(|open| {
        open.directory == (identity.device, identity.inode) && open.name == name && open.left > 0
    }) else {
        return Ok(());
    };
    if open.skip > 0 {
        open.skip -= 1;
        return Ok(());
    }
    open.left -= 1;
    Err(match open.failure {
        OpenFailure::Vanishing => io::ErrorKind::NotFound.into(),
        OpenFailure::Refused(errno) => io::Error::from_raw_os_error(errno),
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sync_replace_parent(directory: &File) -> io::Result<()> {
    #[cfg(test)]
    {
        let identity = stat_identity(directory)?;
        if FAIL_REPLACE_SYNC
            .lock()
            .map_err(|_| io::Error::other("sync injection poisoned"))?
            .contains(&(identity.device, identity.inode))
        {
            return Err(io::Error::other("injected replacement parent sync failure"));
        }
    }
    directory.sync_all()?;
    // The fault model learns of the sync only once it ran, so removing the sync removes this too.
    #[cfg(test)]
    crate::storage::fault::dir_synced(&crate::storage::fault::descriptor_path(directory)?);
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Copy)]
enum RenameMode {
    NoReplace,
    Exchange,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[expect(
    unsafe_code,
    reason = "exclusive and exchange renames have no safe std equivalent"
)]
fn rename_at(
    source_directory: libc::c_int,
    source: &std::ffi::CStr,
    destination_directory: libc::c_int,
    destination: &std::ffi::CStr,
    mode: RenameMode,
) -> Result<(), FsError> {
    // SAFETY: the C strings remain live and the descriptors belong to the caller or AT_FDCWD.
    let result = unsafe {
        #[cfg(target_os = "linux")]
        {
            // musl 1.2.5 has no renameat2 wrapper.
            libc::syscall(
                libc::SYS_renameat2,
                source_directory,
                source.as_ptr(),
                destination_directory,
                destination.as_ptr(),
                match mode {
                    RenameMode::NoReplace => libc::RENAME_NOREPLACE,
                    RenameMode::Exchange => libc::RENAME_EXCHANGE,
                },
            )
        }
        #[cfg(target_os = "macos")]
        {
            libc::renameatx_np(
                source_directory,
                source.as_ptr(),
                destination_directory,
                destination.as_ptr(),
                match mode {
                    RenameMode::NoReplace => libc::RENAME_EXCL,
                    RenameMode::Exchange => libc::RENAME_SWAP,
                },
            )
        }
    };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(libc::ENOSYS | libc::ENOTSUP | libc::EINVAL)
    ) {
        return Err(FsError::Unsupported);
    }
    Err(error.into())
}

/// Refuses to replace an existing entry. A fallback crash may leave both names, which
/// startup's staging recovery handles before admitting requests.
#[cfg(unix)]
pub(crate) fn rename_noreplace(from: &std::path::Path, to: &std::path::Path) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let source = std::ffi::CString::new(from.as_os_str().as_bytes())?;
        let destination = std::ffi::CString::new(to.as_os_str().as_bytes())?;
        match rename_at(
            libc::AT_FDCWD,
            &source,
            libc::AT_FDCWD,
            &destination,
            RenameMode::NoReplace,
        ) {
            Ok(()) => return Ok(()),
            Err(FsError::Unsupported) => {}
            Err(FsError::Io(error)) => return Err(error),
        }
    }
    link_then_unlink(from, to)
}

/// Swaps the entries at `left` and `right` in one atomic exchange, as the fault model undoes a
/// volatile exchange in a snapshot.
#[cfg(all(test, unix))]
pub(crate) fn exchange_paths(left: &std::path::Path, right: &std::path::Path) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::ffi::OsStrExt;
        let left = std::ffi::CString::new(left.as_os_str().as_bytes())?;
        let right = std::ffi::CString::new(right.as_os_str().as_bytes())?;
        rename_at(
            libc::AT_FDCWD,
            &left,
            libc::AT_FDCWD,
            &right,
            RenameMode::Exchange,
        )
        .map_err(io::Error::from)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (left, right);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

#[cfg(unix)]
fn link_then_unlink(from: &std::path::Path, to: &std::path::Path) -> io::Result<()> {
    std::fs::hard_link(from, to)?;
    std::fs::remove_file(from)
}

/// Gives the staged entry the destination name as `replacement` says, and makes both names
/// durable. Only the namespace changes: an exchange retains what the destination held under the
/// staged name, whatever it is, and the caller decides what that is. An error after the rename
/// may have applied it; callers must reconcile both names.
pub(crate) fn durable_replace_between(
    source_directory: &File,
    staged_name: &std::ffi::OsStr,
    destination_directory: &File,
    destination_name: &std::ffi::OsStr,
    replacement: Replacement,
) -> Result<(), FsError> {
    validate_name(staged_name)?;
    validate_name(destination_name)?;
    let source_identity = stat_identity(source_directory)?;
    let destination_identity = stat_identity(destination_directory)?;
    let same_directory = source_identity.node() == destination_identity.node();
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
        let source = open_at(source_directory, &staged, FileOpen::Existing)?;
        source.sync_all()?;
        #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
        crate::storage::fault::file_synced_at(
            &crate::storage::fault::descriptor_path(&source)?,
            &source.metadata()?,
        );
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            rename_at(
                source_directory.as_raw_fd(),
                &staged,
                destination_directory.as_raw_fd(),
                &destination,
                match replacement {
                    Replacement::NoReplace => RenameMode::NoReplace,
                    Replacement::Exchange => RenameMode::Exchange,
                },
            )?;
            // Recorded before the parent syncs, so the power-loss model keeps the rename volatile
            // until they run and a missing or failed one shows.
            #[cfg(test)]
            {
                use crate::storage::fault;
                let staged = fault::descriptor_path(source_directory)?.join(staged_name);
                let destination =
                    fault::descriptor_path(destination_directory)?.join(destination_name);
                match replacement {
                    Replacement::NoReplace => fault::moved(&staged, &destination),
                    Replacement::Exchange => fault::exchanged(&staged, &destination),
                }
            }
            sync_replace_parent(source_directory)?;
            if !same_directory {
                sync_replace_parent(destination_directory)?;
            }
            Ok(())
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (destination, replacement);
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
        match replacement {
            Replacement::NoReplace => {
                move_write_through(&source, &target)?;
                Ok(())
            }
            // Windows has no rename that keeps the replaced entry under the staged name.
            Replacement::Exchange => Err(FsError::Unsupported),
        }
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
            u32::try_from(buffer.len()).map_err(io::Error::other)?,
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
    Ok(std::ffi::OsString::from_wide(
        buffer
            .get(..count as usize)
            .ok_or(io::ErrorKind::InvalidData)?,
    )
    .into())
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

pub(crate) const SOURCE_METADATA_CAP: usize = crate::bounded_body::RESOURCE_BODY_LIMIT;

/// The field stays private, so a caller cannot supply a smaller allocation. A zeroed allocation
/// this large is mapped lazily and only pages an attribute reaches take memory; building it as an
/// array first would put it on the stack.
pub(crate) struct SourceMetadataBuffer(Box<[u8]>);

impl Default for SourceMetadataBuffer {
    fn default() -> Self {
        Self(vec![0; SOURCE_METADATA_CAP + 1].into_boxed_slice())
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
            checked_length(
                usize::try_from(result)
                    .map_err(|error| SourceMetadataError::Io(io::Error::other(error)))?,
            )?
        };
        length
            .map(|length| buffer.0.get(..length).ok_or(SourceMetadataError::TooLarge))
            .transpose()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (file, &mut buffer.0);
        Err(SourceMetadataError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The import journal stores an `AdminError` text that lists changed fields by their former
    /// string names, so each field prints as that quoted name, in a fixed order.
    #[test]
    fn changed_identity_fields_print_as_their_quoted_names_in_order() {
        let before = FileIdentity {
            device: 1,
            inode: 2,
            size: 3,
            modified_ns: 4,
            changed_ns: 5,
        };
        assert!(before.changed_fields(&before).is_empty());
        let after = FileIdentity {
            device: 10,
            inode: 20,
            size: 30,
            modified_ns: 40,
            changed_ns: 50,
        };
        assert_eq!(
            format!("{:?}", before.changed_fields(&after)),
            r#"["device", "inode", "size", "mtime_ns", "ctime_ns"]"#
        );
        assert_eq!(
            before
                .changed_fields(&FileIdentity { size: 9, ..before })
                .iter()
                .collect::<Vec<_>>(),
            [IdentityField::Size]
        );
    }

    /// Only a lone inode change counts as a renumbering; any other changed field, alone or
    /// beside the inode, does not.
    #[test]
    fn only_a_lone_inode_change_is_inode_only() {
        let before = FileIdentity {
            device: 1,
            inode: 2,
            size: 3,
            modified_ns: 4,
            changed_ns: 5,
        };
        assert!(!before.changed_fields(&before).is_inode_only());
        assert!(
            before
                .changed_fields(&FileIdentity { inode: 7, ..before })
                .is_inode_only()
        );
        for after in [
            FileIdentity {
                device: 9,
                ..before
            },
            FileIdentity { size: 9, ..before },
            FileIdentity {
                modified_ns: 9,
                ..before
            },
            FileIdentity {
                changed_ns: 9,
                ..before
            },
            FileIdentity {
                inode: 7,
                changed_ns: 9,
                ..before
            },
        ] {
            let changes = before.changed_fields(&after);
            assert!(
                !changes.is_empty() && !changes.is_inode_only(),
                "{changes:?}"
            );
        }
    }

    /// Callers that report `io::Error` see the native failure itself, and an unsupported
    /// operation as the `Unsupported` kind.
    #[test]
    fn filesystem_errors_keep_their_kind_as_io_errors() {
        let native = io::Error::from(FsError::Io(io::Error::from_raw_os_error(libc::EMFILE)));
        assert_eq!(native.raw_os_error(), Some(libc::EMFILE));
        let denied = io::Error::from(FsError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "denied fixture",
        )));
        assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(denied.to_string(), "denied fixture");
        let unsupported = io::Error::from(FsError::Unsupported);
        assert_eq!(unsupported.kind(), io::ErrorKind::Unsupported);
        assert!(unsupported.get_ref().is_none());
    }

    /// The catalog stores a node as two big-endian BLOB columns; the full `u64` range survives.
    #[test]
    fn node_ids_round_trip_through_their_stored_bytes() {
        for node in [
            NodeId {
                device: 0,
                inode: 0,
            },
            NodeId {
                device: u64::MAX,
                inode: 1,
            },
            NodeId {
                device: 1,
                inode: u64::MAX,
            },
        ] {
            let (device, inode) = node.to_be_bytes();
            assert_eq!(device, node.device.to_be_bytes());
            assert_eq!(inode, node.inode.to_be_bytes());
            assert_eq!(NodeId::from_be_bytes(device, inode), node);
        }
    }

    /// A paged listing returns every name of a directory that fills one or more pages exactly
    /// once and in order, and refuses a directory past its inspection limit.
    #[cfg(unix)]
    #[test]
    fn directory_names_page_through_a_directory_once_in_order() {
        for count in [DirectoryNames::PAGE, DirectoryNames::PAGE * 2 + 3] {
            let directory = crate::test_support::TestDir::new();
            let mut expected: Vec<OsString> = (0..count)
                .rev()
                .map(|index| OsString::from(format!("{index:04}")))
                .collect();
            for name in &expected {
                File::create(directory.0.join(name)).unwrap();
            }
            expected.sort();
            let root = open_directory(&directory.0).unwrap();
            let mut names = DirectoryNames::default();
            let mut listed = Vec::new();
            while let Some(name) = names.next_name(&root, count).unwrap() {
                listed.push(name);
            }
            assert_eq!(listed, expected);
            assert_eq!(names.next_name(&root, count).unwrap(), None);
            assert_eq!(
                DirectoryNames::default()
                    .next_name(&root, count - 1)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn ordered_syncs_accept_written_files_and_directories() {
        use std::io::Write as _;
        let directory = crate::test_support::TestDir::new();
        let path = directory.0.join("blob");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"bytes").unwrap();
        sync_ordered(&file).unwrap();
        sync_dir(&directory.0).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"bytes");
    }

    #[cfg(unix)]
    #[test]
    fn exclusive_rename_and_link_fallback_preserve_existing_destinations() {
        for rename in [rename_noreplace, link_then_unlink] {
            let directory = crate::test_support::TestDir::new();
            let source = directory.0.join("source");
            let destination = directory.0.join("destination");
            std::fs::write(&source, b"first").unwrap();
            rename(&source, &destination).unwrap();
            assert!(!source.exists());
            assert_eq!(std::fs::read(&destination).unwrap(), b"first");
            std::fs::write(&source, b"second").unwrap();
            assert_eq!(
                rename(&source, &destination).unwrap_err().kind(),
                io::ErrorKind::AlreadyExists
            );
            assert_eq!(std::fs::read(&source).unwrap(), b"second");
            assert_eq!(std::fs::read(&destination).unwrap(), b"first");
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn clone_failure_preserves_created_ownership_and_copy_wrapper_cleans_it() {
        let directory = crate::test_support::TestDir::new();
        std::fs::write(directory.0.join("source"), b"source").unwrap();
        let source = File::open(directory.0.join("source")).unwrap();
        let root = File::open(&directory.0).unwrap();
        FAIL_CLONE_SYNC.set(true);
        assert!(matches!(
            clone_into(&source, &root, "clone".as_ref()),
            Err(CloneFailure::Created(_))
        ));
        assert!(directory.0.join("clone").exists());
        unlink_file_at(&root, "clone".as_ref()).unwrap();
        FAIL_CLONE_SYNC.set(true);
        assert!(clone_or_copy_into(&source, &root, "copy".as_ref()).is_err());
        assert!(!directory.0.join("copy").exists());
        std::fs::write(directory.0.join("existing"), b"other owner").unwrap();
        assert!(matches!(
            clone_into(&source, &root, "existing".as_ref()),
            Err(CloneFailure::NotCreated(_))
        ));
        assert_eq!(
            std::fs::read(directory.0.join("existing")).unwrap(),
            b"other owner"
        );
    }

    /// A filesystem that cannot clone, such as ext4 or Docker Desktop's shared folders, gets the
    /// copy in the entry already created for the clone, so nothing is removed. `clone_into`
    /// still leaves no entry behind, as its callers expect.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn copying_after_an_unsupported_clone_keeps_the_new_entry() {
        let directory = crate::test_support::TestDir::new();
        std::fs::write(directory.0.join("source"), b"source").unwrap();
        let source = File::open(directory.0.join("source")).unwrap();
        let root = File::open(&directory.0).unwrap();
        CLONE_UNSUPPORTED.set(true);
        let unlinks = UNLINKS.get();
        let copied = clone_or_copy_into(&source, &root, "copy".as_ref());
        let unlinked = UNLINKS.get() - unlinks;
        let refused = clone_into(&source, &root, "clone".as_ref());
        CLONE_UNSUPPORTED.set(false);
        assert_ne!(copied.unwrap(), CopyMethod::Clone);
        assert_eq!(unlinked, 0);
        assert_eq!(std::fs::read(directory.0.join("copy")).unwrap(), b"source");
        assert!(matches!(
            refused,
            Err(CloneFailure::NotCreated(FsError::Unsupported))
        ));
        assert!(!directory.0.join("clone").exists());
    }

    /// Removing an empty directory never removes what a directory holds and never follows a
    /// symlink: a directory with an entry, and a symlink to a directory or to nothing, all stay as
    /// they were, with what they hold or point to.
    #[cfg(unix)]
    #[test]
    fn removing_an_empty_directory_keeps_entries_and_symlinks() {
        let tree = crate::test_support::TestDir::new();
        let root = File::open(&tree.0).unwrap();
        std::fs::create_dir(tree.0.join("empty")).unwrap();
        std::fs::create_dir_all(tree.0.join("full/inner")).unwrap();
        std::fs::create_dir(tree.0.join("pointed")).unwrap();
        std::os::unix::fs::symlink(tree.0.join("pointed"), tree.0.join("link")).unwrap();
        std::os::unix::fs::symlink(tree.0.join("gone"), tree.0.join("dangling")).unwrap();
        remove_empty_directory_at(&root, "empty".as_ref()).unwrap();
        assert!(!tree.0.join("empty").exists());
        let full = remove_empty_directory_at(&root, "full".as_ref()).unwrap_err();
        assert!(
            matches!(
                full.kind(),
                io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::AlreadyExists
            ),
            "{full:?}"
        );
        assert!(tree.0.join("full/inner").is_dir());
        for link in ["link", "dangling"] {
            let refused = remove_empty_directory_at(&root, link.as_ref()).unwrap_err();
            assert_eq!(refused.kind(), io::ErrorKind::NotADirectory, "{link}");
            assert!(
                std::fs::symlink_metadata(tree.0.join(link))
                    .unwrap()
                    .is_symlink()
            );
        }
        assert!(tree.0.join("pointed").is_dir());
    }

    /// Refilling an emptied file keeps its inode and position-independent bytes, whether or not
    /// the filesystem clones; a file that is not empty is refused before anything is written.
    #[cfg(unix)]
    #[test]
    fn refilling_an_emptied_file_keeps_its_inode() {
        use std::io::Write as _;
        let directory = crate::test_support::TestDir::new();
        let bytes: Vec<u8> = (0..150_003).map(|i| (i % 251) as u8).collect();
        std::fs::write(directory.0.join("source"), &bytes).unwrap();
        let source = File::open(directory.0.join("source")).unwrap();
        let mut copy = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(directory.0.join("copy"))
            .unwrap();
        copy.write_all(b"partial").unwrap();
        let inode = stat_identity(&copy).unwrap().inode;
        assert!(
            matches!(clone_or_copy_onto(&source, &copy), Err(FsError::Io(error)) if error.kind() == io::ErrorKind::InvalidInput)
        );
        assert_eq!(std::fs::read(directory.0.join("copy")).unwrap(), b"partial");
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let unsupported = [false, true];
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let unsupported = [false];
        for unsupported in unsupported {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            CLONE_UNSUPPORTED.set(unsupported);
            copy.set_len(0).unwrap();
            let refilled = clone_or_copy_onto(&source, &copy);
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            CLONE_UNSUPPORTED.set(false);
            let method = refilled.unwrap();
            assert!(!unsupported || method != CopyMethod::Clone, "{method:?}");
            assert_eq!(std::fs::read(directory.0.join("copy")).unwrap(), bytes);
            assert_eq!(stat_identity(&copy).unwrap().inode, inode);
        }
    }

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
        let directory = crate::test_support::TestDir::new();
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

    /// A replacement never judges what the destination held: an exchange keeps any entry there,
    /// changed bytes or a symlink alike, under the staged name for its caller to compare.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn exchange_keeps_whatever_the_destination_held() {
        let directory = crate::test_support::TestDir::new();
        let root = File::open(&directory.0).unwrap();
        let replace =
            |replacement| durable_replace(&root, "stage".as_ref(), "target".as_ref(), replacement);
        std::fs::write(directory.0.join("stage"), b"first").unwrap();
        replace(Replacement::NoReplace).unwrap();
        std::fs::write(directory.0.join("stage"), b"second").unwrap();
        assert!(
            matches!(replace(Replacement::NoReplace), Err(FsError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        assert_eq!(std::fs::read(directory.0.join("target")).unwrap(), b"first");
        replace(Replacement::Exchange).unwrap();
        assert_eq!(std::fs::read(directory.0.join("stage")).unwrap(), b"first");
        assert_eq!(
            std::fs::read(directory.0.join("target")).unwrap(),
            b"second"
        );
        std::fs::write(directory.0.join("stage"), b"third").unwrap();
        replace(Replacement::Exchange).unwrap();
        assert_eq!(std::fs::read(directory.0.join("stage")).unwrap(), b"second");
        assert_eq!(std::fs::read(directory.0.join("target")).unwrap(), b"third");
        std::fs::remove_file(directory.0.join("target")).unwrap();
        assert!(
            matches!(replace(Replacement::Exchange), Err(FsError::Io(error)) if error.kind() == io::ErrorKind::NotFound)
        );
        assert_eq!(std::fs::read(directory.0.join("stage")).unwrap(), b"second");
        std::os::unix::fs::symlink("missing", directory.0.join("target")).unwrap();
        replace(Replacement::Exchange).unwrap();
        assert!(
            std::fs::symlink_metadata(directory.0.join("stage"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(directory.0.join("target")).unwrap(),
            b"second"
        );
    }

    #[cfg(unix)]
    #[test]
    fn export_directory_walk_rejects_redirects_and_unsafe_components() {
        let directory = crate::test_support::TestDir::new();
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
        let directory = crate::test_support::TestDir::new();
        let root = open_directory_nofollow(&directory.0).unwrap();
        let workspace =
            directory_path_at(&root, "work".as_ref(), DirectoryOpen::CreateIfMissing).unwrap();
        let target = directory_path_at(
            &root,
            "bucket/nested".as_ref(),
            DirectoryOpen::CreateIfMissing,
        )
        .unwrap();
        let replace = |replacement| {
            durable_replace_between(
                &workspace,
                "body".as_ref(),
                &target,
                "body".as_ref(),
                replacement,
            )
        };
        std::fs::write(directory.0.join("work/body"), b"first").unwrap();
        replace(Replacement::NoReplace).unwrap();
        assert!(!directory.0.join("work/body").exists());
        std::fs::write(directory.0.join("work/body"), b"second").unwrap();
        assert!(
            matches!(replace(Replacement::NoReplace), Err(FsError::Io(e)) if e.kind()==io::ErrorKind::AlreadyExists)
        );
        replace(Replacement::Exchange).unwrap();
        assert_eq!(
            std::fs::read(directory.0.join("work/body")).unwrap(),
            b"first"
        );
        assert_eq!(
            std::fs::read(directory.0.join("bucket/nested/body")).unwrap(),
            b"second"
        );
        std::fs::write(directory.0.join("work/body"), b"third").unwrap();
        replace(Replacement::Exchange).unwrap();
        assert_eq!(
            std::fs::read(directory.0.join("work/body")).unwrap(),
            b"second"
        );
        assert_eq!(
            std::fs::read(directory.0.join("bucket/nested/body")).unwrap(),
            b"third"
        );
    }

    /// A power loss right after the replacement returns keeps the staged bytes under the
    /// destination name, and an exchange keeps the displaced bytes under the staged name. The
    /// call records its rename before its parent syncs, so only the syncs inside the call can
    /// make it durable: dropping the staged file's sync or either parent's sync fails this test.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn replacement_between_directories_is_durable_when_it_returns() {
        use crate::storage::fault::{self, Fault, Step};
        for replacement in [Replacement::NoReplace, Replacement::Exchange] {
            let directory = crate::test_support::TestDir::new();
            let snapshot = crate::test_support::TestDir::new();
            let snapshot = snapshot.0.join("power-loss");
            let root = open_directory_nofollow(&directory.0).unwrap();
            let workspace =
                directory_path_at(&root, "work".as_ref(), DirectoryOpen::CreateIfMissing).unwrap();
            let target =
                directory_path_at(&root, "bucket".as_ref(), DirectoryOpen::CreateIfMissing)
                    .unwrap();
            let staged = directory.0.join("work/body");
            let destination = directory.0.join("bucket/body");
            if replacement == Replacement::Exchange {
                std::fs::write(&destination, b"displaced").unwrap();
            }
            fault::arm(
                &directory.0,
                Step::SyncCopyRenamed,
                0,
                Fault::PowerLoss(snapshot.clone()),
            );
            // Written after arming, so neither its bytes nor its entry are durable yet.
            std::fs::write(&staged, b"staged").unwrap();
            fault::file_created(&staged);
            durable_replace_between(
                &workspace,
                "body".as_ref(),
                &target,
                "body".as_ref(),
                replacement,
            )
            .unwrap();
            fault::hit(&directory.0, Step::SyncCopyRenamed).unwrap();
            assert_eq!(fault::pending(&directory.0), 0);
            assert_eq!(
                std::fs::read(snapshot.join("bucket/body")).unwrap(),
                b"staged",
                "{replacement:?}"
            );
            match replacement {
                Replacement::Exchange => assert_eq!(
                    std::fs::read(snapshot.join("work/body")).unwrap(),
                    b"displaced"
                ),
                Replacement::NoReplace => assert!(!snapshot.join("work/body").exists()),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn identity_observes_nanosecond_metadata_changes() {
        use std::os::unix::fs::MetadataExt;
        let directory = crate::test_support::TestDir::new();
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
        let directory = crate::test_support::TestDir::new();
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
pub(crate) mod test_support {
    //! Test-only snapshot creation; production source readers never call this module.
    use std::{fs, io};

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
