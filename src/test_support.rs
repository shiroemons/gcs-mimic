//! Test-only local input fixtures shared by unit and wire tests.

use rusqlite::{Connection, types::Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

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

#[derive(Debug)]
pub(crate) struct UploadCleanup {
    pub active: usize,
    pub staging: usize,
    pub blobs: usize,
    pub objects: usize,
    pub pending: usize,
    pub derived: usize,
}

pub(crate) fn upload_files(dir: &Path, directory: &str) -> usize {
    fs::read_dir(dir.join(directory))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            if path.is_dir() {
                fs::read_dir(path).unwrap().count()
            } else {
                1
            }
        })
        .sum()
}

impl UploadCleanup {
    pub(crate) fn observe(dir: &Path, active: usize) -> Self {
        let connection = Connection::open(dir.join("catalog.sqlite3")).unwrap();
        let count = |sql| connection.query_row(sql, [], |row| row.get(0)).unwrap();
        Self {
            active,
            staging: upload_files(dir, "staging"),
            blobs: upload_files(dir, "blobs"),
            objects: count("SELECT count(*) FROM objects"),
            pending: count("SELECT count(*) FROM pending_blob_deletions"),
            derived: count("SELECT count(*) FROM derived_transfers"),
        }
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.active == 0
            && self.staging == 0
            && self.blobs == self.objects
            && self.pending == 0
            && self.derived == 0
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct StoredObjects {
    objects: Vec<Vec<Value>>,
    generation: i64,
    bodies: BTreeMap<String, Vec<u8>>,
}

impl StoredObjects {
    pub(crate) fn read(dir: &Path) -> Self {
        let connection = Connection::open(dir.join("catalog.sqlite3")).unwrap();
        let mut statement = connection
            .prepare("SELECT * FROM objects ORDER BY bucket,name,generation")
            .unwrap();
        let columns = statement.column_count();
        let objects = statement
            .query_map([], |row| {
                (0..columns).map(|column| row.get(column)).collect()
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let mut statement = connection.prepare("SELECT blob_id FROM objects").unwrap();
        let bodies = statement
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|id| {
                let id = id.unwrap();
                let bytes = fs::read(dir.join("blobs").join(&id[..2]).join(&id)).unwrap();
                (id, bytes)
            })
            .collect();
        Self {
            objects,
            generation: connection
                .query_row("SELECT last_generation FROM object_generation", [], |row| {
                    row.get(0)
                })
                .unwrap(),
            bodies,
        }
    }
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
#[expect(
    unsafe_code,
    reason = "F_GETPATH has no safe descriptor-relative std equivalent"
)]
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
