use crate::common::{self, TestDir};
use common::http::Client;
pub(crate) use common::http::TextReply as Reply;
use std::{io::Write, net::TcpStream, time::Duration};

common::process::declare_server!(graceful);

impl Server {
    pub(crate) fn start(dir: &TestDir, options: &[&str]) -> Self {
        Self::launch(common::process::http_command(&dir.0).args(options)).graceful()
    }
    fn client(&self) -> Client<'_> {
        Client {
            address: &self.address,
            host: "localhost",
            headers: "",
            timeout: Duration::from_secs(10),
        }
    }
    pub(crate) fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Reply {
        self.client()
            .request_eof(method, path, headers, body)
            .into_text()
    }
    pub(crate) fn connect(&self) -> TcpStream {
        self.client().connect()
    }
    pub(crate) fn raw(&self, head: &str, body: &[u8]) -> Reply {
        self.client().raw_eof(head, body).into_text()
    }
}
impl Reply {
    pub(crate) fn status(&self, expected: u16) {
        self.expect(expected);
        assert!(self.headers.contains("content-type:"));
    }

    /// Asserts that this is the bare text page Cloud Storage's front end answers for a
    /// transport-compressed body that does not decode (recorded 2026-11-25).
    pub(crate) fn undecodable(&self) {
        self.expect(400);
        assert!(
            self.headers
                .contains("content-type: text/plain; charset=utf-8"),
            "{}",
            self.headers
        );
        assert_eq!(self.body, "Could not decompress request body.");
    }
}

pub(crate) fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}
