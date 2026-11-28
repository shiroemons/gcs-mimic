//! The calls only Windows makes: opening a directory as a handle, the file identity from its
//! handle, and a move that reaches storage before it returns.

use super::{FileIdentity, FsError};
use std::{fs::File, io, os::windows::io::AsRawHandle, path::Path};

/// The Windows [`super::open_directory`]: a directory opens as a handle only with
/// `FILE_FLAG_BACKUP_SEMANTICS`.
pub(super) fn open_directory(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS);
    options.open(path)
}

/// The Windows [`super::stat_identity`]: the volume serial number and file index stand for the
/// device and inode.
#[expect(
    unsafe_code,
    reason = "the volume serial and file index come only from GetFileInformationByHandle"
)]
pub(super) fn stat_identity(file: &File) -> Result<FileIdentity, FsError> {
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: the live file handle and writable output allocation are valid for this call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: a successful call initialized every field of the output structure.
    let information = unsafe { information.assume_init() };
    let modified = (u64::from(information.ftLastWriteTime.dwHighDateTime) << 32)
        | u64::from(information.ftLastWriteTime.dwLowDateTime);
    Ok(FileIdentity {
        device: u64::from(information.dwVolumeSerialNumber),
        inode: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        size: (u64::from(information.nFileSizeHigh) << 32) | u64::from(information.nFileSizeLow),
        modified_ns: i128::from(modified) * 100 - 11_644_473_600_000_000_000,
        changed_ns: change_time(file)?,
    })
}

#[expect(
    unsafe_code,
    reason = "the change time comes only from GetFileInformationByHandleEx"
)]
fn change_time(file: &File) -> Result<i128, FsError> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
    };
    const BASIC_INFO_SIZE: u32 = {
        let size = std::mem::size_of::<FILE_BASIC_INFO>();
        assert!(size <= u32::MAX as usize);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the const assertion bounds the size to a DWORD"
        )]
        {
            size as u32
        }
    };
    let mut basic = std::mem::MaybeUninit::<FILE_BASIC_INFO>::uninit();
    // SAFETY: the output buffer matches FileBasicInfo and the file handle remains live.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            basic.as_mut_ptr().cast(),
            BASIC_INFO_SIZE,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: successful FileBasicInfo initialized the structure.
    Ok(i128::from(unsafe { basic.assume_init() }.ChangeTime) * 100 - 11_644_473_600_000_000_000)
}

#[expect(
    unsafe_code,
    reason = "MOVEFILE_WRITE_THROUGH is reachable only through MoveFileExW"
)]
pub(crate) fn move_write_through(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let wide = |path: &Path| -> io::Result<Vec<u16>> {
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
