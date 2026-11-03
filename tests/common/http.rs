#![expect(
    dead_code,
    reason = "Integration binaries use different parts of the wire harness."
)]

use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpStream,
    time::Duration,
};

pub(crate) struct Reply<B = Vec<u8>, H = BTreeMap<String, String>> {
    pub(crate) status: u16,
    pub(crate) headers: H,
    pub(crate) body: B,
}

pub(crate) type TextReply = Reply<String, String>;

#[derive(Clone, Copy)]
pub(crate) enum Body {
    Expected,
    Head,
}
impl Body {
    pub(crate) fn for_method(method: &str) -> Self {
        if method == "HEAD" {
            Self::Head
        } else {
            Self::Expected
        }
    }
}
#[derive(Clone, Copy)]
enum Framing {
    Required,
    EofAllowed,
}

impl Reply {
    pub(crate) fn read_head(reader: &mut impl BufRead) -> Self {
        read_head(reader).unwrap().into_last()
    }
    pub(crate) fn read_body(&mut self, reader: &mut impl BufRead) {
        let headers: Vec<_> = self
            .headers
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        self.body = read_body(reader, self.status, &headers, Framing::Required)
            .unwrap_or_else(|error| panic!("{error}"))
            .0;
    }
    pub(crate) fn read_joined(stream: TcpStream) -> Self {
        receive(stream, Body::Expected).into_joined()
    }
    pub(crate) fn read_joined_head(stream: TcpStream, body: Body) -> Self {
        receive(stream, body).into_joined()
    }
}

impl<B: AsRef<[u8]>, H> Reply<B, H> {
    #[track_caller]
    pub(crate) fn expect(&self, status: u16) -> &Self {
        assert_eq!(
            self.status,
            status,
            "{}",
            String::from_utf8_lossy(self.body.as_ref())
        );
        self
    }

    #[track_caller]
    pub(crate) fn expect_with(&self, status: u16, context: impl std::fmt::Display) -> &Self {
        assert_eq!(
            self.status,
            status,
            "{context}: {}",
            String::from_utf8_lossy(self.body.as_ref())
        );
        self
    }

    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::from_slice(self.body.as_ref()).unwrap()
    }

    #[track_caller]
    pub(crate) fn json_ok(&self) -> serde_json::Value {
        self.expect(200).json()
    }

    #[track_caller]
    pub(crate) fn json_reason(&self, status: u16, reason: &str) {
        self.json_reason_value(status, reason);
    }

    #[track_caller]
    pub(crate) fn json_reason_value(&self, status: u16, reason: &str) -> serde_json::Value {
        let value = self.expect(status).json();
        assert_eq!(value["error"]["errors"][0]["reason"], reason, "{value}");
        value
    }
}

pub(crate) struct Response {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) head: String,
    pub(crate) body: Vec<u8>,
    pub(crate) raw: Vec<u8>,
}

