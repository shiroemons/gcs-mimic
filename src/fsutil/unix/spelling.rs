use super::*;
#[cfg(test)]
use crate::fsutil::{Child, ListedSpelling, RegularChild};
use crate::fsutil::{Lookup, Opened, Vacant};
use std::time::Duration;

/// The first wait before a name that failed to open is opened again; each later wait doubles.
const FIRST_RETRY: Duration = Duration::from_millis(2);

/// Opens `name` in `parent` with `open`. Only `fstatat`, not following a symlink, finding nothing
/// under the name makes it [`Opened::Missing`]: an open that fails with `ENOENT` while `fstatat`
/// still finds an entry is tried again after waits that start short and double, and gives up as
/// [`Opened::Unopenable`] once the lookup has no waiting left (`Lookup::pause`). The waits block
/// the calling thread and pass on the lookup's clock.
fn open_entry<T>(
    parent: &File,
    name: &OsStr,
    lookup: &Lookup,
    mut open: impl FnMut() -> io::Result<T>,
) -> io::Result<Opened<T>> {
    let mut wait = FIRST_RETRY;
    loop {
        match open() {
            Ok(entry) => return Ok(Opened::Entry(entry)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if !holds_entry(parent, name)? {
            return Ok(Opened::Missing(Vacant::proven()));
        }
        if !lookup.pause(wait) {
            return Ok(Opened::Unopenable);
        }
        wait = wait.saturating_mul(2);
    }
}

/// Whether `fstatat`, not following a symlink, finds an entry under `name` in `parent`.
fn holds_entry(parent: &File, name: &OsStr) -> io::Result<bool> {
    Ok(stat_at(parent, &component(name)?)?.is_some())
}

/// What `fstatat`, not following a symlink, reports for `name` in `parent`; `None` when it finds
/// nothing under the name.
#[expect(
    unsafe_code,
    reason = "fstatat on a pinned descriptor has no safe std equivalent"
)]
fn stat_at(parent: &File, name: &CStr) -> io::Result<Option<libc::stat>> {
    let mut status = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: the live descriptor, the terminated name and the writable buffer are valid for the
    // call, which keeps none of them.
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            status.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        // SAFETY: fstatat succeeded, and the zeroed buffer was valid for every field before it
        // wrote.
        return Ok(Some(unsafe { status.assume_init() }));
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(error)
    }
}

pub(crate) fn regular_unconfirmed(
    parent: &File,
    name: &OsStr,
    lookup: &Lookup,
) -> io::Result<Opened<File>> {
    open_entry(parent, name, lookup, || open_regular_at(parent, name))
}

pub(crate) fn directory_unconfirmed(
    parent: &File,
    name: &OsStr,
    lookup: &Lookup,
) -> io::Result<Opened<File>> {
    open_entry(parent, name, lookup, || directory_at(parent, name))
}

#[cfg(test)]
pub(crate) fn child_directory(parent: &File, name: &OsStr, lookup: &Lookup) -> io::Result<Child> {
    let file = match directory_unconfirmed(parent, name, lookup)? {
        Opened::Entry(file) => file,
        Opened::Missing(vacant) => return Ok(Child::Missing(vacant)),
        Opened::Unopenable => return Ok(Child::Unopenable),
    };
    if spelled_as_opened(parent, name, &file, lookup)? {
        Ok(Child::Directory)
    } else {
        Ok(Child::Alias)
    }
}

#[cfg(test)]
pub(crate) fn child_regular(
    parent: &File,
    name: &OsStr,
    lookup: &Lookup,
) -> io::Result<RegularChild> {
    let file = match regular_unconfirmed(parent, name, lookup)? {
        Opened::Entry(file) => file,
        Opened::Missing(vacant) => return Ok(RegularChild::Missing(vacant)),
        Opened::Unopenable => return Ok(RegularChild::Unopenable),
    };
    if spelled_as_opened(parent, name, &file, lookup)? {
        return Ok(RegularChild::File);
    }
    match listed_spelling(parent, &file) {
        // The listing read now spells the file as it was opened. What answered otherwise, such as
        // a listing kept from before a change the directory's timestamps did not show, is older
        // than this listing, which is the answer; a kept listing is not trusted again.
        Ok(ListedSpelling::Listed(spelling)) if OsStr::new(&spelling) == name => {
            lookup.forget(parent)?;
            Ok(RegularChild::File)
        }
        Ok(ListedSpelling::Listed(_) | ListedSpelling::Unnamed) | Err(_) => {
            Ok(RegularChild::Respelled)
        }
    }
}

