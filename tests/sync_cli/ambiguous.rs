use super::*;
use std::io::Read;

#[tokio::test]
async fn live_cli_lost_resolution_response_does_not_retry_a_committed_restore() {
    let data = TestDir::new();
    let tree = TestDir::new();
    api_fixture(&data, &tree, &["restore"]).await;
    let server = Server::start(&data, Some(&tree));
    scan_with_conflicts(&server, None);
    let page = server.cli("conflicts", &[], true);
    let item = &page["items"][0];
    let generation = server
        .request("GET", "/storage/v1/b/test-bucket/o/restore", None, b"")
        .json()["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let options = conflict_options(item, "restore", &generation, "lost-response");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let address = server.endpoint.strip_prefix("http://").unwrap().to_owned();
    let proxy = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        stream.read_line(&mut line).unwrap();
        assert_eq!(line, "POST /_internal/sync/resolve HTTP/1.1\r\n");
        let mut length = None;
        loop {
            line.clear();
            assert_ne!(stream.read_line(&mut line).unwrap(), 0);
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':')
                && key.eq_ignore_ascii_case("content-length")
            {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
        let mut body = vec![0; length.unwrap()];
        stream.read_exact(&mut body).unwrap();
        let reply =
            object_effects_support::request(&address, "POST", "/_internal/sync/resolve", "", &body);
        assert_eq!(
            reply.status,
            202,
            "{}",
            String::from_utf8_lossy(&reply.body)
        );
        let accepted: Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(accepted["completed"], true);
        drop(stream);
        (listener, accepted)
    });
    let output = common::server_command()
        .args([
            "sync",
            "resolve",
            "--endpoint",
            &endpoint,
            "--timeout",
            "1s",
        ])
        .args(&options)
        .output()
        .unwrap();
    let (listener, accepted) = proxy.join().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    listener.set_nonblocking(true).unwrap();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert!(
        server.cli("conflicts", &[], true)["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let winner = server
        .request("GET", "/storage/v1/b/test-bucket/o/restore", None, b"")
        .json();
    assert_eq!(
        winner["generation"],
        accepted["result_generation"].to_string()
    );
    assert_eq!(server.cli("resolve", &options, true), accepted);
    assert_eq!(
        server
            .request("GET", "/storage/v1/b/test-bucket/o/restore", None, b"")
            .json(),
        winner
    );
    assert_eq!(
        server
            .request(
                "GET",
                "/storage/v1/b/test-bucket/o/restore?alt=media",
                None,
                b""
            )
            .body,
        b"external\0\xff"
    );
}
