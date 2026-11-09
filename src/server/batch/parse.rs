use super::{Part, RequestBody, invalid};
use crate::json::{self, JsonError};
use http_body_util::Full;
use hyper::{
    HeaderMap, Method, Request, Uri,
    body::Bytes,
    header::{HeaderName, HeaderValue},
};

const HEADER_LIMIT: usize = 8192;
const DISCARD_LIMIT: usize = 4096;

/// The line endings a batch envelope may use. Cloud Storage accepts LF wherever CRLF is expected.
#[derive(Clone, Copy)]
enum LineEnding {
    Crlf,
    Lf,
}

impl LineEnding {
    const fn as_bytes(self) -> &'static [u8] {
        match self {
            Self::Crlf => b"\r\n",
            Self::Lf => b"\n",
        }
    }
}

/// One line split off the front of an envelope. Header blocks, request lines and delimiters all
/// read lines through [`Line::split_first`], so they agree on what ends a line.
struct Line<'a> {
    text: &'a [u8],
    /// `None` only for a line that the end of the input cut off.
    ending: Option<LineEnding>,
    rest: &'a [u8],
}

impl<'a> Line<'a> {
    fn split_first(bytes: &'a [u8]) -> Option<Self> {
        if bytes.is_empty() {
            return None;
        }
        let split = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .and_then(|at| bytes.split_at_checked(at));
        let Some((before, from_lf)) = split else {
            return Some(Self {
                text: bytes,
                ending: None,
                rest: &[],
            });
        };
        let rest = from_lf.strip_prefix(b"\n")?;
        let (text, ending) = match before.strip_suffix(b"\r") {
            Some(text) => (text, LineEnding::Crlf),
            None => (before, LineEnding::Lf),
        };
        Some(Self {
            text,
            ending: Some(ending),
            rest,
        })
    }

    /// The text of a request line or header line. When the delimiter of an LF-framed envelope
    /// took a CRLF's LF, the part's last line still carries that CR.
    fn head_text(&self) -> &'a [u8] {
        if self.ending.is_some() {
            self.text
        } else {
            self.text.strip_suffix(b"\r").unwrap_or(self.text)
        }
    }
}

fn lines(bytes: &[u8]) -> impl Iterator<Item = Line<'_>> {
    std::iter::successors(Line::split_first(bytes), |line| {
        Line::split_first(line.rest)
    })
}

/// What a delimiter line turned out to be, with the bytes that follow it.
enum DelimiterLine<'a> {
    Next { ending: LineEnding, rest: &'a [u8] },
    Closing { epilogue: &'a [u8] },
}

pub(super) fn hop_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-connection"
            | "transfer-encoding"
            | "te"
            | "trailer"
            | "upgrade"
            | "proxy-authorization"
            | "proxy-authenticate"
    )
}

fn append_header(headers: &mut HeaderMap, line: &[u8]) -> Result<(), JsonError> {
    let line = std::str::from_utf8(line).map_err(|_| invalid("Invalid batch part headers"))?;
    let (name, value) = line
        .split_once(':')
        .ok_or_else(|| invalid("Invalid batch part header line"))?;
    let name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| invalid("Invalid batch part header name"))?;
    let value = HeaderValue::from_str(value.trim())
        .map_err(|_| invalid("Invalid batch part header value"))?;
    if headers.len() >= 64 {
        return Err(invalid("Batch parts support at most 64 headers"));
    }
    headers.append(name, value);
    Ok(())
}

/// Parses the header block at the start of `bytes` and returns what follows its blank line. `None`
/// means the part ended before a blank line, so the block runs to the end of the part.
fn header_block(bytes: &[u8]) -> Result<(HeaderMap, Option<&[u8]>), JsonError> {
    let mut headers = HeaderMap::new();
    let mut size = 0;
    for line in lines(bytes) {
        let text = line.head_text();
        if text.is_empty() {
            return Ok((headers, Some(line.rest)));
        }
        size += text.len();
        if size > HEADER_LIMIT {
            return Err(invalid("Missing or oversized batch header block"));
        }
        append_header(&mut headers, text)?;
        // Only an ending with another line after it counts toward the limit.
        size += line.ending.map_or(0, |ending| ending.as_bytes().len());
    }
    Ok((headers, None))
}

