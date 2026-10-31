//! Private local HTTP transport. Advertised HTTP origins never identify this socket.

use std::{
    fs, io,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::UnixListener as StdListener,
    },
    path::{Path, PathBuf},
    task::{Context, Poll},
};
use tokio::{
    net::{UnixListener, UnixStream},
    runtime::Handle,
};

#[cfg(test)]
mod tests;

/// The binary startup adapter needs this library type to retain pathname ownership.
pub struct Listener {
    socket: UnixListener,
    owner: Owner,
}

impl Listener {
    pub async fn bind(path: &Path) -> io::Result<Self> {
        let path = path.to_owned();
        let runtime = Handle::current();
        // There is one startup job, and no await between bind and establishing
        // pathname ownership. A canceled waiter drops the returned owner in the pool.
        let (send, receive) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let result = crate::task::catch_job("unix_socket_bind", || bind(path, runtime))
                .map_err(io::Error::other)
                .and_then(|result| result);
            if let Err(Ok((socket, mut owner))) = send.send(result) {
                drop(socket);
                // The runtime may already be stopping. Stay in this worker
                // rather than submitting another job after blocking admission closes.
                if let Some(path) = owner.path.take() {
                    let _ = cleanup(path, owner.identity);
                }
            }
        });
        let (socket, owner) = receive.await.map_err(io::Error::other)??;
        Ok(Self {
            socket: UnixListener::from_std(socket)?,
            owner,
        })
    }

    pub(crate) fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<io::Result<UnixStream>> {
        self.socket.poll_accept(cx).map_ok(|(stream, _)| stream)
    }

    pub async fn close(self) -> io::Result<()> {
        let Self { socket, mut owner } = self;
        drop(socket);
        if let Some(path) = owner.path.take() {
            cleanup_job(&owner.runtime, path, owner.identity)
                .await
                .map_err(io::Error::other)??;
        }
        Ok(())
    }
}

fn bind(path: PathBuf, runtime: Handle) -> io::Result<(StdListener, Owner)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "unix_socket requires a private parent directory",
            )
        })?;
    let directory = fs::symlink_metadata(parent)?;
    if !directory.file_type().is_dir() || directory.permissions().mode() & 0o777 != 0o700 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unix_socket parent must be an existing private 0700 directory, not a symlink",
        ));
    }
    // macOS bind can follow a dangling symlink; reject every existing entry first.
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "unix_socket entry already exists",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let socket = StdListener::bind(&path)?;
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unix_socket pathname changed during bind",
        ));
    }
    let owner = Owner {
        path: Some(path.clone()),
        identity: (metadata.dev(), metadata.ino()),
        runtime,
    };
    if directory.uid() != metadata.uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unix_socket parent must be owned by the server user",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    socket.set_nonblocking(true)?;
    Ok((socket, owner))
}

struct Owner {
    path: Option<PathBuf>,
    identity: (u64, u64),
    runtime: Handle,
}

fn cleanup_job(
    runtime: &Handle,
    path: PathBuf,
    identity: (u64, u64),
) -> tokio::task::JoinHandle<io::Result<()>> {
    runtime.spawn_blocking(move || cleanup(path, identity))
}

fn cleanup(path: PathBuf, identity: (u64, u64)) -> io::Result<()> {
    let result = crate::task::catch_job("unix_socket_cleanup", || {
        match fs::symlink_metadata(&path) {
            Ok(metadata)
                if metadata.file_type().is_socket()
                    && (metadata.dev(), metadata.ino()) == identity =>
            {
                fs::remove_file(path)
            }
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    })
    .map_err(io::Error::other)
    .and_then(|result| result);
    if let Err(error) = &result {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Error,
            "unix_socket_cleanup_failed",
            serde_json::json!({"message":"Cannot remove the owned Unix socket", "kind":format!("{:?}",error.kind())}),
        );
    }
    result
}

impl Drop for Owner {
    fn drop(&mut self) {
        // Only canceled/failed startup uses this fallback. The captured runtime
        // waits for its bounded blocking jobs before destruction; normal close awaits it.
        if let Some(path) = self.path.take() {
            drop(cleanup_job(&self.runtime, path, self.identity));
        }
    }
}