impl Response {
    pub(crate) fn header_last(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn header(&self, name: &str) -> Option<String> {
        let values: Vec<_> = self
            .headers
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect();
        (!values.is_empty()).then(|| values.join(", "))
    }

    pub(crate) fn into_last(self) -> Reply {
        Reply {
            status: self.status,
            headers: self.headers.into_iter().collect(),
            body: self.body,
        }
    }

    pub(crate) fn into_unique_joined(self, repeatable: &[&str]) -> Reply {
        let mut seen = std::collections::BTreeSet::new();
        for (key, _) in &self.headers {
            assert!(
                seen.insert(key) || repeatable.contains(&key.as_str()),
                "duplicate header: {key}"
            );
        }
        self.into_joined()
    }

    pub(crate) fn into_joined(self) -> Reply {
        let mut headers = BTreeMap::<String, String>::new();
        for (key, value) in self.headers {
            headers
                .entry(key)
                .and_modify(|joined| {
                    joined.push_str(", ");
                    joined.push_str(&value);
                })
                .or_insert(value);
        }
        Reply {
            status: self.status,
            headers,
            body: self.body,
        }
    }

    pub(crate) fn into_text(self) -> TextReply {
        Reply {
            status: self.status,
            headers: self.head,
            body: String::from_utf8(self.body).unwrap(),
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn line(reader: &mut impl BufRead, raw: &mut Vec<u8>) -> io::Result<usize> {
    let start = raw.len();
    if reader.read_until(b'\n', raw)? == 0 {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    Ok(start)
}

pub(crate) fn read_head(reader: &mut impl BufRead) -> io::Result<Response> {
    let mut raw = Vec::new();
    let (status, text) = loop {
        let start = raw.len();
        while !raw[start..].ends_with(b"\r\n\r\n") {
            line(reader, &mut raw)?;
        }
        let text = std::str::from_utf8(&raw[start..raw.len() - 4])
            .map_err(|_| invalid("invalid HTTP headers"))?;
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or_else(|| invalid("missing HTTP status"))?;
        if !(100..200).contains(&status) {
            break (status, text.to_owned());
        }
    };
    let headers = text
        .lines()
        .skip(1)
        .map(|line| {
            let (key, value) = line
                .split_once(':')
                .ok_or_else(|| invalid("invalid HTTP header"))?;
            Ok((key.to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect::<io::Result<Vec<_>>>()?;
    Ok(Response {
        status,
        headers,
        head: text,
        body: Vec::new(),
        raw,
    })
}

fn read_body(
    reader: &mut impl BufRead,
    status: u16,
    headers: &[(&str, &str)],
    framing: Framing,
) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut body = Vec::new();
    let mut raw = Vec::new();
    let header = |name: &str| {
        headers
            .iter()
            .rev()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| *value)
    };
    if matches!(status, 204 | 304) {
        return Ok((body, raw));
    }
    if header("transfer-encoding").is_some_and(|value| value.contains("chunked")) {
        loop {
            let start = line(reader, &mut raw)?;
            let size =
                std::str::from_utf8(&raw[start..]).map_err(|_| invalid("invalid chunk size"))?;
            let size = usize::from_str_radix(size.trim().split(';').next().unwrap_or(""), 16)
                .map_err(|_| invalid("invalid chunk size"))?;
            if size == 0 {
                while line(reader, &mut raw).map(|start| raw[start..] != *b"\r\n")? {}
                break;
            }
            let start = raw.len();
            raw.resize(start + size + 2, 0);
            reader.read_exact(&mut raw[start..])?;
            if &raw[start + size..] != b"\r\n" {
                return Err(invalid("invalid chunk terminator"));
            }
            body.extend_from_slice(&raw[start..start + size]);
        }
    } else {
        match header("content-length") {
            Some(length) => {
                let length = length
                    .parse::<u64>()
                    .map_err(|_| invalid("invalid Content-Length"))?;
                let read = reader.by_ref().take(length).read_to_end(&mut body);
                if !matches!(read, Ok(size) if size as u64 == length) {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        format!(
                            "HTTP {} body ended after {} of {length} bytes ({read:?}); headers: {:?}",
                            status,
                            body.len(),
                            headers
                        ),
                    ));
                }
            }
            None if matches!(framing, Framing::EofAllowed) => {
                reader.read_to_end(&mut body)?;
            }
            None => {
                return Err(invalid(&format!(
                    "HTTP {} response has neither Content-Length nor chunked encoding; headers: {:?}",
                    status, headers
                )));
            }
        }
        raw.extend_from_slice(&body);
    }
    Ok((body, raw))
}

pub(crate) fn read(reader: &mut impl BufRead, body: Body) -> io::Result<Response> {
    read_with_framing(reader, body, Framing::Required)
}
pub(crate) fn read_eof(reader: &mut impl BufRead, body: Body) -> io::Result<Response> {
    read_with_framing(reader, body, Framing::EofAllowed)
}
fn read_with_framing(
    reader: &mut impl BufRead,
    body: Body,
    framing: Framing,
) -> io::Result<Response> {
    let mut response = read_head(reader)?;
    if matches!(body, Body::Expected) {
        let headers: Vec<_> = response
            .headers
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let (body, raw) = read_body(reader, response.status, &headers, framing)?;
        response.body = body;
        response.raw.extend_from_slice(&raw);
    }
    Ok(response)
}
#[track_caller]
pub(crate) fn receive(stream: TcpStream, body: Body) -> Response {
    receive_with_framing(stream, body, Framing::Required)
}
#[track_caller]
pub(crate) fn receive_eof(stream: TcpStream, body: Body) -> Response {
    receive_with_framing(stream, body, Framing::EofAllowed)
}
#[track_caller]
fn receive_with_framing(stream: TcpStream, body: Body, framing: Framing) -> Response {
    let mut reader = BufReader::new(stream);
    let response =
        read_with_framing(&mut reader, body, framing).unwrap_or_else(|error| panic!("{error}"));
    if matches!(body, Body::Head) {
        let mut extra = Vec::new();
        reader.read_to_end(&mut extra).unwrap();
        assert!(extra.is_empty(), "HEAD emitted a response body: {extra:?}");
    } else if response.header_last("connection") == Some("close") {
        super::await_server_close(&mut reader);
    }
    response
}

pub(crate) struct Client<'a> {
    pub(crate) address: &'a str,
    pub(crate) host: &'a str,
    pub(crate) headers: &'a str,
    pub(crate) timeout: Duration,
}

impl Client<'_> {
    pub(crate) fn connect(&self) -> TcpStream {
        let stream = TcpStream::connect(self.address).unwrap();
        stream.set_read_timeout(Some(self.timeout)).unwrap();
        stream.set_write_timeout(Some(self.timeout)).unwrap();
        stream
    }

    pub(crate) fn request(&self, method: &str, path: &str, headers: &str, body: &[u8]) -> Response {
        self.request_with_framing(method, path, headers, body, Framing::Required)
    }
    pub(crate) fn request_eof(
        &self,
        method: &str,
        path: &str,
        headers: &str,
        body: &[u8],
    ) -> Response {
        self.request_with_framing(method, path, headers, body, Framing::EofAllowed)
    }
    fn request_with_framing(
        &self,
        method: &str,
        path: &str,
        headers: &str,
        body: &[u8],
        framing: Framing,
    ) -> Response {
        self.raw_with_framing(
            &super::process::request_head(
                method,
                path,
                self.host,
                &format!("{}{headers}Connection: close\r\n", self.headers),
                body.len(),
            ),
            body,
            framing,
        )
    }

    pub(crate) fn raw(&self, head: &str, body: &[u8]) -> Response {
        self.raw_with_framing(head, body, Framing::Required)
    }
    pub(crate) fn raw_eof(&self, head: &str, body: &[u8]) -> Response {
        self.raw_with_framing(head, body, Framing::EofAllowed)
    }
    fn raw_with_framing(&self, head: &str, body: &[u8], framing: Framing) -> Response {
        let mut stream = self.connect();
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
        receive_with_framing(
            stream,
            if head.starts_with("HEAD ") {
                Body::Head
            } else {
                Body::Expected
            },
            framing,
        )
    }
}