fn single<'a>(headers: &'a HeaderMap, key: &str) -> Result<Option<&'a str>, JsonError> {
    let mut values = headers.get_all(key).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(invalid("Duplicate batch framing header"));
    }
    value
        .to_str()
        .map(Some)
        .map_err(|_| invalid("Invalid batch framing header"))
}

fn uri(raw: &str, outer_query: Option<&str>, api_base_url: &str) -> Result<Uri, JsonError> {
    // Google answers absolute request lines on its own endpoint like relative paths, and SDKs
    // such as the Java client send them. Only the advertised API origin is accepted; any other
    // origin fails the whole envelope so no part is executed or forwarded elsewhere.
    let raw = if raw.starts_with('/') {
        raw
    } else {
        crate::url::path_below_base(raw, api_base_url).ok_or_else(|| {
            invalid("Absolute batch part URLs must use the advertised API endpoint")
        })?
    };
    if !raw.starts_with("/storage/v1/") || raw.contains(['#', '\\']) {
        return Err(invalid(
            "Batch parts require relative Google JSON API paths; uploads, downloads, internal routes and nested batches are unsupported",
        ));
    }
    let uri: Uri = raw
        .parse()
        .map_err(|_| invalid("Invalid batch request target"))?;
    if uri.scheme().is_some() || uri.authority().is_some() {
        return Err(invalid("Batch parts cannot specify an authority"));
    }
    let inner = json::parse_query(uri.query().unwrap_or(""))?;
    let mut target = raw.to_owned();
    let outer = outer_query.unwrap_or("");
    json::parse_query(outer)?;
    for pair in outer.split('&').filter(|pair| !pair.is_empty()) {
        let parsed = json::parse_query(pair)?;
        if parsed.keys().any(|key| !inner.contains_key(key)) {
            target.push(if target.contains('?') { '&' } else { '?' });
            target.push_str(pair);
        }
    }
    let uri: Uri = target
        .parse()
        .map_err(|_| invalid("Invalid inherited batch query"))?;
    if json::parse_query(uri.query().unwrap_or(""))?
        .get("alt")
        .is_some_and(|alt| alt == "media")
    {
        return Err(invalid(
            "Batch requests cannot upload or download object media",
        ));
    }
    Ok(uri)
}

