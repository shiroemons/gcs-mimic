use std::{
    io,
    task::{Context, Poll},
    time::Duration,
};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AcceptFailure {
    Connection,
    Exhausted,
    Fatal,
}

pub(super) fn classify(error: &io::Error) -> AcceptFailure {
    use io::ErrorKind::*;
    if matches!(
        error.kind(),
        ConnectionAborted | ConnectionReset | ConnectionRefused | Interrupted | TimedOut
    ) {
        return AcceptFailure::Connection;
    }
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error() {
        if matches!(
            code,
            libc::EPROTO
                | libc::ENOPROTOOPT
                | libc::ENETDOWN
                | libc::EHOSTDOWN
                | libc::EHOSTUNREACH
                | libc::EOPNOTSUPP
                | libc::ENETUNREACH
                | libc::EPERM
        ) {
            return AcceptFailure::Connection;
        }
        if matches!(
            code,
            libc::EBADF | libc::ENOTSOCK | libc::EINVAL | libc::EFAULT
        ) {
            return AcceptFailure::Fatal;
        }
        #[cfg(target_os = "linux")]
        if code == libc::ENONET {
            return AcceptFailure::Connection;
        }
    }
    if error.kind() == InvalidInput {
        AcceptFailure::Fatal
    } else {
        AcceptFailure::Exhausted
    }
}

pub(super) struct AcceptBackoff {
    delay: Duration,
    failures: u64,
}

impl Default for AcceptBackoff {
    fn default() -> Self {
        Self {
            delay: Duration::from_millis(10),
            failures: 0,
        }
    }
}

impl AcceptBackoff {
    pub(super) fn exhausted(&mut self, error: &io::Error) -> Duration {
        self.failures = self.failures.saturating_add(1);
        let delay = self.delay;
        self.delay = (delay * 2).min(Duration::from_secs(1));
        if self.failures == 1 {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Warn,
                "accept_failed",
                serde_json::json!({"kind": error.kind().to_string(), "os_error": error.raw_os_error(), "retry_ms": delay.as_millis()}),
            );
        }
        delay
    }

    pub(super) fn accepted(&mut self) {
        if self.failures > 0 {
            crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Info,
                "accept_recovered",
                serde_json::json!({"failures": self.failures}),
            );
            *self = Self::default();
        }
    }
}

#[cfg(test)]
mod tests;

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
