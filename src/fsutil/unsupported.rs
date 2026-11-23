use super::DirectoryOpen;
pub(crate) fn child_directory(_: &File, _: &OsStr, _: &super::Lookup) -> io::Result<super::Child> {
    Err(io::ErrorKind::Unsupported.into())
}
pub(crate) fn child_regular(
    _: &File,
    _: &OsStr,
    _: &super::Lookup,
) -> io::Result<super::RegularChild> {
    Err(io::ErrorKind::Unsupported.into())
}
pub(crate) fn regular_unconfirmed(
    _: &File,
    _: &OsStr,
    _: &super::Lookup,
) -> io::Result<super::Opened<File>> {
    Err(io::ErrorKind::Unsupported.into())
}
pub(crate) fn directory_unconfirmed(
    _: &File,
    _: &OsStr,
    _: &super::Lookup,
) -> io::Result<super::Opened<File>> {
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

pub(crate) fn directory_names_within(
    directory: &File,
    after: Option<&OsStr>,
    limit: usize,
    _: usize,
) -> io::Result<Vec<OsString>> {
    directory_names_after(directory, after, limit)
}

pub(crate) fn each_name(
    _: &File,
    _: impl FnMut(&OsStr) -> io::Result<std::ops::ControlFlow<()>>,
) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub(crate) fn uncached_identity(_: &File) -> io::Result<super::FileIdentity> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn directory_writable(_: &File) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

#[expect(dead_code, reason = "the seed watcher reuses this")]
pub(crate) fn name_max(_: &File) -> io::Result<Option<usize>> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn open_directory_nofollow(_: &Path) -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn open_regular_at(_: &File, _: &OsStr) -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}

pub(crate) fn directory_at(_: &File, _: &OsStr, _: DirectoryOpen) -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}
