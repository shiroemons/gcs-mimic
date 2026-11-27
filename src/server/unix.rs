//! Private local HTTP transport. Advertised HTTP origins never identify this socket.

use crate::{
    fsutil::NodeId,
    task::{BlockingFailed, JobPanicked},
};
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

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(crate) struct SocketError(#[from] SocketFailure);

#[derive(Debug, thiserror::Error)]
enum SocketFailure {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Panicked(#[from] JobPanicked),
    #[error(transparent)]
    Worker(#[from] BlockingFailed),
    #[error(transparent)]
    Closed(#[from] tokio::sync::oneshot::error::RecvError),
}

impl crate::diagnostics::RuntimeFailure for SocketError {
    fn kind(&self) -> io::ErrorKind {
        match &self.0 {
            SocketFailure::Io(error) => error.kind(),
            SocketFailure::Panicked(_) | SocketFailure::Worker(_) | SocketFailure::Closed(_) => {
                io::ErrorKind::Other
            }
        }
    }
}

impl Listener {
    pub(crate) async fn bind(path: &Path) -> Result<Self, SocketError> {
        let path = path.to_owned();
        let runtime = Handle::current();
        let bind_runtime = runtime.clone();
        // There is one startup job, and no await between bind and establishing
        // pathname ownership. A canceled waiter drops the returned owner in the pool.
        let (send, receive) = tokio::sync::oneshot::channel();
        crate::task::spawn_blocking_on(&runtime, "unix_socket_bind", move || {
            let result = crate::task::catch_job("unix_socket_bind", || {
                bind(path, bind_runtime).map_err(SocketFailure::from)
            })
            .map_err(SocketFailure::from)
            .flatten();
            if let Err(Ok((socket, mut owner))) = send.send(result) {
                drop(socket);
                // The runtime may already be stopping. Stay in this worker
                // rather than submitting another job after blocking admission closes.
                if let Some(path) = owner.path.take() {
                    let _ = cleanup(path, owner.identity);
                }
            }
        });
        let (socket, owner) = receive.await.map_err(SocketFailure::from)??;
        Ok(Self {
            socket: UnixListener::from_std(socket).map_err(SocketFailure::from)?,
            owner,
        })
    }

    pub(crate) fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<io::Result<UnixStream>> {
        self.socket.poll_accept(cx).map_ok(|(stream, _)| stream)
    }

    pub(crate) async fn close(self) -> Result<(), SocketError> {
        let Self { socket, mut owner } = self;
        drop(socket);
        if let Some(path) = owner.path.take() {
            let identity = owner.identity;
            crate::task::run_blocking_on(&owner.runtime, "unix_socket_cleanup", move || {
                cleanup(path, identity)
            })
            .await
            .map_err(SocketFailure::from)??;
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
        identity: node(&metadata),
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

#[must_use = "dropping the owner schedules cleanup of the bound Unix socket"]
struct Owner {
    path: Option<PathBuf>,
    /// The socket file this server bound; cleanup removes the path only while it names it.
    identity: NodeId,
    runtime: Handle,
}

fn node(metadata: &fs::Metadata) -> NodeId {
    NodeId {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

fn cleanup(path: PathBuf, identity: NodeId) -> Result<(), SocketError> {
    let result = crate::task::catch_job("unix_socket_cleanup", || {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_socket() && node(&metadata) == identity => {
                fs::remove_file(path)
            }
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    })
    .map_err(SocketFailure::from)
    .and_then(|result| result.map_err(SocketFailure::from))
    .map_err(SocketError);
    if let Err(error) = &result {
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Error,
            crate::diagnostics::Event::UnixSocketCleanupFailed,
            serde_json::json!({"message":"Cannot remove the owned Unix socket", "kind":format!("{:?}",crate::diagnostics::RuntimeFailure::kind(error))}),
        );
    }
    result
}

impl Drop for Owner {
    fn drop(&mut self) {
        // Only canceled/failed startup uses this fallback. The captured runtime
        // waits for its bounded blocking jobs before destruction; normal close awaits it.
        if let Some(path) = self.path.take() {
            let identity = self.identity;
            drop(crate::task::spawn_blocking_on(
                &self.runtime,
                "unix_socket_cleanup",
                move || cleanup(path, identity),
            ));
        }
    }
}
