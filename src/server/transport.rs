use std::{
    io,
    task::{Context, Poll},
};
use tokio::net::{TcpListener, TcpStream};

pub(super) enum Listener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(super::unix::Listener),
}

pub(super) enum Stream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
}

impl Listener {
    pub(super) async fn close(self) -> io::Result<()> {
        match self {
            Self::Tcp(_) => Ok(()),
            #[cfg(unix)]
            Self::Unix(listener) => listener.close().await,
        }
    }

    pub(super) fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<io::Result<Stream>> {
        match self {
            Self::Tcp(listener) => listener
                .poll_accept(cx)
                .map_ok(|(stream, _)| Stream::Tcp(stream)),
            #[cfg(unix)]
            Self::Unix(listener) => listener.poll_accept(cx).map_ok(Stream::Unix),
        }
    }
}
