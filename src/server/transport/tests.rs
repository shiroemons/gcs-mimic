use super::*;

#[test]
fn accept_backoff_doubles_to_cap_and_resets() {
    let mut backoff = AcceptBackoff::default();
    let error = io::Error::other("exhausted");
    for expected in [10, 20, 40, 80, 160, 320, 640, 1000, 1000] {
        assert_eq!(backoff.exhausted(&error), Duration::from_millis(expected));
    }
    backoff.accepted();
    assert_eq!(backoff.failures, 0);
    assert_eq!(backoff.exhausted(&error), Duration::from_millis(10));
}

#[cfg(unix)]
#[tokio::test]
async fn accepted_tcp_connections_enable_keepalive() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _client = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let listener = Listener::Tcp(listener);
    let accepted = std::future::poll_fn(|cx| listener.poll_accept(cx))
        .await
        .unwrap();
    let Stream::Tcp(stream) = accepted else {
        panic!("a TCP listener accepted another stream type");
    };
    assert!(socket2::SockRef::from(&stream).keepalive().unwrap());
}

#[cfg(unix)]
#[test]
fn accept_failures_classify_connection_resource_and_listener_errors() {
    for code in [libc::EMFILE, libc::ENFILE, libc::ENOBUFS, libc::ENOMEM] {
        assert_eq!(
            classify(&io::Error::from_raw_os_error(code)),
            AcceptFailure::Exhausted
        );
    }
    for code in [libc::ECONNABORTED, libc::ECONNRESET, libc::EPROTO] {
        assert_eq!(
            classify(&io::Error::from_raw_os_error(code)),
            AcceptFailure::Connection
        );
    }
    for code in [libc::EBADF, libc::EINVAL, libc::ENOTSOCK] {
        assert_eq!(
            classify(&io::Error::from_raw_os_error(code)),
            AcceptFailure::Fatal
        );
    }
    assert_eq!(
        classify(&io::Error::other("unknown")),
        AcceptFailure::Exhausted
    );
}
