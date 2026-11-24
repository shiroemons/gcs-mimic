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

pub fn directory_writable(_: &File) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn open_directory_nofollow(_: &Path) -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}

pub fn open_regular_at(_: &File, _: &OsStr) -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}

pub(crate) fn directory_at(_: &File, _: &OsStr) -> io::Result<File> {
    Err(io::ErrorKind::Unsupported.into())
}
