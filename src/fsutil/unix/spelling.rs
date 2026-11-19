use super::*;
use crate::fsutil::{Child, Created, DirectoryNames, Lookup, RegularChild, lookup::Known};

pub(crate) fn child_directory(
    parent: &File,
    name: &OsStr,
    open: DirectoryOpen,
    lookup: &Lookup,
) -> io::Result<Child> {
    let (file, created) = match directory_at(parent, name, DirectoryOpen::Existing) {
        Ok(file) => (file, Created::No),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if open == DirectoryOpen::Existing {
                return Ok(Child::Missing);
            }
            create_child(parent, name)?
        }
        Err(error) => return Err(error),
    };
    if spelled_as_opened(parent, name, &file, lookup)? {
        Ok(Child::Directory { file, created })
    } else {
        Ok(Child::Alias)
    }
}

#[expect(
    unsafe_code,
    reason = "mkdirat reports whether this call created the pinned child entry"
)]
fn create_child(parent: &File, name: &OsStr) -> io::Result<(File, Created)> {
    let component = component(name)?;
    // SAFETY: the validated component and live parent descriptor remain valid for the call.
    let created = if unsafe { libc::mkdirat(parent.as_raw_fd(), component.as_ptr(), 0o700) } == 0 {
        Created::Yes
    } else {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error);
        }
        Created::No
    };
    Ok((
        directory_at(parent, name, DirectoryOpen::Existing)?,
        created,
    ))
}

pub(crate) fn child_regular(
    parent: &File,
    name: &OsStr,
    lookup: &Lookup,
) -> io::Result<RegularChild> {
    let file = match open_regular_at(parent, name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(RegularChild::Missing),
        Err(error) => return Err(error),
    };
    if spelled_as_opened(parent, name, &file, lookup)? {
        Ok(RegularChild::File(file))
    } else {
        Ok(RegularChild::Alias)
    }
}

/// Whether `file`, opened as `name` in `parent`, has that exact spelling there. Tests can take
/// the platform to answer that it does ([`crate::fsutil::UNCHECKED_SPELLING`]), which a lookup
/// below a root that folds names does not believe.
fn spelled_as_opened(
    parent: &File,
    name: &OsStr,
    file: &File,
    lookup: &Lookup,
) -> io::Result<bool> {
    let known = lookup.known(parent)?;
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    {
        let identity = crate::fsutil::stat_identity(parent)?;
        if crate::fsutil::UNCHECKED_SPELLING
            .lock()
            .map_err(|_| io::Error::other("spelling injection poisoned"))?
            .contains(&(identity.device, identity.inode))
        {
            return flags_spelling(parent, name, Some(0), known);
        }
    }
    exact_spelling(parent, name, file, known)
}

fn listed_spelling(parent: &File, name: &OsStr) -> io::Result<bool> {
    let mut names = DirectoryNames::default();
    while let Some(entry) = names.next_name(parent, usize::MAX)? {
        if entry == name {
            return Ok(true);
        }
    }
    Ok(false)
}

/// APFS and HFS+ are case- and normalization-insensitive or sensitive per volume, and the name
/// they report for an open file is the one its directory lists, so the root's rules only spare
/// asking on a root that keeps names apart.
#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "fgetattrlist returns the descriptor's actual name without scanning its parent"
)]
fn exact_spelling(parent: &File, name: &OsStr, file: &File, known: Known) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    match known {
        Known::Exact => return Ok(true),
        Known::Nothing | Known::Folding => {}
    }
    let metadata = file.metadata()?;
    if metadata.is_file() && metadata.nlink() > 1 {
        return listed_spelling(parent, name);
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

#[cfg(target_os = "linux")]
#[expect(
    unsafe_code,
    reason = "the parent casefold flag is available only through FS_IOC_GETFLAGS"
)]
fn exact_spelling(parent: &File, name: &OsStr, _: &File, known: Known) -> io::Result<bool> {
    let mut flags: libc::c_int = 0;
    // SAFETY: the live descriptor and writable flags word satisfy the ioctl contract.
    let result = unsafe { libc::ioctl(parent.as_raw_fd(), libc::FS_IOC_GETFLAGS, &mut flags) };
    flags_spelling(parent, name, (result == 0).then_some(flags), known)
}

/// Decides a spelling from the parent's casefold flag, `None` where the filesystem does not
/// report flags (`ENOTTY` on FUSE and network mounts), and what the root's rules say about the
/// parent. A set flag always needs the listing: ext4 sets it per directory, so the rules of a
/// root that keeps names apart do not cover such a directory.
#[cfg(any(target_os = "linux", test))]
fn flags_spelling(
    parent: &File,
    name: &OsStr,
    flags: Option<libc::c_int>,
    known: Known,
) -> io::Result<bool> {
    const CASEFOLD: libc::c_int = 0x4000_0000;
    let casefolded = flags.map(|flags| flags & CASEFOLD != 0);
    match (casefolded, known) {
        (Some(true), Known::Nothing | Known::Exact | Known::Folding)
        | (None, Known::Nothing | Known::Folding)
        | (Some(false), Known::Folding) => listed_spelling(parent, name),
        (None | Some(false), Known::Exact) | (Some(false), Known::Nothing) => Ok(true),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exact_spelling(parent: &File, name: &OsStr, _: &File, known: Known) -> io::Result<bool> {
    match known {
        Known::Exact => Ok(true),
        Known::Nothing | Known::Folding => listed_spelling(parent, name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flag answer that does not settle the spelling falls back to the parent's listing, which
    /// tells the exact spelling apart from another one.
    #[test]
    fn unavailable_casefold_flags_fall_back_to_exact_directory_names() {
        let dir = crate::test_support::TestDir::new();
        std::fs::write(dir.0.join("Actual"), b"bytes").unwrap();
        let parent = File::open(&dir.0).unwrap();
        for flags in [None, Some(0x4000_0000)] {
            assert!(flags_spelling(&parent, "Actual".as_ref(), flags, Known::Nothing).unwrap());
            assert!(!flags_spelling(&parent, "actual".as_ref(), flags, Known::Nothing).unwrap());
        }
    }

    /// Below a root that folds names, a clear casefold flag is not believed, as a mount that folds
    /// without the flag reports one; below a root that keeps names apart, an unavailable flag
    /// needs no listing, while a set flag, which ext4 sets per directory, still does. A name the
    /// rules accept without a listing is one that opened, so `actual` passes there.
    #[test]
    fn root_rules_decide_only_what_the_casefold_flag_leaves_open() {
        let dir = crate::test_support::TestDir::new();
        std::fs::write(dir.0.join("Actual"), b"bytes").unwrap();
        let parent = File::open(&dir.0).unwrap();
        let spelled = |flags, known| flags_spelling(&parent, "actual".as_ref(), flags, known);
        assert!(!spelled(Some(0), Known::Folding).unwrap());
        assert!(!spelled(None, Known::Folding).unwrap());
        assert!(!spelled(Some(0x4000_0000), Known::Folding).unwrap());
        assert!(!spelled(Some(0x4000_0000), Known::Exact).unwrap());
        assert!(spelled(None, Known::Exact).unwrap());
        assert!(spelled(Some(0), Known::Exact).unwrap());
        assert!(spelled(Some(0), Known::Nothing).unwrap());
    }
}
