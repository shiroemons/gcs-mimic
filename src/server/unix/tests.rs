use super::*;
use crate::test_support::TestDir;
use std::time::Duration;

fn private_directory() -> TestDir {
    let directory = TestDir::new();
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o700)).unwrap();
    directory
}

#[tokio::test]
async fn private_socket_permissions_and_explicit_close_remove_only_owned_entry() {
    let directory = private_directory();
    let path = directory.0.join("s");
    let listener = Listener::bind(&path).await.unwrap();
    assert_eq!(
        fs::symlink_metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let stream = UnixStream::connect(&path).await.unwrap();
    drop(stream);
    listener.close().await.unwrap();
    assert!(!path.exists());

    let listener = Listener::bind(&path).await.unwrap();
    let moved = directory.0.join("original");
    fs::rename(&path, &moved).unwrap();
    fs::write(&path, b"foreign replacement").unwrap();
    listener.close().await.unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"foreign replacement");
}

#[tokio::test]
async fn existing_files_sockets_symlinks_and_nonprivate_parents_fail_without_unlink() {
    let directory = private_directory();
    let path = directory.0.join("s");
    fs::write(&path, b"foreign").unwrap();
    assert!(Listener::bind(&path).await.is_err());
    assert_eq!(fs::read(&path).unwrap(), b"foreign");
    fs::remove_file(&path).unwrap();
    let foreign = StdListener::bind(&path).unwrap();
    let inode = fs::symlink_metadata(&path).unwrap().ino();
    assert!(Listener::bind(&path).await.is_err());
    assert_eq!(fs::symlink_metadata(&path).unwrap().ino(), inode);
    drop(foreign);
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink("missing", &path).unwrap();
    assert!(Listener::bind(&path).await.is_err());
    assert!(!directory.0.join("missing").exists());
    assert!(
        fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(&path).unwrap();
    fs::set_permissions(&directory.0, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Listener::bind(&path).await.is_err());
    assert!(!path.exists());
}

#[test]
fn canceled_bind_reclaims_its_socket_even_during_runtime_shutdown() {
    let directory = private_directory();
    let path = directory.0.join("s");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let (release, blocked) = std::sync::mpsc::channel();
    runtime.block_on(async {
        tokio::task::spawn_blocking(move || blocked.recv().unwrap());
        assert!(
            tokio::time::timeout(Duration::from_millis(20), Listener::bind(&path))
                .await
                .is_err()
        );
    });
    release.send(()).unwrap();
    drop(runtime);
    assert!(!path.exists(), "canceled bind leaked a socket pathname");
}
