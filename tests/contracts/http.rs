use crate::common::http::read;

#[test]
fn framing_preserves_duplicate_headers_interim_responses_and_chunk_extensions() {
    let raw = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nVary: Origin\r\nVary: X-Origin\r\n\r\n3;ext=yes\r\none\r\n2\r\n!!\r\n0\r\nTrailer: yes\r\n\r\n";
    let reply = read(&mut &raw[..], false).unwrap();
    assert_eq!(reply.status, 200);
    assert_eq!(reply.header("vary").as_deref(), Some("Origin, X-Origin"));
    assert_eq!(reply.header_last("vary"), Some("X-Origin"));
    assert_eq!(reply.body, b"one!!");
    assert_eq!(reply.raw, raw);
}

#[test]
fn framing_rejects_short_bodies_and_invalid_chunk_boundaries() {
    for raw in [
        &b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\none"[..],
        &b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\none!!0\r\n\r\n"[..],
    ] {
        assert!(read(&mut &raw[..], false).is_err());
    }
    let mut raw = &b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\noops"[..];
    assert!(read(&mut raw, true).unwrap().body.is_empty());
    assert_eq!(raw, b"oops");
}

#[test]
fn interruption_stays_broken_after_repeated_writes() {
    use std::io::Write;
    for remaining in [0, 1, 2] {
        let mut writer = crate::common::interrupt::Interrupt { remaining };
        assert_eq!(writer.write(b"prefix").unwrap(), 6);
        if remaining == 2 {
            assert_eq!(writer.write(b"first\n").unwrap(), 6);
        }
        for _ in 0..2 {
            assert_eq!(
                writer.write(b"line\n").unwrap_err().kind(),
                std::io::ErrorKind::BrokenPipe
            );
        }
        writer.flush().unwrap();
    }
}

#[test]
fn reply_helpers_keep_success_and_error_validation_distinct() {
    use crate::common::http::Reply;
    let reply = Reply {
        status: 400,
        headers: std::collections::BTreeMap::new(),
        body: br#"{"error":{"errors":[{"reason":"invalid"}]}}"#.to_vec(),
    };
    assert!(reply.json().is_object());
    reply.json_reason(400, "invalid");
    assert!(std::panic::catch_unwind(|| reply.json_ok()).is_err());
    assert!(std::panic::catch_unwind(|| reply.json_error(400, "invalid")).is_err());
}

#[test]
fn duplicate_header_policies_keep_last_joined_and_unique_checks() {
    let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nVary: Origin\r\nVary: X-Origin\r\n\r\n";
    assert_eq!(
        read(&mut &raw[..], false).unwrap().into_last().headers["vary"],
        "X-Origin"
    );
    assert_eq!(
        read(&mut &raw[..], false).unwrap().into_joined().headers["vary"],
        "Origin, X-Origin"
    );
    assert_eq!(
        read(&mut &raw[..], false)
            .unwrap()
            .into_unique_joined(&["vary"])
            .headers["vary"],
        "Origin, X-Origin"
    );
    assert!(
        std::panic::catch_unwind(|| read(&mut &raw[..], false).unwrap().into_unique_joined(&[]))
            .is_err()
    );
}

#[test]
fn head_reply_rejects_stray_bytes_after_the_headers() {
    use std::{
        io::Write,
        net::{TcpListener, TcpStream},
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let peer = std::thread::spawn(move || {
        listener
            .accept()
            .unwrap()
            .0
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\noops")
            .unwrap();
    });
    assert!(std::panic::catch_unwind(|| crate::common::http::receive(stream, true)).is_err());
    peer.join().unwrap();
}
