use serde_json::Value;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpListener,
    time::Duration,
};

pub(crate) fn accept_local_publish(listener: &TcpListener) -> (String, Value) {
    listener.set_nonblocking(true).unwrap();
    let (mut socket, _) = super::input_fixtures::eventually_some("local Pub/Sub request", || {
        match listener.accept() {
            Ok(connection) => Some(connection),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => None,
            Err(error) => panic!("{error}"),
        }
    });
    socket.set_nonblocking(false).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(socket.try_clone().unwrap());
    let mut path = String::new();
    reader.read_line(&mut path).unwrap();
    let mut length = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(!line.is_empty(), "request ended before its headers");
        if line == "\r\n" {
            break;
        }
        assert!(!line.to_ascii_lowercase().starts_with("authorization:"));
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
    }
    if path.starts_with("GET ") {
        let body = r#"{"name":"projects/local/topics/explicit"}"#;
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .unwrap();
        return (path, Value::Null);
    }
    assert!((1..=131072).contains(&length));
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 23\r\n\r\n{\"messageIds\":[\"done\"]}")
        .unwrap();
    (
        path,
        serde_json::from_slice::<Value>(&body).unwrap()["messages"][0].clone(),
    )
}