/// The spelling under which `parent` lists `file`, a regular file opened there under another
/// one, found by comparing each listed entry's device and inode with the file's, since only a
/// listing tells a Linux casefolded directory's spelling. More than one name linking the file, a
/// name that is not UTF-8 and a file no longer listed name no one spelling to report. A listed
/// entry that cannot be read is skipped: with one link only, it cannot be a second name of the
/// file, and a spelling found elsewhere is still the file's one.
#[cfg(test)]
fn listed_spelling(parent: &File, file: &File) -> io::Result<ListedSpelling> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Ok(ListedSpelling::Unnamed);
    }
    let mut found: Option<OsString> = None;
    super::each_name(parent, |listed| {
        let Ok(component) = component(listed) else {
            return Ok(ControlFlow::Continue(()));
        };
        let Ok(Some(status)) = stat_at(parent, &component) else {
            return Ok(ControlFlow::Continue(()));
        };
        if status.st_mode & libc::S_IFMT == libc::S_IFREG
            && listed_device(&status) == metadata.dev()
            && status.st_ino == metadata.ino()
        {
            found = Some(listed.to_owned());
            return Ok(ControlFlow::Break(()));
        }
        Ok(ControlFlow::Continue(()))
    })?;
    Ok(found
        .and_then(|spelling| spelling.into_string().ok())
        .map_or(ListedSpelling::Unnamed, ListedSpelling::Listed))
}

/// A listed entry's device as [`std::os::unix::fs::MetadataExt::dev`] reports it, which widens
/// macOS's signed `dev_t` the way a cast does, so that the two compare equal.
#[cfg(all(test, target_os = "macos"))]
fn listed_device(status: &libc::stat) -> u64 {
    i64::from(status.st_dev).cast_unsigned()
}

#[cfg(all(test, not(target_os = "macos")))]
fn listed_device(status: &libc::stat) -> u64 {
    status.st_dev
}

/// Whether `file`, opened as `name` in `parent`, has that exact spelling there. Tests can take
/// the platform to answer that it does ([`crate::fsutil::UNCHECKED_SPELLING`]), which a lookup
/// below a root that folds names does not believe.
#[cfg(test)]
fn spelled_as_opened(
    parent: &File,
    name: &OsStr,
    file: &File,
    lookup: &Lookup,
) -> io::Result<bool> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let identity = crate::fsutil::stat_identity(parent)?;
        if crate::fsutil::UNCHECKED_SPELLING
            .lock()
            .map_err(|_| io::Error::other("spelling injection poisoned"))?
            .contains(&(identity.device, identity.inode))
        {
            return flags_spelling(parent, name, Some(0), lookup);
        }
    }
    exact_spelling(parent, name, file, lookup)
}

/// APFS and HFS+ are case- and normalization-insensitive or sensitive per volume, and the name
/// they report for an open file is the one its directory lists. A mount below the root can fold
/// names although the root does not, so each file is asked whatever the root's rules say.
#[cfg(all(test, target_os = "macos"))]
#[expect(
    unsafe_code,
    reason = "fgetattrlist returns the descriptor's actual name without scanning its parent"
)]
fn exact_spelling(parent: &File, name: &OsStr, file: &File, lookup: &Lookup) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    if metadata.is_file() && metadata.nlink() > 1 {
        return lookup.listed(parent, name);
    }
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_NAME,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buffer = [0u8; 1024];
    // SAFETY: the live descriptor, initialized attribute list and output buffer are valid.
    if unsafe {
        libc::fgetattrlist(
            file.as_raw_fd(),
            (&raw mut attributes).cast(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            0,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let invalid = || io::Error::from(io::ErrorKind::InvalidData);
    let offset = i32::from_ne_bytes(buffer[4..8].try_into().map_err(|_| invalid())?);
    let length = u32::from_ne_bytes(buffer[8..12].try_into().map_err(|_| invalid())?) as usize;
    let start = 4usize
        .checked_add_signed(offset as isize)
        .ok_or_else(invalid)?;
    let end = start.checked_add(length).ok_or_else(invalid)?;
    let bytes = buffer
        .get(start..end)
        .and_then(|bytes| bytes.strip_suffix(&[0]))
        .ok_or_else(invalid)?;
    Ok(bytes == name.as_bytes())
}

#[cfg(all(test, target_os = "linux"))]
#[expect(
    unsafe_code,
    reason = "the parent casefold flag is available only through FS_IOC_GETFLAGS"
)]
fn exact_spelling(parent: &File, name: &OsStr, _: &File, lookup: &Lookup) -> io::Result<bool> {
    let mut flags: libc::c_int = 0;
    // SAFETY: the live descriptor and writable flags word satisfy the ioctl contract.
    let result = unsafe { libc::ioctl(parent.as_raw_fd(), libc::FS_IOC_GETFLAGS, &mut flags) };
    flags_spelling(parent, name, (result == 0).then_some(flags), lookup)
}

