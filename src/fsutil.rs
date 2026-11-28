#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Descriptor-based filesystem operations; OS-specific unsafe stays in this module.
//!
//! This file holds the types every platform shares and the functions whose body picks a
//! platform's module with `cfg_select!`. `unix` holds what Unix platforms share, including a
//! step that Linux and macOS spell with different calls inside one shared body, such as the
//! exclusive rename and the xattr read: a `cfg_select!` there picks the spelling. `linux`,
//! `apple` and `windows` hold the operations only that platform has, and `unsupported` answers
//! the descriptor-relative lookups where no Unix descriptor exists. The platform modules name
//! `unix` and the shared types, never each other, and only this file chooses between them.

use std::{collections::VecDeque, ffi::OsString, fs::File, io};
mod lookup;

#[cfg(target_vendor = "apple")]
mod apple;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(not(unix))]
mod unsupported;
#[cfg(not(unix))]
use unsupported as imp;
#[cfg(windows)]
mod windows;

pub(crate) use imp::directory_at;
use imp::directory_names_within;
pub(crate) use imp::{directory_names_after, open_directory_nofollow, open_regular_at};
pub(crate) use lookup::Lookup;
#[cfg(all(test, unix))]
pub(crate) use lookup::Matching;
#[cfg(unix)]
pub(crate) use unix::{NodeId, rename_noreplace};
#[cfg(windows)]
pub(crate) use windows::move_write_through;

pub(crate) fn open_directory(path: &std::path::Path) -> io::Result<File> {
    cfg_select! {
        windows => { windows::open_directory(path) }
        _ => { File::open(path) }
    }
}

/// What a spelling-checked lookup of a directory found. Only tests ask it, and only which answer
/// it gave, so the opened directory is not handed back.
#[cfg(all(test, unix))]
#[derive(Debug)]
pub(crate) enum Child {
    Directory,
    Alias,
    Missing(Vacant),
    /// The name kept failing to open while an entry stood under it ([`Opened::Unopenable`]).
    Unopenable,
}

/// What a spelling-checked lookup of a regular file found, as [`Child`] says for a directory.
#[cfg(all(test, unix))]
#[derive(Debug)]
pub(crate) enum RegularChild {
    File,
    /// A regular file that the name reached under another letter case or normalization of it. A
    /// listing that fails to tell the spelling the directory lists it under does not fail the
    /// lookup: the spelling still differs.
    Respelled,
    Missing(Vacant),
    /// The name kept failing to open while an entry stood under it ([`Opened::Unopenable`]).
    Unopenable,
}

/// What opening a name found, under the rule that only `fstatat` finding nothing shows that the
/// name holds nothing. A reader that takes a name for empty treats the file it held as removed,
/// and a Docker Desktop bind mount fails an open with `ENOENT` for a moment after the host
/// replaced a file while `fstatat` still finds it
/// (`docs/en/evidence/sync-bind-mount-stale-reads.md`), so such an open is tried again rather
/// than taken for an empty name.
#[cfg_attr(
    not(unix),
    expect(
        dead_code,
        reason = "descriptor-relative lookup is unsupported on this platform"
    )
)]
#[cfg_attr(test, derive(Debug))]
pub(crate) enum Opened<T> {
    /// The entry the name holds.
    Entry(T),
    /// Nothing: `fstatat`, not following a symlink, found nothing under the name.
    Missing(Vacant),
    /// The open kept failing with `ENOENT` while `fstatat` still found an entry under the name,
    /// until the retries ran out. Whether the name holds anything is unknown, and a later read
    /// decides. No error comes with it: the answer is made only that way, so the open's error
    /// tells nothing the variant does not, and a caller that must fail uses [`Unopenable`].
    Unopenable,
}

/// A name that kept failing to open while `fstatat` still found an entry under it
/// ([`Opened::Unopenable`]), as the error of a step that fails rather than waits for a later read.
/// Errors carry it as a variant of their own, never inside an `io::Error`: there it read as the
/// kind the open failed with, which each caller judged its own way, and none as a failure that
/// passes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the entry kept failing to open while its name still held one")]
pub(crate) struct Unopenable;

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
#[cfg(all(test, unix))]
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
            if let Some(last) = page.last() {
                self.after = Some(last.clone());
            }
            self.buffered = page.into();
        }
        Ok(self.buffered.pop_front())
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum FsError {
    #[error(transparent)]
    Io(io::Error),
    #[error("filesystem operation is unsupported")]
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

// Windows write/change timestamps can repeat within a clock tick, so equality there can miss
// same-size rewrites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    pub(crate) device: u64,
    pub(crate) inode: u64,
    pub(crate) size: u64,
    pub(crate) modified_ns: i128,
    pub(crate) changed_ns: i128,
}

/// How long after a file's last change an observation of it is settled, in nanoseconds: two
/// seconds cover the coarsest supported timestamp granularity (HFS+ and FAT).
const SETTLED_AFTER_NS: i128 = 2_000_000_000;

