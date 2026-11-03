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
