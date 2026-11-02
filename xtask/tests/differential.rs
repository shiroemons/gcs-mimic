use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Stub(Child);

impl Drop for Stub {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn pubsub_stub_survives_a_broken_connection_and_serves_the_next_lookup() {
    let mut stub = Stub(
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["differential", "pubsub-stub"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut endpoint = String::new();
    BufReader::new(stub.0.stdout.take().unwrap())
        .take(128)
        .read_line(&mut endpoint)
        .unwrap();
    let endpoint = endpoint.trim();
    drop(TcpStream::connect(endpoint).unwrap());
    let mut stream = TcpStream::connect(endpoint).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(b"GET /v1/projects/local-project/topics/gcs-mimic-diff-no-such-topic HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();
    let mut response = String::new();
    stream.take(8192).read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 404 Not Found\r\n"),
        "{response}"
    );
    stub.0.kill().unwrap();
    stub.0.wait().unwrap();
    let mut errors = String::new();
    stub.0
        .stderr
        .take()
        .unwrap()
        .take(8192)
        .read_to_string(&mut errors)
        .unwrap();
    assert!(
        errors.contains("incomplete Pub/Sub lookup headers"),
        "{errors}"
    );
}
