#![allow(dead_code)]

//! Test-only local input fixtures shared by unit and wire tests.

use std::path::{Path, PathBuf};

pub(crate) struct TestDir(pub PathBuf);

impl TestDir {
    pub(crate) fn new() -> Self {
        Self::new_in(&std::env::temp_dir())
    }

    pub(crate) fn new_in(parent: &Path) -> Self {
        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        // Keep nested Unix socket fixtures within the platform path limit.
        let path = parent.join(format!("gcs-{:016x}", u64::from_ne_bytes(random)));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

/// Length of a file that another handle may still be writing. Windows directory
/// listings report a stale size for open files, so stat the path itself; a file
/// removed since the listing counts as empty.
pub(crate) fn live_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |metadata| metadata.len())
}

/// Blocking catalog threads can close their files just after a test returns, and
/// Windows refuses to delete open files (os error 32) until then. A handle that
/// stays open past the retries still fails cleanup.
#[cfg(windows)]
fn retry_while_open(path: &Path, mut result: std::io::Result<()>) -> std::io::Result<()> {
    for _ in 0..100 {
        if !matches!(&result, Err(error) if error.raw_os_error() == Some(32)) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        result = std::fs::remove_dir_all(path);
    }
    result
}

/// Unix removes directories whose files are still open, so a test that keeps a
/// catalog connection past its fixture passes locally and fails only on Windows,
/// where SQLite opens files without delete sharing. Waiting as long as the
/// Windows retry and then reporting every remaining handle surfaces such leaks
/// without a Windows runner; it also flags handles Windows would tolerate.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn wait_until_closed(path: &Path) -> Vec<PathBuf> {
    let Ok(path) = path.canonicalize() else {
        return Vec::new();
    };
    let mut open = open_files_under(&path);
    for _ in 0..100 {
        if open.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        open = open_files_under(&path);
    }
    open
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_files_under(path: &Path) -> Vec<PathBuf> {
    let descriptors = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    let Ok(entries) = std::fs::read_dir(descriptors) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .filter_map(descriptor_path)
        .filter(|open| open.starts_with(path))
        .collect()
}

#[cfg(target_os = "linux")]
fn descriptor_path(fd: i32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{fd}")).ok()
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn descriptor_path(fd: i32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut buffer = [0u8; libc::MAXPATHLEN as usize];
    // SAFETY: F_GETPATH writes a terminated path of at most MAXPATHLEN bytes; a
    // descriptor closed since the listing only makes the call fail.
    if unsafe { libc::fcntl(fd, libc::F_GETPATH, buffer.as_mut_ptr()) } == -1 {
        return None;
    }
    let length = buffer.iter().position(|&byte| byte == 0)?;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(
        &buffer[..length],
    )))
}

impl Drop for TestDir {
    fn drop(&mut self) {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let open = wait_until_closed(&self.0);
        let result = std::fs::remove_dir_all(&self.0);
        #[cfg(windows)]
        let result = retry_while_open(&self.0, result);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let result = result.and_then(|()| {
            if open.is_empty() {
                Ok(())
            } else {
                Err(std::io::Error::other(format!(
                    "files still open after the test: {open:?}"
                )))
            }
        });
        if let Err(error) = result {
            if std::thread::panicking() {
                eprintln!("Cannot remove test input directory: {error}");
            } else {
                panic!("Cannot remove test input directory: {error}");
            }
        }
    }
}

#[cfg(unix)]
pub(crate) fn fifo(path: &Path) {
    assert!(
        std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}
