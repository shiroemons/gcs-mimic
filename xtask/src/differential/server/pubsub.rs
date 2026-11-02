//! A loopback-only stand-in for the missing topic used by notification cases.

use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    time::Duration,
};

pub(super) struct Stub {
    child: Child,
    pub(super) endpoint: String,
}

impl Stub {
    pub(super) fn start() -> io::Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .args(["differential", "pubsub-stub"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("stub stdout missing"))?;
        let mut stub = Self {
            child,
            endpoint: String::new(),
        };
        BufReader::new(stdout)
            .take(128)
            .read_line(&mut stub.endpoint)?;
        stub.endpoint = stub.endpoint.trim().to_owned();
        let address: std::net::SocketAddr = stub.endpoint.parse().map_err(io::Error::other)?;
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err(io::Error::other("stub did not report a loopback endpoint"));
        }
        Ok(stub)
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(super) fn serve() -> io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    println!("{}", listener.local_addr()?);
    for connection in listener.incoming() {
        answer(connection?)?;
    }
    Ok(())
}

fn answer(mut stream: TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut request = String::new();
    let mut reader = BufReader::new(&stream).take(8192);
    reader.read_line(&mut request)?;
    let mut header = String::new();
    loop {
        if reader.read_line(&mut header)? == 0 {
            return Err(io::Error::other("incomplete Pub/Sub lookup headers"));
        }
        if header == "\r\n" {
            break;
        }
        header.clear();
    }
    let status = lookup_status(&request);
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

fn lookup_status(request: &str) -> &'static str {
    if request == "GET /v1/projects/local-project/topics/gcs-mimic-diff-no-such-topic HTTP/1.1\r\n"
    {
        "404 Not Found"
    } else {
        "400 Bad Request"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_declared_missing_topic_is_not_found() {
        assert_eq!(
            lookup_status(
                "GET /v1/projects/local-project/topics/gcs-mimic-diff-no-such-topic HTTP/1.1\r\n"
            ),
            "404 Not Found"
        );
        for request in [
            "POST /v1/projects/local-project/topics/gcs-mimic-diff-no-such-topic:publish HTTP/1.1\r\n",
            "GET /v1/projects/local-project/topics/another-topic HTTP/1.1\r\n",
            "GET / HTTP/1.1\r\n",
        ] {
            assert_eq!(lookup_status(request), "400 Bad Request");
        }
    }
}
