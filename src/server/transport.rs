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

/// Silence before the first probe, the wait after each probe, and how many go unanswered before
/// the peer counts as dead. An idle connection to a vanished peer so ends after
/// 30 + 4 * 15 = 90 seconds on every platform, where the operating system defaults (Linux 9,
/// macOS 8, Windows 10) would differ. Data still waiting for an acknowledgement falls under the
/// kernel's retransmission limit instead. The kernel answers probes for a process that is merely
/// paused or busy, so a false positive takes a real network loss of that length.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEPALIVE_PROBES: u32 = 4;

/// Application timeouts can all be disabled, and a peer that sleeps or loses its network sends
/// no FIN. Without probes such a connection would keep its slot and any session claim forever.
/// Go's `net/http` listener enables keep-alive the same way.
fn enable_keepalive(stream: &TcpStream) {
    let probes = socket2::TcpKeepalive::new()
        .with_time(KEEPALIVE_IDLE)
        .with_interval(KEEPALIVE_INTERVAL)
        .with_retries(KEEPALIVE_PROBES);
    if let Err(error) = socket2::SockRef::from(stream).set_tcp_keepalive(&probes) {
        // A peer that already reset fails here too. Any other connection only loses dead peer
        // detection, so it still serves.
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Debug,
            "tcp_keepalive_failed",
            serde_json::json!({"message":"could not enable TCP keep-alive on an accepted connection; a dead peer stays connected until a configured timeout","cause":error.kind().to_string()}),
        );
    }
}

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
            Self::Tcp(listener) => listener.poll_accept(cx).map_ok(|(stream, _)| {
                enable_keepalive(&stream);
                Stream::Tcp(stream)
            }),
            #[cfg(unix)]
            Self::Unix(listener) => listener.poll_accept(cx).map_ok(Stream::Unix),
        }
    }
}
