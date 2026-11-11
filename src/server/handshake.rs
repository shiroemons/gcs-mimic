//! Selects the protocol an accepted connection speaks before hyper reads from it.

use crate::tls::ListenerSecurity;
use tokio::{net::TcpStream, sync::watch, time::Instant};
use tokio_rustls::TlsAcceptor;

/// The content type of a TLS handshake record, which every TLS client sends first
/// (RFC 8446 section 5.1). HTTP/1 request lines and the HTTP/2 preface start with a letter.
const TLS_HANDSHAKE_RECORD: u8 = 0x16;

/// How a connection's bytes reach hyper.
pub(super) enum Handshake {
    Plain,
    Tls(TlsAcceptor),
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
        let config = crate::config::Config {
            scheme: crate::config::Scheme::Auto,
            ..Default::default()
        };
        crate::tls::load_server(&config).unwrap()
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(30)
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
