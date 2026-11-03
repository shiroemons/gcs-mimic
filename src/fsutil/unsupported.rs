use super::{DirectoryOpen, validate_name};
pub(crate) fn child_directory(_: &File, _: &OsStr, _: DirectoryOpen) -> io::Result<super::Child> {
    Err(io::ErrorKind::Unsupported.into())
}
pub(crate) fn child_regular(_: &File, _: &OsStr) -> io::Result<super::RegularChild> {
    Err(io::ErrorKind::Unsupported.into())
}
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    path::Path,
};

pub fn directory_names_after(
    _: &File,
    _: Option<&OsStr>,
    limit: usize,
) -> io::Result<Vec<OsString>> {
    if !(1..=256).contains(&limit) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Err(io::ErrorKind::Unsupported.into())
}

pub fn directory_writable(_: &File) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub(crate) fn unlink_file_at(_: &File, name: &OsStr) -> io::Result<()> {
    validate_name(name)?;
    Err(io::ErrorKind::Unsupported.into())
}

pub fn open_directory_nofollow(_: &Path) -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn open_regular_at(_: &File, _: &OsStr) -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}

pub(crate) fn create_regular_at(_: &File, _: &OsStr) -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}

pub(crate) fn directory_at(_: &File, _: &OsStr, _: DirectoryOpen) -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}