impl FileIdentity {
    /// Whether these timestamps, observed at `observed_ns`, would show any later change: the last
    /// modification or status change lies `SETTLED_AFTER_NS` or more before the observation.
    /// Closer to it, or after it, a change in the same tick can leave the size and every
    /// timestamp as they were, as with git's racily clean index entries, so the observation is
    /// racy. The status change counts as well, since a copy that keeps the modification time,
    /// such as `cp -p`, changes only that.
    pub(crate) fn settled_at(&self, observed_ns: i128) -> bool {
        observed_ns.saturating_sub(self.modified_ns.max(self.changed_ns)) >= SETTLED_AFTER_NS
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
#[cfg(unix)]
pub(crate) fn sync_ordered(file: &File) -> io::Result<()> {
    cfg_select! {
        target_vendor = "apple" => { apple::sync_ordered(file) }
        _ => { file.sync_all() }
    }
}

#[derive(Debug)]
pub(crate) enum CloneFailure {
    NotCreated(FsError),
    #[cfg_attr(
        all(not(test), not(any(target_os = "linux", target_os = "macos"))),
        expect(
            dead_code,
            reason = "outside tests, only supported clone platforms create a destination before failure"
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

// Both outcomes expose the same FsError they display. Revisit with the clone diagnostic contract.
impl std::error::Error for CloneFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotCreated(error) | Self::Created(error) => Some(error),
        }
    }
}

#[cfg(test)]
pub(crate) fn clone_failure_pair(
    outer: &(dyn std::error::Error + 'static),
    inner: &(dyn std::error::Error + 'static),
) -> bool {
    matches!(outer.downcast_ref(), Some(CloneFailure::NotCreated(error) | CloneFailure::Created(error))
        if inner.downcast_ref().is_some_and(|source| std::ptr::eq(error, source)))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
thread_local! {
    static FAIL_CLONE_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Reports that the filesystem cannot clone, as ext4 and Docker Desktop's shared folders do.
    static CLONE_UNSUPPORTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn stat_identity(file: &File) -> Result<FileIdentity, FsError> {
    cfg_select! {
        unix => { Ok(FileIdentity::from(&file.metadata()?)) }
        windows => { windows::stat_identity(file) }
    }
}

/// The identity of `file` as its filesystem reports it now. Linux asks the filesystem itself,
/// past the attribute cache that a FUSE, virtiofs or NFS mount answers `fstat` from. Other
/// platforms have no such request, so a lookup there sees what a network mount's attribute
/// cache answers.
pub(crate) fn uncached_identity(file: &File) -> io::Result<FileIdentity> {
    cfg_select! {
        target_os = "linux" => { linux::uncached_identity(file) }
        _ => { Ok(stat_identity(file)?) }
    }
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

/// Creates a clone without copying; Unsupported leaves no new entry.
/// Mutable-source staging can then fall back to its own single-pass copy and hash.
pub(crate) fn clone_into(
    src: &File,
    directory: &File,
    name: &std::ffi::OsStr,
) -> Result<(), CloneFailure> {
    validate_name(name)?;
    regular_source(src)?;
    cfg_select! {
        target_os = "linux" => { linux::clone_into(src, directory, name) }
        target_os = "macos" => { apple::clone_into(src, directory, name) }
        _ => {
            let _ = directory;
            Err(CloneFailure::NotCreated(FsError::Unsupported))
        }
    }
}

pub(crate) fn read_at(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    cfg_select! {
        unix => { std::os::unix::fs::FileExt::read_at(file, bytes, offset) }
        windows => { std::os::windows::fs::FileExt::seek_read(file, bytes, offset) }
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
pub(crate) fn read_cached_at(file: &File, bytes: &mut [u8], offset: u64) -> Option<usize> {
    cfg_select! {
        all(target_os = "linux", any(target_env = "gnu", target_env = "musl")) => {
            linux::read_cached_at(file, bytes, offset)
        }
        _ => {
            let _ = (file, bytes, offset);
            None
        }
    }
}

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

/// Names whose opens fail with `ENOENT` while the entry stays, as a Docker Desktop bind mount
/// answers for a file it is still bringing over from the host ([`Opened::Unopenable`]): by the
/// device and inode number of the directory, the name, how many opens succeed first and how many
/// fail after them. Opening an existing regular file or directory by name fails that way;
/// `fstatat` still finds the entry. Tests run in parallel, so each adds its own name and removes
/// only that one again.
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
static FAILING_OPENS: std::sync::Mutex<Vec<FailingOpen>> = std::sync::Mutex::new(Vec::new());

/// A name in [`FAILING_OPENS`].
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
struct FailingOpen {
    directory: (u64, u64),
    name: OsString,
    skip: usize,
    left: usize,
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
        let identity = stat_identity(&File::open(directory).unwrap()).unwrap();
        let directory = (identity.device, identity.inode);
        FAILING_OPENS.lock().unwrap().push(FailingOpen {
            directory,
            name: name.into(),
            skip,
            left: opens,
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
    Err(io::ErrorKind::NotFound.into())
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

#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn checked_length(length: usize) -> Result<Option<usize>, SourceMetadataError> {
    if length > SOURCE_METADATA_CAP {
        Err(SourceMetadataError::TooLarge)
    } else {
        Ok(Some(length))
    }
}

/// The dedicated buffer prevents callers from supplying a smaller allocation.
pub(crate) fn read_source_metadata<'a>(
    file: &File,
    buffer: &'a mut SourceMetadataBuffer,
) -> Result<Option<&'a [u8]>, SourceMetadataError> {
    cfg_select! {
        any(target_os = "linux", target_os = "macos") => {
            unix::read_source_metadata(file, buffer)
        }
        _ => {
            let _ = (file, &mut buffer.0);
            Err(SourceMetadataError::Unsupported)
        }
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

    /// A clone that fails after it created its entry says so and leaves the entry for its caller.
    /// A name that already holds an entry, and a filesystem that cannot clone, such as ext4 or
    /// Docker Desktop's shared folders, create nothing and leave no entry behind.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn clone_failure_reports_whether_it_created_the_entry() {
        let directory = crate::test_support::TestDir::new();
        std::fs::write(directory.0.join("source"), b"source").unwrap();
        let source = File::open(directory.0.join("source")).unwrap();
        let root = File::open(&directory.0).unwrap();
        FAIL_CLONE_SYNC.set(true);
        std::assert_matches!(
            clone_into(&source, &root, "clone".as_ref()),
            Err(CloneFailure::Created(_))
        );
        assert!(directory.0.join("clone").exists());
        std::fs::write(directory.0.join("existing"), b"other owner").unwrap();
        std::assert_matches!(
            clone_into(&source, &root, "existing".as_ref()),
            Err(CloneFailure::NotCreated(_))
        );
        assert_eq!(
            std::fs::read(directory.0.join("existing")).unwrap(),
            b"other owner"
        );
        CLONE_UNSUPPORTED.set(true);
        let refused = clone_into(&source, &root, "unsupported".as_ref());
        CLONE_UNSUPPORTED.set(false);
        std::assert_matches!(refused, Err(CloneFailure::NotCreated(FsError::Unsupported)));
        assert!(!directory.0.join("unsupported").exists());
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
        std::assert_matches!(
            checked_length(SOURCE_METADATA_CAP + 1),
            Err(SourceMetadataError::TooLarge)
        );
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
                    std::assert_matches!(
                        error.raw_os_error(),
                        Some(libc::E2BIG | libc::ENOSPC | libc::ERANGE)
                    );
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
                std::assert_matches!(
                    read_source_metadata(&file, &mut buffer),
                    Err(SourceMetadataError::TooLarge)
                );
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
        cfg_select! {
            any(target_os = "linux", target_os = "macos") => {
                use std::os::fd::AsRawFd;
                // SAFETY: the test owns the live descriptor; the literal key and input
                // allocation remain valid for the synchronous Linux/macOS ABI call.
                let result = unsafe {
                    cfg_select! {
                        target_os = "linux" => {
                            libc::fsetxattr(
                                file.as_raw_fd(),
                                c"user.metadata".as_ptr(),
                                bytes.as_ptr().cast(),
                                bytes.len(),
                                0,
                            )
                        }
                        target_os = "macos" => {
                            libc::fsetxattr(
                                file.as_raw_fd(),
                                c"user.metadata".as_ptr(),
                                bytes.as_ptr().cast(),
                                bytes.len(),
                                0,
                                0,
                            )
                        }
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
            _ => {
                let _ = (file, bytes);
                Ok(false)
            }
        }
    }
}

#[cfg(test)]
#[test]
fn error_text_and_sources() {
    let cases: &[(&dyn std::error::Error, &str)] = &[
        (
            &Unopenable,
            "the entry kept failing to open while its name still held one",
        ),
        (&FsError::Unsupported, "filesystem operation is unsupported"),
        (
            &FsError::Io(std::io::ErrorKind::NotFound.into()),
            "entity not found",
        ),
    ];
    for (error, expected) in cases {
        assert_eq!(error.to_string(), *expected);
        assert!(error.source().is_none());
    }
    for error in [
        CloneFailure::Created(FsError::Unsupported),
        CloneFailure::NotCreated(FsError::Unsupported),
    ] {
        let cause = std::error::Error::source(&error).unwrap();
        assert!(cause.is::<FsError>());
        assert_eq!(error.to_string(), "filesystem operation is unsupported");
        assert_eq!(cause.to_string(), error.to_string());
        assert_eq!(
            crate::error::Report::chain(&error).to_string(),
            error.to_string()
        );
    }
}
