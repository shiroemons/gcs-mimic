use super::*;

#[test]
fn endpoints_reject_production_metadata_and_ambiguous_origins() {
    for value in [
        "https://pubsub.googleapis.com:443",
        "pubsub.googleapis.com:80",
        "169.254.169.254:80",
        "metadata.google.internal:80",
        "8.8.8.8:8085",
        "[::ffff:8.8.8.8]:8085",
        "localhost",
        "localhost:0",
        "http://user@localhost:8085",
        "http://localhost:8085/path",
        "http://localhost:8085/?x=y",
    ] {
        assert!(Endpoint::parse(value).is_err(), "{value}");
    }
    for value in [
        "localhost:8085",
        "http://127.0.0.1:8085",
        "[::1]:8085",
        "pubsub:8085",
        "192.168.1.3:8085",
    ] {
        assert!(Endpoint::parse(value).is_ok(), "{value}");
    }
}

#[tokio::test]
async fn dns_resolution_rejects_alternate_public_and_metadata_ipv4_notation() {
    for host in [
        "134744072:8085",
        "0x08080808:8085",
        "2852039166:8085",
        "[::ffff:169.254.169.254]:8085",
    ] {
        if let Ok(endpoint) = Endpoint::parse(host) {
            assert!(endpoint.address().await.is_err(), "{host}");
        }
    }
}

#[tokio::test]
async fn topic_names_are_encoded_before_entering_the_pubsub_request_path() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Endpoint::parse(&listener.local_addr().unwrap().to_string()).unwrap();
    let server = crate::task::spawn_supervised("encoded-topic-test", async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut reader = tokio::io::BufReader::new(socket);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with(
            "POST /v1/projects/example.com%3Alocal/topics/a%25b%2Bc:publish HTTP/1.1"
        ));
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        assert!((1..=1024).contains(&length));
        reader.read_exact(&mut vec![0; length]).await.unwrap();
        let mut socket = reader.into_inner();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 23\r\n\r\n{\"messageIds\":[\"done\"]}")
            .await
            .unwrap();
    });
    endpoint
        .publish(
            "//pubsub.googleapis.com/projects/example.com:local/topics/a%b+c",
            "{\"messages\":[]}".into(),
        )
        .await
        .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn publish_uses_checked_local_rest_origin_and_requires_one_acknowledgement() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Endpoint::parse(&listener.local_addr().unwrap().to_string()).unwrap();
    for (status, response, succeeds) in [
        (200, r#"{"messageIds":["first"]}"#.to_owned(), true),
        (200, r#"{"messageIds":[]}"#.to_owned(), false),
        (200, r#"{"messageIds":["one","two"]}"#.to_owned(), false),
        (200, "invalid JSON".to_owned(), false),
        (
            200,
            format!("{{\"messageIds\":[\"{}\"]}}", "x".repeat(8192)),
            false,
        ),
        (307, "{}".to_owned(), false),
        (503, "{}".to_owned(), false),
    ] {
        let server = crate::task::spawn_supervised("pubsub-test", async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 8192];
            let read = socket.read(&mut buffer).await.unwrap();
            let request = std::str::from_utf8(&buffer[..read]).unwrap();
            assert!(request.starts_with("POST /v1/projects/p/topics/topic:publish HTTP/1.1\r\n"));
            assert!(!request.to_ascii_lowercase().contains("authorization:"));
            socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
            listener
        });
        assert_eq!(
            endpoint
                .publish(
                    "//pubsub.googleapis.com/projects/p/topics/topic",
                    r#"{"messages":[{"attributes":{"eventType":"OBJECT_FINALIZE"}}]}"#.into()
                )
                .await
                .is_ok(),
            succeeds
        );
        // Keep the same explicit origin across all attempts.
        listener = server.await.unwrap();
    }
}
