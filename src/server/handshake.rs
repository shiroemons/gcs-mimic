//! Selects the protocol an accepted connection speaks and completes its TLS handshake before
//! hyper reads from it. The HTTP and gRPC listeners share this path, so detection, the
//! handshake deadline and shutdown behave the same on both.

use crate::{config::Config, tls::ListenerSecurity};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    sync::watch,
    time::Instant,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

/// The content type of a TLS handshake record, which every TLS client sends first
/// (RFC 8446 section 5.1). HTTP/1 request lines and the HTTP/2 preface start with a letter.
const TLS_HANDSHAKE_RECORD: u8 = 0x16;

/// How a connection's bytes reach hyper.
pub(super) enum Handshake {
    Plain,
    Tls(TlsAcceptor),
}

/// The protocol a listener serves, which names what its connections' failures mean.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ListenerProtocol {
    /// HTTP/1 over plain TCP or TLS.
    Http,
    /// gRPC over HTTP/2, in plaintext (h2c) or over TLS.
    Grpc,
}

impl ListenerProtocol {
    fn report_silence(self) {
        match self {
            Self::Http => super::warn_connection_timeout(
                "request headers were not received before header_read_timeout_ms",
            ),
            // The HTTP/2 driver never started, so there is no peer to send GOAWAY to.
            Self::Grpc => crate::diagnostics::emit(
                crate::diagnostics::LogLevel::Info,
                crate::diagnostics::Event::GrpcHandshakeTimeout,
                serde_json::json!({"message":"gRPC client sent nothing before header_read_timeout_ms; closing the connection; connect an h2c or TLS gRPC client to grpc_listen or raise header_read_timeout_ms, for example 60000"}),
            ),
        }
    }

    fn report_handshake_failure(self, cause: &std::io::Error) {
        let message = match self {
            Self::Http => "TLS handshake failed; check client protocol and trust configuration",
            Self::Grpc => {
                "gRPC TLS handshake failed; the client must trust the server's CA, name a host in its certificate and offer ALPN h2 or none"
            }
        };
        crate::diagnostics::emit(
            crate::diagnostics::LogLevel::Warn,
            crate::diagnostics::Event::TlsHandshakeFailed,
            serde_json::json!({"message":message,"cause":cause.to_string()}),
        );
    }
}

/// A connection's bytes once its handshake is done.
pub(super) enum Secured<S> {
    Plain(S),
    /// Decrypted bytes. Boxed because a TLS session is far larger than a socket.
    Tls(Box<TlsStream<S>>),
}

/// A connection ready for its protocol.
pub(super) struct Established<S> {
    pub(super) stream: Secured<S>,
    /// Where the protocol's first read limit counts from: accept for plaintext, so waiting for
    /// the first byte never extends it, and the end of the handshake for TLS, which gives the
    /// first request its own full limit.
    pub(super) since: Instant,
}

/// Completes `handshake` on a connection accepted `at`. A TLS handshake ends at
/// `tls_handshake_timeout_ms` counted from `at`, so detecting the protocol never extends it, and
/// at shutdown. It runs in the connection's task, which holds a connection slot until it ends.
pub(super) async fn establish<S: AsyncRead + AsyncWrite + Unpin>(
    stream: S,
    handshake: Handshake,
    at: Instant,
    config: &Config,
    stopping: &mut watch::Receiver<bool>,
    protocol: ListenerProtocol,
) -> Option<Established<S>> {
    let acceptor = match handshake {
        Handshake::Plain => {
            return Some(Established {
                stream: Secured::Plain(stream),
                since: at,
            });
        }
        Handshake::Tls(acceptor) => acceptor,
    };
    let handshake = tokio::time::timeout_at(
        config.tls_handshake_timeout().deadline_from(at),
        acceptor.accept(stream),
    );
    let stream = tokio::select! {
        biased;
        _ = stopping.changed() => return None,
        result = handshake => match result {
            Ok(Ok(stream)) => stream,
            Ok(Err(cause)) => {
                protocol.report_handshake_failure(&cause);
                return None;
            }
            Err(_elapsed) => {
                crate::diagnostics::emit(crate::diagnostics::LogLevel::Warn, crate::diagnostics::Event::TlsHandshakeTimeout, serde_json::json!({"message":"TLS handshake deadline exceeded"}));
                return None;
            }
        }
    };
    Some(Established {
        stream: Secured::Tls(Box::new(stream)),
        since: Instant::now(),
    })
}

