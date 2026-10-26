#![allow(dead_code)]

//! Test-only local input fixtures shared by unit and wire tests.

use std::path::{Path, PathBuf};

pub struct TestDir(pub PathBuf);

impl TestDir {
    pub fn new() -> Self {
        Self::new_in(&std::env::temp_dir())
    }

    pub fn new_in(parent: &Path) -> Self {
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
pub fn live_len(path: &Path) -> u64 {
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

impl Drop for TestDir {
    fn drop(&mut self) {
        let result = std::fs::remove_dir_all(&self.0);
        #[cfg(windows)]
        let result = retry_while_open(&self.0, result);
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
pub fn fifo(path: &Path) {
    assert!(
        std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}