fn part(
    bytes: &Bytes,
    outer: &HeaderMap,
    query: Option<&str>,
    api_base_url: &str,
) -> Result<Part, JsonError> {
    let (mime, Some(http)) = header_block(bytes)? else {
        return Err(invalid("Missing or oversized batch header block"));
    };
    if single(&mime, "content-type")?
        .is_none_or(|value| !value.eq_ignore_ascii_case("application/http"))
        || single(&mime, "content-transfer-encoding")?
            .is_some_and(|value| !value.eq_ignore_ascii_case("binary"))
    {
        return Err(invalid(
            "Batch parts require Content-Type: application/http and binary transfer encoding",
        ));
    }
    let id = single(&mime, "content-id")?.map(str::to_owned);
    if id
        .as_ref()
        .is_some_and(|id| id.is_empty() || id.len() > 512 || !id.is_ascii())
    {
        return Err(invalid("Invalid batch Content-ID"));
    }
    // The request line may end at the part's end: Cloud Storage runs a call whose request line is
    // directly followed by the delimiter.
    let request_line =
        Line::split_first(http).ok_or_else(|| invalid("Missing batch HTTP request line"))?;
    let line = request_line.head_text();
    if line.len() > HEADER_LIMIT {
        return Err(invalid("Missing batch HTTP request line"));
    }
    let line = std::str::from_utf8(line).map_err(|_| invalid("Invalid batch request line"))?;
    let fields: Vec<_> = line.split(' ').collect();
    let [method, target, "HTTP/1.1"] = fields.as_slice() else {
        return Err(invalid("Batch parts require an HTTP/1.1 request line"));
    };
    let method: Method = method
        .parse()
        .map_err(|_| invalid("Invalid batch method"))?;
    if !matches!(
        method,
        Method::GET | Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        return Err(invalid("Unsupported batch method"));
    }
    let uri = uri(target, query, api_base_url)?;
    // Headers that run into the delimiter leave no blank line, as OpenDAL sends them: the CRLF
    // that would have ended the block is the delimiter's own. Such a call has no body.
    let (inner, body) = header_block(request_line.rest)?;
    let body = body.unwrap_or_default();
    if inner
        .keys()
        .any(|name| hop_header(name.as_str()) || name == "host")
    {
        return Err(invalid(
            "Batch parts cannot specify hop-by-hop headers or Host",
        ));
    }
    if let Some(length) = single(&inner, "content-length")?
        && (length.is_empty()
            || !length.bytes().all(|byte| byte.is_ascii_digit())
            || length.parse::<usize>().ok() != Some(body.len()))
    {
        return Err(invalid("Batch part Content-Length does not match its body"));
    }
    let mut inherited = HeaderMap::new();
    for (name, value) in outer {
        if !name.as_str().starts_with("content-")
            && !hop_header(name.as_str())
            && name != "host"
            && !inner.contains_key(name)
        {
            inherited.append(name.clone(), value.clone());
        }
    }
    inherited.extend(inner);
    inherited.insert("content-length", HeaderValue::from(body.len()));
    let mut request = Request::new(RequestBody::Buffered(Full::new(bytes.slice_ref(body))));
    *request.method_mut() = method;
    *request.uri_mut() = uri;
    *request.headers_mut() = inherited;
    super::super::ingress::override_method(&mut request)?;
    Ok(Part { id, request })
}

/// Reads the rest of a delimiter line, starting right after its `--boundary`.
fn delimiter_line(bytes: &[u8]) -> Result<DelimiterLine<'_>, JsonError> {
    let malformed = || invalid("Malformed batch delimiter line");
    let line = Line::split_first(bytes).ok_or_else(malformed)?;
    let (padding, delimiter) = if let Some(padding) = line.text.strip_prefix(b"--") {
        // Only a closing delimiter may end the input without a line ending.
        let epilogue = line.rest;
        (padding, DelimiterLine::Closing { epilogue })
    } else {
        let ending = line.ending.ok_or_else(malformed)?;
        let rest = line.rest;
        (line.text, DelimiterLine::Next { ending, rest })
    };
    if padding.len() > HEADER_LIMIT {
        return Err(invalid("Oversized batch delimiter padding"));
    }
    if !padding.iter().all(|byte| matches!(byte, b' ' | b'\t')) {
        return Err(malformed());
    }
    Ok(delimiter)
}

/// Returns what follows the first `--boundary` at a line start within the preamble limit.
fn after_opening_delimiter<'a>(bytes: &'a [u8], boundary: &[u8]) -> Option<&'a [u8]> {
    std::iter::successors(Some(bytes), |unread| {
        Line::split_first(unread).map(|line| line.rest)
    })
    .take_while(|unread| bytes.len() - unread.len() <= DISCARD_LIMIT)
    .find_map(|unread| unread.strip_prefix(boundary))
}

/// Splits `bytes` around its first `delimiter`, without the delimiter itself.
fn split_at_delimiter<'a>(bytes: &'a [u8], delimiter: &[u8]) -> Option<(&'a [u8], &'a [u8])> {
    let at = bytes
        .windows(delimiter.len())
        .position(|window| window == delimiter)?;
    let (before, from_delimiter) = bytes.split_at_checked(at)?;
    Some((before, from_delimiter.strip_prefix(delimiter)?))
}