/// Selects the protocol of a TCP connection accepted `at` and completes its handshake. The wait
/// for the first byte ends at `header_read_timeout_ms` counted from `at`, the limit a silent
/// plaintext client has.
pub(super) async fn establish_tcp(
    stream: TcpStream,
    security: ListenerSecurity,
    at: Instant,
    config: &Config,
    stopping: &mut watch::Receiver<bool>,
    protocol: ListenerProtocol,
) -> Option<Established<TcpStream>> {
    let first_byte_by = config.header_read_timeout().deadline_from(at);
    let handshake = match security.select(&stream, first_byte_by, stopping).await {
        Ok(handshake) => handshake,
        Err(Undetected::Silent) => {
            protocol.report_silence();
            return None;
        }
        // A probe that only opens a connection is not a client fault.
        Err(Undetected::Closed | Undetected::Stopping) => return None,
    };
    establish(stream, handshake, at, config, stopping, protocol).await
}

/// Why a connection ended before its first byte selected a protocol.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Undetected {
    /// The client closed or reset the connection without sending a byte, as a TCP probe does.
    Closed,
    /// Nothing arrived before the deadline.
    Silent,
    /// The server began to shut down.
    Stopping,
}

impl ListenerSecurity {
    /// The handshake for a TCP connection. Only detection waits, for the first byte until
    /// `first_byte_by`, without consuming it.
    pub(super) async fn select(
        self,
        stream: &TcpStream,
        first_byte_by: Instant,
        stopping: &mut watch::Receiver<bool>,
    ) -> Result<Handshake, Undetected> {
        match self {
            Self::Plain => Ok(Handshake::Plain),
            Self::Tls(acceptor) => Ok(Handshake::Tls(acceptor)),
            Self::Detect(acceptor) => {
                let first = first_byte(stream, first_byte_by, stopping).await?;
                Ok(if first == TLS_HANDSHAKE_RECORD {
                    Handshake::Tls(acceptor)
                } else {
                    Handshake::Plain
                })
            }
        }
    }

    /// The handshake for a Unix socket, which is never inspected: it serves plain HTTP under
    /// detection, because no TLS client reaches it.
    #[cfg(unix)]
    pub(super) fn for_unix_socket(self) -> Handshake {
        match self {
            Self::Plain | Self::Detect(_) => Handshake::Plain,
            Self::Tls(acceptor) => Handshake::Tls(acceptor),
        }
    }
}

