use super::*;
use crate::fsutil::{Child, Created, RegularChild};

pub(crate) fn child_directory(
    parent: &File,
    name: &OsStr,
    open: DirectoryOpen,
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
    if exact_spelling(parent, name, &file)? {
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

pub(crate) fn child_regular(parent: &File, name: &OsStr) -> io::Result<RegularChild> {
    let file = match open_regular_at(parent, name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(RegularChild::Missing),
        Err(error) => return Err(error),
    };
    if exact_spelling(parent, name, &file)? {
        Ok(RegularChild::File(file))
    } else {
        Ok(RegularChild::Alias)
    }
}

fn listed_spelling(parent: &File, name: &OsStr) -> io::Result<bool> {
    let mut after = None;
    loop {
        let page = directory_names_after(parent, after.as_deref(), 256)?;
        if page.iter().any(|entry| entry == name) {
            return Ok(true);
        }
        if page.len() < 256 {
            return Ok(false);
        }
        after = page.last().cloned();
    }
}

#[cfg(target_os = "macos")]
#[expect(
    unsafe_code,
    reason = "fgetattrlist returns the descriptor's actual name without scanning its parent"
)]
fn exact_spelling(parent: &File, name: &OsStr, file: &File) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
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
fn exact_spelling(parent: &File, name: &OsStr, _: &File) -> io::Result<bool> {
    let mut flags: libc::c_int = 0;
    // SAFETY: the live descriptor and writable flags word satisfy the ioctl contract.
    let result = unsafe { libc::ioctl(parent.as_raw_fd(), libc::FS_IOC_GETFLAGS, &mut flags) };
    flags_spelling(parent, name, if result == 0 { Some(flags) } else { None })
}

#[cfg(any(target_os = "linux", test))]
fn flags_spelling(parent: &File, name: &OsStr, flags: Option<libc::c_int>) -> io::Result<bool> {
    if flags.is_some_and(|flags| flags & 0x4000_0000 == 0) {
        return Ok(true);
    }
    listed_spelling(parent, name)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn exact_spelling(parent: &File, name: &OsStr, _: &File) -> io::Result<bool> {
    listed_spelling(parent, name)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_casefold_flags_fall_back_to_exact_directory_names() {
        let dir = crate::test_support::TestDir::new();
        std::fs::write(dir.0.join("Actual"), b"bytes").unwrap();
        let parent = File::open(&dir.0).unwrap();
        for flags in [None, Some(0x4000_0000)] {
            assert!(flags_spelling(&parent, "Actual".as_ref(), flags).unwrap());
            assert!(!flags_spelling(&parent, "actual".as_ref(), flags).unwrap());
        }
    }
}