/// Decides a spelling from the parent's casefold flag, `None` where the filesystem does not
/// report flags (`ENOTTY` on FUSE, 9p and network mounts), and what the root's rules say. Only a
/// clear flag spares the listing, and only below a root that the probe did not find to fold
/// names. A mount that cannot report the flag cannot report that one directory folds names while
/// another does not either, as a WSL2 `drvfs` mount with `case=dir` does on one device, so the
/// probe of one directory says nothing about the others there.
#[cfg(test)]
fn flags_spelling(
    parent: &File,
    name: &OsStr,
    flags: Option<libc::c_int>,
    lookup: &Lookup,
) -> io::Result<bool> {
    use crate::fsutil::lookup::Known;
    const CASEFOLD: libc::c_int = 0x4000_0000;
    let casefolded = flags.map(|flags| flags & CASEFOLD != 0);
    match (casefolded, lookup.known()) {
        (Some(false), Known::Nothing) => Ok(true),
        (Some(true) | None, Known::Nothing | Known::Folding) | (Some(false), Known::Folding) => {
            lookup.listed(parent, name)
        }
    }
}

#[cfg(all(test, not(any(target_os = "linux", target_os = "macos"))))]
fn exact_spelling(parent: &File, name: &OsStr, _: &File, lookup: &Lookup) -> io::Result<bool> {
    lookup.listed(parent, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{clock::SystemClock, fsutil::Matching};
    use std::sync::Arc;

    /// A listing names the spelling of the file a name reached, also under another letter case
    /// where the directory folds it, and no spelling when nothing is there or another name links
    /// the file too, since either name could be the one to report.
    #[test]
    fn a_listing_names_the_spelling_of_the_file_a_name_reached() {
        let dir = crate::test_support::TestDir::new();
        std::fs::write(dir.0.join("Actual"), b"bytes").unwrap();
        let parent = File::open(&dir.0).unwrap();
        let spelling = |name: &str| {
            let file = super::super::open_regular_at(&parent, name.as_ref()).ok()?;
            match listed_spelling(&parent, &file).unwrap() {
                ListedSpelling::Listed(spelling) => Some(spelling),
                ListedSpelling::Unnamed => None,
            }
        };
        assert_eq!(spelling("Actual").as_deref(), Some("Actual"));
        assert_eq!(spelling("missing"), None);
        if dir.0.join("actual").exists() {
            assert_eq!(spelling("actual").as_deref(), Some("Actual"));
        }
        std::fs::hard_link(dir.0.join("Actual"), dir.0.join("Linked")).unwrap();
        assert_eq!(spelling("Actual"), None);
    }

    /// A flag answer that does not settle the spelling falls back to the parent's listing, which
    /// tells the exact spelling apart from another one.
    #[test]
    fn unavailable_casefold_flags_fall_back_to_exact_directory_names() {
        let dir = crate::test_support::TestDir::new();
        std::fs::write(dir.0.join("Actual"), b"bytes").unwrap();
        let parent = File::open(&dir.0).unwrap();
        for flags in [None, Some(0x4000_0000)] {
            let lookup = Lookup::unprobed();
            assert!(flags_spelling(&parent, "Actual".as_ref(), flags, &lookup).unwrap());
            assert!(!flags_spelling(&parent, "actual".as_ref(), flags, &lookup).unwrap());
        }
    }

    /// The root's rules only make the check stricter. Below a root that folds names, a clear
    /// casefold flag is not believed, as a mount that folds without the flag reports one. Below a
    /// root that keeps names apart, only a clear flag spares the listing: an unavailable flag is
    /// listed, since a mount that cannot report it can fold names in some directories and not in
    /// the one the probe measured. A name accepted without a listing is one that opened, so
    /// `actual` passes there.
    #[test]
    fn root_rules_decide_only_what_the_casefold_flag_leaves_open() {
        let dir = crate::test_support::TestDir::new();
        std::fs::write(dir.0.join("Actual"), b"bytes").unwrap();
        let parent = File::open(&dir.0).unwrap();
        let probed = |matching| Lookup::probed(matching, Arc::new(SystemClock));
        let spelled =
            |flags, lookup: Lookup| flags_spelling(&parent, "actual".as_ref(), flags, &lookup);
        assert!(!spelled(Some(0), probed(Matching::Folding)).unwrap());
        assert!(!spelled(None, probed(Matching::Folding)).unwrap());
        assert!(!spelled(Some(0x4000_0000), probed(Matching::Folding)).unwrap());
        assert!(!spelled(Some(0x4000_0000), probed(Matching::Exact)).unwrap());
        assert!(!spelled(None, probed(Matching::Exact)).unwrap());
        assert!(spelled(Some(0), probed(Matching::Exact)).unwrap());
        assert!(spelled(Some(0), Lookup::unprobed()).unwrap());
    }
}