async fn first_byte(
    stream: &TcpStream,
    by: Instant,
    stopping: &mut watch::Receiver<bool>,
) -> Result<u8, Undetected> {
    let mut first = [0; 1];
    let peeked = tokio::select! {
        biased;
        _ = stopping.changed() => return Err(Undetected::Stopping),
        peeked = tokio::time::timeout_at(by, stream.peek(&mut first)) => peeked,
    };
    match peeked {
        Ok(Ok(1..)) => Ok(first[0]),
        Ok(Ok(0) | Err(_)) => Err(Undetected::Closed),
        Err(_elapsed) => Err(Undetected::Silent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, server)
    }

    fn detecting() -> ListenerSecurity {
        let config = crate::config::ConfigDraft {
            scheme: crate::config::Scheme::Auto,
            ..Default::default()
        }
        .build();
        crate::tls::ServerSecurity::load(&config).unwrap().primary
    }

    /// The gRPC listener under the default `grpc_scheme=auto`, beside a plain HTTP listener.
    fn detecting_grpc() -> ListenerSecurity {
        let config = crate::config::ConfigDraft {
            scheme: crate::config::Scheme::Http,
            grpc_listen: Some("127.0.0.1:0".parse().unwrap()),
            ..Default::default()
        }
        .build();
        crate::tls::ServerSecurity::load(&config).unwrap().grpc
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    #[tokio::test]
    async fn detection_on_the_grpc_listener_selects_tls_for_a_handshake_record() {
        let (_stop, stopping) = watch::channel(false);
        for (first, tls) in [
            (&[0x16, 0x03, 0x01][..], true),
            (b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n", false),
        ] {
            let (mut client, server) = pair().await;
            client.write_all(first).await.unwrap();
            let handshake = detecting_grpc()
                .select(&server, far(), &mut stopping.clone())
                .await
                .unwrap();
            assert_eq!(matches!(handshake, Handshake::Tls(_)), tls);
        }
    }

    #[tokio::test]
    async fn a_tls_record_selects_tls_and_a_request_line_selects_plain_http() {
        let (_stop, stopping) = watch::channel(false);
        for (first, tls) in [
            (&[0x16, 0x03, 0x01][..], true),
            (b"GET / HTTP/1.1\r\n", false),
        ] {
            let (mut client, server) = pair().await;
            client.write_all(first).await.unwrap();
            let handshake = detecting()
                .select(&server, far(), &mut stopping.clone())
                .await
                .unwrap();
            assert_eq!(matches!(handshake, Handshake::Tls(_)), tls);
        }
    }

    #[tokio::test]
    async fn detection_leaves_the_first_byte_for_the_protocol() {
        let (_stop, stopping) = watch::channel(false);
        let (mut client, mut server) = pair().await;
        client.write_all(b"PRI * HTTP/2.0").await.unwrap();
        let handshake = detecting()
            .select(&server, far(), &mut stopping.clone())
            .await
            .unwrap();
        assert!(matches!(handshake, Handshake::Plain));
        let mut read = [0; 14];
        server.read_exact(&mut read).await.unwrap();
        assert_eq!(&read, b"PRI * HTTP/2.0");
    }

    #[tokio::test]
    async fn a_client_that_closes_first_is_closed_and_a_silent_one_times_out() {
        let (_stop, stopping) = watch::channel(false);
        let (client, server) = pair().await;
        drop(client);
        let closed = detecting()
            .select(&server, far(), &mut stopping.clone())
            .await;
        assert_eq!(closed.err(), Some(Undetected::Closed));

        let (_client, server) = pair().await;
        let started = Instant::now();
        let silent = detecting()
            .select(
                &server,
                started + Duration::from_millis(50),
                &mut stopping.clone(),
            )
            .await;
        assert_eq!(silent.err(), Some(Undetected::Silent));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn shutdown_ends_the_wait_for_the_first_byte() {
        let (stop, mut stopping) = watch::channel(false);
        let (_client, server) = pair().await;
        let detection = detecting().select(&server, far(), &mut stopping);
        let (selected, ()) = tokio::join!(detection, async {
            stop.send_replace(true);
        });
        assert_eq!(selected.err(), Some(Undetected::Stopping));
    }

    #[tokio::test]
    async fn fixed_modes_never_wait_for_the_first_byte() {
        let (_stop, stopping) = watch::channel(false);
        let (_client, server) = pair().await;
        let past = Instant::now();
        let plain = ListenerSecurity::Plain
            .select(&server, past, &mut stopping.clone())
            .await;
        assert!(matches!(plain, Ok(Handshake::Plain)));
        let ListenerSecurity::Detect(acceptor) = detecting() else {
            panic!("auto loads a detecting listener");
        };
        assert!(matches!(
            ListenerSecurity::Tls(acceptor)
                .select(&server, past, &mut stopping.clone())
                .await,
            Ok(Handshake::Tls(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_unix_socket_serves_plain_http_under_detection() {
        assert!(matches!(detecting().for_unix_socket(), Handshake::Plain));
        let ListenerSecurity::Detect(acceptor) = detecting() else {
            panic!("auto loads a detecting listener");
        };
        assert!(matches!(
            ListenerSecurity::Tls(acceptor).for_unix_socket(),
            Handshake::Tls(_)
        ));
    }
}
