#[cfg(unix)]
#[test]
fn fetch_failure_is_never_reported_as_current() {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};
    let directory =
        std::env::temp_dir().join(format!("gcs-mimic-fetch-failure-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let executable = directory.join("curl");
    fs::write(&executable, "#!/bin/sh\nexit 7\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["dependencies", "check"])
        .env("PATH", &directory)
        .output()
        .unwrap();
    fs::remove_file(executable).unwrap();
    fs::remove_dir(directory).unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("CHECK_FAILED: fetch failed"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("CURRENT"));
}
