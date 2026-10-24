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

impl Drop for TestDir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
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
