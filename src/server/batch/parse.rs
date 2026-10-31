use super::{Part, RequestBody, invalid};
use crate::json::{self, JsonError};
use http_body_util::Full;
use hyper::{
    HeaderMap, Method, Request, Uri,
    body::Bytes,
    header::{HeaderName, HeaderValue},
};

const HEADER_LIMIT: usize = 8192;
const BODY_LIMIT: usize = 64 * 1024;
const DISCARD_LIMIT: usize = 4096;

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

fn headers(bytes: &[u8]) -> Result<HeaderMap, JsonError> {
    if bytes.len() > HEADER_LIMIT {
        return Err(invalid("Batch part headers exceed 8 KiB"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("Invalid batch part headers"))?;
    let mut headers = HeaderMap::new();
    if text.is_empty() {
        return Ok(headers);
    }
    for line in text.split("\r\n") {
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
    }
    Ok(headers)
}

fn block(bytes: &[u8]) -> Result<(&[u8], &[u8]), JsonError> {
    if bytes.starts_with(b"\r\n") {
        return Ok((&[], &bytes[2..]));
    }
    let end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .filter(|end| *end <= HEADER_LIMIT)
        .ok_or_else(|| invalid("Missing or oversized batch header block"))?;
    Ok((&bytes[..end], &bytes[end + 4..]))
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

fn uri(raw: &str, outer_query: Option<&str>) -> Result<Uri, JsonError> {
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

fn part(bytes: &Bytes, outer: &HeaderMap, query: Option<&str>) -> Result<Part, JsonError> {
    let (mime, http) = block(bytes)?;
    let mime = headers(mime)?;
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
    let line_end = http
        .windows(2)
        .position(|window| window == b"\r\n")
        .filter(|end| *end <= HEADER_LIMIT)
        .ok_or_else(|| invalid("Missing batch HTTP request line"))?;
    let line = std::str::from_utf8(&http[..line_end])
        .map_err(|_| invalid("Invalid batch request line"))?;
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
    let uri = uri(target, query)?;
    let (inner, body) = block(&http[line_end + 2..])?;
    let inner = headers(inner)?;
    if inner
        .keys()
        .any(|name| hop_header(name.as_str()) || name == "host")
    {
        return Err(invalid(
            "Batch parts cannot specify hop-by-hop headers or Host",
        ));
    }
    if body.len() > BODY_LIMIT {
        return Err(invalid("Batch part body exceeds 64 KiB"));
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
    let offset = bytes.len() - body.len();
    let mut request = Request::new(RequestBody::Buffered(Full::new(bytes.slice(offset..))));
    *request.method_mut() = method;
    *request.uri_mut() = uri;
    *request.headers_mut() = inherited;
    super::super::ingress::override_method(&mut request)?;
    Ok(Part { id, request })
}

fn delimiter_suffix(bytes: &[u8]) -> Result<(bool, usize), JsonError> {
    let (closing, bytes, prefix) = if let Some(bytes) = bytes.strip_prefix(b"--") {
        (true, bytes, 2)
    } else {
        (false, bytes, 0)
    };
    let padding = bytes
        .iter()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    if padding > HEADER_LIMIT {
        return Err(invalid("Oversized batch delimiter padding"));
    }
    let rest = &bytes[padding..];
    if rest.starts_with(b"\r\n") {
        return Ok((closing, prefix + padding + 2));
    }
    if closing && rest.is_empty() {
        return Ok((closing, prefix + padding));
    }
    Err(invalid("Malformed batch delimiter line"))
}

pub(super) fn parts(
    bytes: &Bytes,
    boundary: &str,
    outer: &HeaderMap,
    query: Option<&str>,
) -> Result<Vec<Part>, JsonError> {
    let first = format!("--{boundary}").into_bytes();
    let delimiter = format!("\r\n--{boundary}").into_bytes();
    let first_at = bytes
        .windows(first.len())
        .position(|window| window == first)
        .filter(|at| *at <= DISCARD_LIMIT && (*at == 0 || bytes[..*at].ends_with(b"\r\n")))
        .ok_or_else(|| invalid("Missing batch opening boundary"))?;
    let mut offset = first_at + first.len();
    let mut result = Vec::new();
    loop {
        let (closing, length) = delimiter_suffix(&bytes[offset..])?;
        offset += length;
        if closing {
            if result.is_empty() || bytes.len() - offset > DISCARD_LIMIT {
                return Err(invalid("Empty batch or oversized batch epilogue"));
            }
            return Ok(result);
        }
        if result.len() >= 100 {
            return Err(invalid("Batch requests support at most 100 calls"));
        }
        let end = bytes[offset..]
            .windows(delimiter.len())
            .position(|window| window == delimiter)
            .map(|end| offset + end)
            .ok_or_else(|| invalid("Missing batch closing boundary"))?;
        result.push(part(&bytes.slice(offset..end), outer, query)?);
        offset = end + delimiter.len();
    }
}
