use crate::common::{self, TestDir};
use common::http::Client;
pub(crate) use common::http::TextReply as Reply;
use std::{io::Write, net::TcpStream, time::Duration};

common::server::declare_server!();

impl Server {
    pub(crate) fn start(dir: &TestDir, options: &[&str]) -> Self {
        let mut server = Self::launch(common::server::http_command(&dir.0).args(options));
        server.0 = server.0.graceful();
        server
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
            .request(method, path, headers, body)
            .into_text()
    }
    pub(crate) fn connect(&self) -> TcpStream {
        self.client().connect()
    }
    pub(crate) fn raw(&self, head: &str, body: &[u8]) -> Reply {
        self.client().raw(head, body).into_text()
    }
}
impl Reply {
    pub(crate) fn status(&self, expected: u16) {
        self.expect(expected);
        assert!(self.headers.contains("content-type:"));
    }
}

pub(crate) fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}