pub(super) fn parts(
    bytes: &Bytes,
    boundary: &str,
    outer: &HeaderMap,
    query: Option<&str>,
    api_base_url: &str,
) -> Result<Vec<Part>, JsonError> {
    let boundary = format!("--{boundary}").into_bytes();
    let after_boundary = after_opening_delimiter(bytes, &boundary)
        .ok_or_else(|| invalid("Missing batch opening boundary"))?;
    let DelimiterLine::Next { ending, mut rest } = delimiter_line(after_boundary)? else {
        return Err(invalid("Empty batch or oversized batch epilogue"));
    };
    // Later delimiters must begin with the line ending the opening line used. In a CRLF envelope
    // an LF-only `--boundary` line is therefore body content: RFC 2046 defines the delimiter as
    // CRLF `--boundary`, and honoring LF there would cut a body short and run what follows it as
    // a part of its own. An LF-only envelope, which Cloud Storage also accepts, has no CRLF to
    // look for and delimits on LF, so a CR before that LF stays in the body.
    let delimiter = [ending.as_bytes(), boundary.as_slice()].concat();
    let mut result = Vec::new();
    loop {
        if result.len() >= 100 {
            return Err(invalid("Batch requests support at most 100 calls"));
        }
        let (section, after_boundary) = split_at_delimiter(rest, &delimiter)
            .ok_or_else(|| invalid("Missing batch closing boundary"))?;
        result.push(part(&bytes.slice_ref(section), outer, query, api_base_url)?);
        match delimiter_line(after_boundary)? {
            DelimiterLine::Next { rest: next, .. } => rest = next,
            DelimiterLine::Closing { epilogue } => {
                if epilogue.len() > DISCARD_LIMIT {
                    return Err(invalid("Empty batch or oversized batch epilogue"));
                }
                return Ok(result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    const REQUEST: &str = "PATCH /storage/v1/b/x HTTP/1.1";

    async fn bodies(envelope: String) -> Vec<Bytes> {
        let parts = parts(
            &Bytes::from(envelope),
            "B",
            &HeaderMap::new(),
            None,
            "http://localhost",
        )
        .unwrap_or_else(|error| panic!("{}", error.message));
        let mut bodies = Vec::new();
        for part in parts {
            let RequestBody::Buffered(body) = part.request.into_body() else {
                panic!("parts hold buffered bodies");
            };
            bodies.push(body.collect().await.unwrap().to_bytes());
        }
        bodies
    }

    /// The handlers only accept JSON, so these bodies are asserted here, byte for byte.
    #[tokio::test]
    async fn bodies_keep_every_byte_except_the_delimiter_line_ending() {
        for body in [
            "",
            "a",
            "a\n",
            "a\r",
            "a\r\n",
            "x--B y",
            "a\n--B\nb",
            "a\n--B--\n",
        ] {
            let envelope = format!(
                "--B\r\nContent-Type: application/http\r\n\r\n{REQUEST}\r\n\r\n{body}\r\n--B--\r\n"
            );
            assert_eq!(bodies(envelope).await, [body], "{body:?}");
        }
        for body in ["", "a", "a\n", "a\r", "a\r\n", "x--B y"] {
            let envelope =
                format!("--B\nContent-Type: application/http\n\n{REQUEST}\n\n{body}\n--B--\n");
            assert_eq!(bodies(envelope).await, [body], "{body:?}");
        }
    }

    #[tokio::test]
    async fn a_part_ending_before_its_blank_line_has_an_empty_body() {
        for tail in ["\r\n", "\r\nX-A: b\r\n"] {
            let envelope =
                format!("--B\r\nContent-Type: application/http\r\n\r\n{REQUEST}{tail}--B--\r\n");
            assert_eq!(bodies(envelope).await, [""], "{tail:?}");
        }
    }
}
