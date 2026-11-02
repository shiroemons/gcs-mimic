use super::{JsonError, ObjectMetadataRequest, parse_object_metadata};
use crate::object::{ExpectedHashes, ObjectMetadataUpdate};
use bytes::{Buf, Bytes, BytesMut};
use http_body_util::BodyExt;
use hyper::{
    HeaderMap, StatusCode,
    body::Body,
    header::{HeaderName, HeaderValue},
};

const HEADER_CAP: usize = 8192;
const DISCARD_CAP: usize = 4096;
const JSON_CAP: usize = 64 * 1024;

fn invalid() -> JsonError {
    JsonError::invalid_parameter("Invalid multipart/related framing")
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Which quotes may delimit a Content-Type parameter value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParamQuotes {
    /// RFC 2045 quoted strings only.
    Double,
    /// Also a value in single quotes that is not itself a valid token. gcloud storage (apitools,
    /// Python's email package) sends `boundary='===============N=='`, and Cloud Storage accepts
    /// it on that production path; no live recording was made. A single-quoted value that is a
    /// valid RFC token keeps its literal meaning, quotes included.
    DoubleOrSingle,
}

// Quoted parameter values may contain semicolons and escaped quotes.
fn content_type(value: &str) -> Result<(String, Vec<(String, String)>), JsonError> {
    content_type_with(value, ParamQuotes::Double)
}

fn content_type_with(
    value: &str,
    quotes: ParamQuotes,
) -> Result<(String, Vec<(String, String)>), JsonError> {
    let (mime, rest) = value.split_once(';').unwrap_or((value, ""));
    let mime = mime.trim();
    if !mime
        .split_once('/')
        .is_some_and(|(a, b)| token(a) && token(b))
    {
        return Err(invalid());
    }
    let mut rest = rest.trim();
    let mut params = Vec::new();
    while !rest.is_empty() {
        let (key, tail) = rest.split_once('=').ok_or_else(invalid)?;
        let key = key.trim().to_ascii_lowercase();
        if !token(&key) || params.iter().any(|(k, _)| k == &key) {
            return Err(invalid());
        }
        let tail = tail.trim_start();
        let (value, tail) = if let Some(tail) = tail.strip_prefix('"') {
            let mut escaped = false;
            let mut end = None;
            let mut value = String::new();
            for (i, c) in tail.char_indices() {
                if escaped {
                    value.push(c);
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    end = Some(i);
                    break;
                } else {
                    value.push(c);
                }
            }
            let end = end.ok_or_else(invalid)?;
            (value, &tail[end + 1..])
        } else if quotes == ParamQuotes::DoubleOrSingle
            && let Some(inner) = tail.strip_prefix('\'')
            && let Some(end) = inner.find('\'')
            && !token(&tail[..end + 2])
        {
            (inner[..end].to_owned(), &inner[end + 1..])
        } else {
            let index = tail.find(';').unwrap_or(tail.len());
            let (value, tail) = tail.split_at(index);
            let value = value.trim();
            if !token(value) {
                return Err(invalid());
            }
            (value.to_owned(), tail)
        };
        if value.bytes().any(|b| b < 32 || b == 127) {
            return Err(invalid());
        }
        params.push((key, value));
        rest = tail.trim_start();
        if !rest.is_empty() {
            rest = rest.strip_prefix(';').ok_or_else(invalid)?.trim_start();
        }
        if rest.is_empty() && tail.trim_end().ends_with(';') {
            return Err(invalid());
        }
    }
    Ok((mime.to_ascii_lowercase(), params))
}

pub(crate) fn multipart_boundary(headers: &HeaderMap) -> Result<String, JsonError> {
    let value = headers
        .get("content-type")
        .ok_or_else(invalid)?
        .to_str()
        .map_err(|_| invalid())?;
    let (mime, params) = content_type_with(value, ParamQuotes::DoubleOrSingle)?;
    if mime != "multipart/related" {
        return Err(invalid());
    }
    // A multipart type without a boundary got the front end's bare text page.
    let boundary = boundary(&params).map_err(|mut error| {
        error.plain = Some(&super::error::BAD_CONTENT_TYPE_TEXT);
        error
    })?;
    // RFC 2387 permits type and start; reordered roots are outside this ordered subset.
    for (key, value) in &params {
        if key != "boundary" && !(key == "type" && value.eq_ignore_ascii_case("application/json")) {
            return Err(JsonError::unsupported_object(key));
        }
    }
    Ok(boundary)
}

/// Whether the request declares a `multipart/*` body at all.
pub(crate) fn is_multipart_content_type(headers: &HeaderMap) -> bool {
    headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("multipart/")
        })
}

/// The media part and the metadata must agree on the type. The message is Cloud Storage's,
/// which spells out where each upload type puts its type.
pub(crate) fn check_declared_content_type(declared: &str, part: &str) -> Result<(), JsonError> {
    let essence = |value: &str| {
        value
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
    };
    if essence(declared) == essence(part) {
        return Ok(());
    }
    Err(JsonError::invalid(format!(
        "Content-Type specified in the upload ({part}) does not match Content-Type specified in metadata ({declared}). If it was a simple upload (uploadType=media), the Content-Type was specified as a bare header. If it was a multipart upload (uploadType=multipart), then the Content-Type was specified in the second part of the multipart. If it was a resumable upload (uploadType=resumable), then the Content-Type was specified with the X-Upload-Content-Type header with the start of the resumable session. For more information, see https://cloud.google.com/storage/docs/json_api/v1/how-tos/upload."
    ))
    .html_typed())
}

pub(crate) fn batch_boundary(headers: &HeaderMap) -> Result<String, JsonError> {
    let mut values = headers.get_all("content-type").iter();
    let value = values.next().ok_or_else(invalid)?;
    if values.next().is_some() {
        return Err(invalid());
    }
    let (mime, params) = content_type(value.to_str().map_err(|_| invalid())?)?;
    if mime != "multipart/mixed" || params.iter().any(|(key, _)| key != "boundary") {
        return Err(invalid());
    }
    boundary(&params)
}

/// XML POST Object forms use the same RFC 2046 boundary grammar as multipart/related.
pub(crate) fn form_data_boundary(headers: &HeaderMap) -> Result<String, JsonError> {
    let value = headers
        .get("content-type")
        .ok_or_else(invalid)?
        .to_str()
        .map_err(|_| invalid())?;
    let (mime, params) = content_type(value)?;
    if mime != "multipart/form-data" {
        return Err(invalid());
    }
    let boundary = boundary(&params)?;
    // Forms must be UTF-8 encoded; another declared charset would change field values.
    for (key, value) in &params {
        if key != "boundary" && !(key == "charset" && value.eq_ignore_ascii_case("utf-8")) {
            return Err(JsonError::unsupported_object(key));
        }
    }
    Ok(boundary)
}

fn boundary(params: &[(String, String)]) -> Result<String, JsonError> {
    let boundary = params
        .iter()
        .find(|(key, _)| key == "boundary")
        .map(|(_, v)| v)
        .ok_or_else(invalid)?;
    if boundary.is_empty()
        || boundary.len() > 70
        || boundary.ends_with(' ')
        || !boundary
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"'()+_,-./:=? ".contains(&b))
    {
        return Err(invalid());
    }
    Ok(boundary.clone())
}

/// Which part of a multipart upload a header block introduces. The metadata part must be
/// `application/json`; the media part may declare any valid content type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PartKind {
    Metadata,
    Media,
}

/// How a header line may end: a delimiter line may also end the body.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LineEnd {
    Crlf,
    CrlfOrEof,
}

/// Which line breaks frame delimiters and part headers. One body never mixes them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Framing {
    /// RFC 2046 CRLF only.
    Crlf,
    /// Bare LF only.
    Lf,
    /// Not settled yet: the first boundary line chooses `Lf` when it ends in a bare LF and
    /// `Crlf` otherwise, as Go's mime/multipart decides. gcloud storage (apitools) frames
    /// multipart/related uploads with bare LFs throughout on its production path; no live
    /// recording was made. A CRLF body keeps strict CRLF delimiters, so media containing an
    /// LF followed by boundary text stays media.
    Detect,
}

pub(crate) struct Multipart<B> {
    body: B,
    pending: Bytes,
    buffer: BytesMut,
    /// `CRLF--boundary`, or `LF--boundary` once the framing is `Lf`.
    delimiter: Vec<u8>,
    framing: Framing,
    eof: bool,
    closed: bool,
}

impl<B: Body<Data = Bytes> + Unpin> Multipart<B> {
    /// A strictly CRLF-framed body, such as an XML POST Object form.
    pub(crate) fn new(body: B, boundary: &str) -> Self {
        Self::with_framing(body, boundary, Framing::Crlf)
    }

    /// A JSON multipart/related upload, framed by CRLF or, throughout, by bare LF.
    pub(crate) fn related(body: B, boundary: &str) -> Self {
        Self::with_framing(body, boundary, Framing::Detect)
    }

    fn with_framing(body: B, boundary: &str, framing: Framing) -> Self {
        let mut delimiter = format!("\r\n--{boundary}").into_bytes();
        if framing == Framing::Lf {
            delimiter.remove(0);
        }
        Self {
            body,
            pending: Bytes::new(),
            // A synthetic CRLF lets the first boundary use the same search as later boundaries.
            buffer: BytesMut::from(&b"\r\n"[..]),
            delimiter,
            framing,
            eof: false,
            closed: false,
        }
    }

    /// Settles a `Detect` framing from the first line that starts with `--boundary`: a bare LF
    /// after the boundary and any linear white space selects `Lf`, anything else `Crlf`.
    /// Returns how many leading buffer bytes may be discarded while more input is needed, or
    /// `None` once the framing is settled. Nothing is consumed, so the delimiter search of
    /// the settled framing runs over the same bytes.
    fn detect(&mut self) -> Option<usize> {
        let pattern = &self.delimiter[1..];
        let Some(index) = self
            .buffer
            .windows(pattern.len())
            .position(|v| v == pattern)
        else {
            if self.eof {
                self.framing = Framing::Crlf;
                return None;
            }
            // Keep a partial `LF--boundary` and the CR that may precede it.
            return Some(self.buffer.len().saturating_sub(self.delimiter.len()));
        };
        let rest = &self.buffer[index + pattern.len()..];
        match rest.iter().find(|b| !matches!(b, b' ' | b'\t')) {
            Some(b'\n') => {
                self.framing = Framing::Lf;
                self.delimiter.remove(0);
            }
            // The line ending is not known yet; overlong white space fails as a CRLF line.
            None if !self.eof && rest.len() <= HEADER_CAP => {
                return Some(index.saturating_sub(1));
            }
            _ => self.framing = Framing::Crlf,
        }
        None
    }

    fn find_delimiter(&self) -> Option<usize> {
        self.buffer
            .windows(self.delimiter.len())
            .position(|v| v == self.delimiter)
    }

    /// Bytes kept back while no delimiter is complete, as they may start one.
    fn held_back(&self) -> usize {
        self.delimiter.len() - 1
    }

    /// The length of a line break under the settled framing.
    fn newline_len(&self) -> usize {
        if self.framing == Framing::Lf { 1 } else { 2 }
    }

    async fn fill(&mut self) -> Result<(), JsonError> {
        while self.pending.is_empty() && !self.eof {
            match self.body.frame().await {
                Some(Ok(frame)) => self.pending = frame.into_data().map_err(|_| invalid())?,
                Some(Err(_)) => return Err(invalid()),
                None => self.eof = true,
            }
        }
        let length = self.pending.len().min(8192);
        self.buffer
            .extend_from_slice(&self.pending.split_to(length));
        Ok(())
    }

    async fn line(&mut self, cap: usize, end: LineEnd) -> Result<Vec<u8>, JsonError> {
        let mut scanned = 0;
        loop {
            if let Some((end, terminator)) = self.line_end(scanned) {
                if end > cap {
                    return Err(invalid());
                }
                let line = self.buffer.split_to(end).to_vec();
                self.buffer.advance(terminator);
                return Ok(line);
            }
            // Under CRLF framing a trailing CR may become the line terminator in the next frame.
            if self.buffer.len() > cap
                && (self.framing == Framing::Lf
                    || self.eof
                    || self.buffer.len() != cap + 1
                    || !self.buffer.ends_with(b"\r"))
            {
                return Err(invalid());
            }
            if self.eof {
                return if end == LineEnd::CrlfOrEof {
                    Ok(self.buffer.split().to_vec())
                } else {
                    Err(invalid())
                };
            }
            scanned = self.buffer.len().saturating_sub(1);
            self.fill().await?;
        }
    }

    /// The end of the first line at or after `from` and its terminator length. Under LF
    /// framing a CR before the LF stays in the line, so a CRLF line fails as a header.
    fn line_end(&self, from: usize) -> Option<(usize, usize)> {
        if self.framing == Framing::Lf {
            self.buffer[from..]
                .iter()
                .position(|b| *b == b'\n')
                .map(|end| (from + end, 1))
        } else {
            self.buffer[from..]
                .windows(2)
                .position(|v| v == b"\r\n")
                .map(|end| (from + end, 2))
        }
    }

    /// Consumes the delimiter at the start of the buffer and the rest of its line.
    async fn delimiter(&mut self) -> Result<(), JsonError> {
        self.buffer.advance(self.delimiter.len());
        let line = self.line(HEADER_CAP, LineEnd::CrlfOrEof).await?;
        let tail = if let Some(tail) = line.strip_prefix(b"--") {
            self.closed = true;
            tail
        } else {
            &line
        };
        if tail.iter().any(|b| !matches!(b, b' ' | b'\t'))
            || (!self.closed && self.eof && self.buffer.is_empty())
        {
            return Err(invalid());
        }
        Ok(())
    }

    pub(crate) async fn start(&mut self) -> Result<(), JsonError> {
        let mut discarded = 0;
        loop {
            if self.framing == Framing::Detect
                && let Some(discard) = self.detect()
            {
                discarded += discard;
                if discarded > DISCARD_CAP + 2 {
                    return Err(invalid());
                }
                self.buffer.advance(discard);
                self.fill().await?;
                continue;
            }
            if let Some(index) = self.find_delimiter() {
                discarded += index;
                if discarded > DISCARD_CAP + 2 {
                    return Err(invalid());
                }
                self.buffer.advance(index);
                self.delimiter().await?;
                return if self.closed { Err(invalid()) } else { Ok(()) };
            }
            let discard = self.buffer.len().saturating_sub(self.held_back());
            discarded += discard;
            if discarded > DISCARD_CAP + 2 || self.eof {
                return Err(invalid());
            }
            self.buffer.advance(discard);
            self.fill().await?;
        }
    }

    /// Whether the closing delimiter has been read, so no further part follows.
    pub(crate) fn closed(&self) -> bool {
        self.closed
    }

    pub(crate) async fn headers(&mut self, part: PartKind) -> Result<Option<String>, JsonError> {
        let headers = self.part_headers().await?;
        let value = headers
            .get("content-type")
            .map(|v| {
                v.to_str()
                    .map(str::trim)
                    .map(str::to_owned)
                    .map_err(|_| invalid())
            })
            .transpose()?;
        if part == PartKind::Metadata {
            let (mime, params) = content_type(value.as_deref().ok_or_else(invalid)?)?;
            if mime != "application/json"
                || params
                    .iter()
                    .any(|(k, v)| k != "charset" || !v.eq_ignore_ascii_case("utf-8"))
            {
                return Err(invalid());
            }
        } else if let Some(value) = &value {
            content_type(value)?;
        }
        Ok(value)
    }

    /// Read one bounded part header block; form-data callers need Content-Disposition.
    pub(crate) async fn part_headers(&mut self) -> Result<HeaderMap, JsonError> {
        if self.closed {
            return Err(invalid());
        }
        let mut headers = HeaderMap::new();
        let mut size = 0;
        loop {
            let line = self
                .line(HEADER_CAP.saturating_sub(size), LineEnd::Crlf)
                .await?;
            size += line.len() + self.newline_len();
            if size > HEADER_CAP {
                return Err(invalid());
            }
            if line.is_empty() {
                break;
            }
            let index = line.iter().position(|b| *b == b':').ok_or_else(invalid)?;
            let key = HeaderName::from_bytes(&line[..index]).map_err(|_| invalid())?;
            let value = HeaderValue::from_bytes(&line[index + 1..]).map_err(|_| invalid())?;
            if headers.insert(key, value).is_some() {
                return Err(invalid());
            }
        }
        if let Some(value) = headers.get("content-transfer-encoding")
            && !value
                .to_str()
                .map_err(|_| invalid())?
                .trim()
                .eq_ignore_ascii_case("binary")
        {
            return Err(invalid());
        }
        Ok(headers)
    }

    pub(crate) async fn next_chunk(&mut self) -> Result<Option<Bytes>, JsonError> {
        loop {
            if let Some(index) = self.find_delimiter() {
                if index > 0 {
                    return Ok(Some(self.buffer.split_to(index).freeze()));
                }
                self.delimiter().await?;
                return Ok(None);
            }
            let length = self.buffer.len().saturating_sub(self.held_back());
            if length > 0 {
                return Ok(Some(self.buffer.split_to(length).freeze()));
            }
            if self.eof {
                return Err(invalid());
            }
            self.fill().await?;
        }
    }

    pub(crate) async fn metadata(&mut self) -> Result<Vec<u8>, JsonError> {
        self.start().await?;
        self.headers(PartKind::Metadata).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = self.next_chunk().await? {
            if chunk.len() > JSON_CAP - bytes.len() {
                return Err(JsonError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "uploadTooLarge",
                    "Multipart metadata exceeds 64 KiB",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        // Only the metadata part arrived; Cloud Storage counts the payloads it received.
        if self.closed {
            return Err(JsonError::new(
                StatusCode::BAD_REQUEST,
                "invalidPayloadSize",
                "Payload size invalid. Expected 2-3 payloads. Actual size: 1",
            ));
        }
        Ok(bytes)
    }

    pub(crate) async fn finish(&mut self) -> Result<(), JsonError> {
        if !self.closed {
            return Err(invalid());
        }
        let mut size = self.buffer.len();
        self.buffer.clear();
        loop {
            if size > DISCARD_CAP {
                return Err(invalid());
            }
            if self.eof {
                return Ok(());
            }
            self.fill().await?;
            size += self.buffer.len();
            self.buffer.clear();
        }
    }

    pub(crate) async fn checksums(
        &mut self,
        expected: ExpectedHashes,
    ) -> Result<ExpectedHashes, JsonError> {
        if self.closed {
            return Ok(expected);
        }
        self.headers(PartKind::Metadata).await?;
        let mut bytes = Vec::new();
        while let Some(chunk) = self.next_chunk().await? {
            if chunk.len() > JSON_CAP - bytes.len() {
                return Err(JsonError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "uploadTooLarge",
                    "Multipart checksums exceed 64 KiB",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
            JsonError::new(
                StatusCode::BAD_REQUEST,
                "parseError",
                "Invalid checksum JSON",
            )
        })?;
        let mut values = value.as_object().cloned().ok_or_else(invalid)?;
        if values
            .keys()
            .any(|k| !matches!(k.as_str(), "md5Hash" | "crc32c"))
        {
            return Err(JsonError::new(
                StatusCode::BAD_REQUEST,
                "invalid",
                "Only checksum fields are supported in the third part",
            ));
        }
        merge_hashes(&mut values, expected)
    }
}

pub(crate) fn multipart_metadata(
    bytes: &[u8],
    query_name: Option<String>,
    expected: ExpectedHashes,
) -> Result<(String, ObjectMetadataUpdate, ExpectedHashes), JsonError> {
    let ObjectMetadataRequest {
        mut update,
        mut unchanged,
    } = parse_object_metadata(bytes, super::WriteMode::Patch).map_err(|error| {
        // The upload front end labelled a metadata part that is not JSON as `text/html`.
        if error.message.starts_with("Parse Error") {
            error.html_typed()
        } else {
            error
        }
    })?;
    update.content_type = update
        .content_type
        .filter(|value| value.as_ref().is_some_and(|value| !value.is_empty()));
    if let Some(Some(metadata)) = &update.metadata
        && metadata
            .iter()
            .filter_map(|(key, value)| value.as_ref().map(|value| key.len() + value.len()))
            .sum::<usize>()
            > 8192
    {
        return Err(JsonError::invalid_argument(
            "Custom metadata exceeds 8192 bytes",
        ));
    }
    let metadata_name = unchanged.remove("name").filter(|value| !value.is_null());
    if metadata_name.as_ref().is_some_and(|name| !name.is_string()) {
        return Err(JsonError::invalid_argument("name must be a string or null"));
    }
    let name = match query_name {
        Some(name) => name,
        None => metadata_name
            .ok_or_else(JsonError::missing_object_name)?
            .as_str()
            .ok_or_else(|| JsonError::invalid_argument("name must be a string"))?
            .to_owned(),
    };
    super::validate_object_name(&name)?;
    if let Some(bucket) = unchanged.remove("bucket")
        && !bucket.is_string()
    {
        return Err(JsonError::invalid_argument("bucket must be a string"));
    }
    if let Some(kind) = unchanged.remove("kind")
        && kind != "storage#object"
    {
        return Err(JsonError::unsupported_object("kind"));
    }
    // Insert accepts a creation class; metadata updates keep comparing it as output-only.
    if let Some(class) = unchanged
        .remove("storageClass")
        .filter(|value| !value.is_null())
    {
        let class = class
            .as_str()
            .ok_or_else(|| JsonError::invalid_argument("storageClass must be a string or null"))?;
        crate::object::validate_object_storage_class(class).map_err(JsonError::from)?;
        update.storage_class = Some(class.to_owned());
    }
    let expected = merge_hashes(&mut unchanged, expected)?;
    if let Some((key, _)) = unchanged.into_iter().next() {
        return Err(JsonError::unsupported_object(&key));
    }
    Ok((name, update, expected))
}

fn merge_hashes(
    values: &mut serde_json::Map<String, serde_json::Value>,
    mut expected: ExpectedHashes,
) -> Result<ExpectedHashes, JsonError> {
    for key in ["md5Hash", "crc32c"] {
        if let Some(value) = values.remove(key) {
            let value = value
                .as_str()
                .ok_or_else(|| JsonError::invalid_parameter("Checksum must be a base64 string"))?;
            if key == "md5Hash" {
                let value = super::objects::decode_hash(value)?;
                if expected.md5.is_some_and(|old| old != value) {
                    return Err(JsonError::invalid_parameter("Conflicting MD5 checksums"));
                }
                expected.md5 = Some(value);
            } else {
                let value = u32::from_be_bytes(super::objects::decode_hash(value)?);
                if expected.crc32c.is_some_and(|old| old != value) {
                    return Err(JsonError::invalid_parameter("Conflicting CRC32C checksums"));
                }
                expected.crc32c = Some(value);
            }
        }
    }
    Ok(expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::chunk_body;
    use http_body_util::Full;

    fn wire(metadata: &[u8], media: &[u8]) -> Vec<u8> {
        let mut body =
            b"--boundary\r\nContent-Type: application/json; charset=\"UTF-8\"\r\n\r\n".to_vec();
        body.extend_from_slice(metadata);
        body.extend_from_slice(b"\r\n--boundary\r\nContent-Type: application/octet-stream\r\n\r\n");
        body.extend_from_slice(media);
        body.extend_from_slice(b"\r\n--boundary--\r\n");
        body
    }

    const FRAMINGS: [Framing; 2] = [Framing::Crlf, Framing::Detect];

    /// Parses a CRLF-framed body strictly and with detected framing; both must agree on it.
    async fn parse(body: impl Into<Bytes>) -> Result<(Vec<u8>, Vec<u8>), JsonError> {
        let body = body.into();
        let strict = parse_with(Full::new(body.clone()), Framing::Crlf).await;
        let detected = parse_with(Full::new(body), Framing::Detect).await;
        assert_eq!(strict.as_ref().ok(), detected.as_ref().ok());
        strict
    }

    async fn parse_with<B: Body<Data = Bytes> + Unpin>(
        body: B,
        framing: Framing,
    ) -> Result<(Vec<u8>, Vec<u8>), JsonError> {
        parse_bounded(body, "boundary", framing).await
    }

    async fn parse_bounded<B: Body<Data = Bytes> + Unpin>(
        body: B,
        boundary: &str,
        framing: Framing,
    ) -> Result<(Vec<u8>, Vec<u8>), JsonError> {
        let mut parser = Multipart::with_framing(body, boundary, framing);
        let metadata = parser.metadata().await?;
        parser.headers(PartKind::Media).await?;
        let mut media = Vec::new();
        while let Some(chunk) = parser.next_chunk().await? {
            media.extend_from_slice(&chunk);
        }
        parser.finish().await?;
        Ok((metadata, media))
    }

    #[tokio::test]
    async fn multipart_parser_preserves_boundary_prefixes_at_every_chunk_split() {
        let payload = b"\x00\xff\r\n--boundar\r\n--\r\n--bound\r\nend\r\n";
        let body = wire(b"{}", payload);
        for framing in FRAMINGS {
            let (metadata, media) = parse_in_chunks(&body, "boundary", framing).await;
            assert_eq!(metadata, b"{}");
            assert_eq!(media, payload);
        }
    }

    /// Parses `body` once per frame width, from one byte to the whole body, and returns the
    /// result every width agreed on.
    async fn parse_in_chunks(body: &[u8], boundary: &str, framing: Framing) -> (Vec<u8>, Vec<u8>) {
        let whole = parse_bounded(Full::new(Bytes::copy_from_slice(body)), boundary, framing)
            .await
            .unwrap();
        for width in 1..=body.len() {
            let (sender, receiver) = chunk_body::channel();
            let chunks: Vec<_> = body.chunks(width).map(Bytes::copy_from_slice).collect();
            let producer = tokio::spawn(async move {
                for chunk in chunks {
                    sender.send(Ok(chunk)).await.unwrap();
                }
            });
            assert_eq!(
                parse_bounded(receiver, boundary, framing).await.unwrap(),
                whole,
                "{width}"
            );
            producer.await.unwrap();
        }
        whole
    }

    /// The exact request shape gcloud storage 591.0.0 (apitools) sends for a small upload.
    const GCLOUD_BOUNDARY: &str = "===============5708193607524502531==";
    const GCLOUD_BODY: &[u8] = b"--===============5708193607524502531==\nContent-Type: application/json\nMIME-Version: 1.0\n\n{\"bucket\": \"example\", \"contentType\": \"text/plain\", \"name\": \"hello.txt\"}\n--===============5708193607524502531==\nContent-Type: text/plain\nMIME-Version: 1.0\nContent-Transfer-Encoding: binary\n\nHello from gcloud!\n\n--===============5708193607524502531==--\n";

    #[tokio::test]
    async fn related_uploads_accept_the_gcloud_lf_framing_and_quoted_boundary() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            format!("multipart/related; boundary='{GCLOUD_BOUNDARY}'")
                .parse()
                .unwrap(),
        );
        assert_eq!(multipart_boundary(&headers).unwrap(), GCLOUD_BOUNDARY);
        let mut parser =
            Multipart::related(Full::new(Bytes::from_static(GCLOUD_BODY)), GCLOUD_BOUNDARY);
        parser.metadata().await.unwrap();
        assert_eq!(
            parser.headers(PartKind::Media).await.unwrap().as_deref(),
            Some("text/plain")
        );
        let (metadata, media) =
            parse_in_chunks(GCLOUD_BODY, GCLOUD_BOUNDARY, Framing::Detect).await;
        assert!(metadata.starts_with(b"{\"bucket\"") && metadata.ends_with(b"}"));
        // The LF before the delimiter belongs to the delimiter; the one before it is data.
        assert_eq!(media, b"Hello from gcloud!\n");
        // The strict framing of XML POST Object forms still rejects bare LFs.
        let mut strict =
            Multipart::new(Full::new(Bytes::from_static(GCLOUD_BODY)), GCLOUD_BOUNDARY);
        assert!(strict.metadata().await.is_err());
    }

    fn lf_wire(first_line: &[u8], media: &[u8]) -> Vec<u8> {
        let mut body = first_line.to_vec();
        body.extend_from_slice(b"Content-Type: application/json\n\n{}");
        body.extend_from_slice(b"\n--boundary\nContent-Type: application/octet-stream\n\n");
        body.extend_from_slice(media);
        body.extend_from_slice(b"\n--boundary--\n");
        body
    }

    #[tokio::test]
    async fn related_uploads_read_lf_framing_at_every_chunk_split() {
        let payload = b"\x00\xff\n--boundar\r\n--\n--bound\rx\nend\n";
        // White space after the first boundary and a preamble still select LF framing.
        for first_line in [
            b"--boundary\n".as_slice(),
            b"--boundary \t\n",
            b"preamble\n--boundary\n",
        ] {
            let (metadata, media) =
                parse_in_chunks(&lf_wire(first_line, payload), "boundary", Framing::Detect).await;
            assert_eq!(metadata, b"{}");
            assert_eq!(media, payload);
        }
        // Under LF framing a CR before a delimiter is media, as Go's mime/multipart reads it.
        let (_, media) = parse_in_chunks(
            &lf_wire(b"--boundary\n", b"media\r"),
            "boundary",
            Framing::Detect,
        )
        .await;
        assert_eq!(media, b"media\r");
        for body in [
            // A body never mixes CRLF and LF: the first boundary line fixes the framing.
            b"--boundary\nContent-Type: application/json\r\n\r\n{}\n--boundary\n\nmedia\n--boundary--\n".as_slice(),
            b"--boundary\nContent-Type: application/json\n\n{}\r\n--boundary\r\n\r\nmedia\r\n--boundary--\r\n",
            b"--boundary\r\nContent-Type: application/json\n\n{}\n--boundary\n\nmedia\n--boundary--\n",
            b"--boundary\r\nContent-Type: application/json\r\n\r\n{}\n--boundary\n\nmedia\n--boundary--\n",
            // A lone CR does not end a header line.
            b"--boundary\nContent-Type: application/json\r\r\n\n{}\n--boundary--\n",
            // Missing close delimiter, an unterminated header block and a third part stay invalid.
            b"--boundary\nContent-Type: application/json\n\n{}\n--boundary\n\nmedia\n",
            b"--boundary\nContent-Type: application/json\n",
            b"--boundary\nContent-Type: application/json\n\n{}\n--boundary\n\nmedia\n--boundary\n\nthird\n--boundary--\n",
            b"--boundary\nContent-Type: application/json\n\n{}\n--boundary\n\nmedia\n--boundary-x\n",
            b"--boundary\nContent-Type: application/json\n\n{}\n--boundary\n\nmedia\n--boundary--\r\n",
        ] {
            assert!(
                parse_with(Full::new(Bytes::copy_from_slice(body)), Framing::Detect)
                    .await
                    .is_err(),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[tokio::test]
    async fn related_uploads_keep_lf_boundary_text_in_crlf_media_at_every_chunk_split() {
        // A CRLF body uses CRLF delimiters throughout, so LF plus boundary text is media.
        for payload in [
            b"AAA\n--boundary--\nBBB".as_slice(),
            b"AAA\n--boundaryX",
            b"\n--boundary\n",
            b"AAA\n--boundary\nContent-Type: text/plain\n\nBBB\n--boundary--\n",
        ] {
            let body = wire(b"{}", payload);
            for framing in FRAMINGS {
                let (metadata, media) = parse_in_chunks(&body, "boundary", framing).await;
                assert_eq!(metadata, b"{}");
                assert_eq!(media, payload, "{}", String::from_utf8_lossy(payload));
            }
        }
    }

    #[tokio::test]
    async fn multipart_parser_reads_long_headers_and_padding_in_one_byte_frames() {
        let padding = " ".repeat(HEADER_CAP - 2);
        let header = "Content-Type: application/json\r\nX-Long: ";
        let value = "x".repeat(HEADER_CAP - header.len() - 4);
        let body = format!(
            "--boundary{padding}\r\n{header}{value}\r\n\r\n{{}}\r\n--boundary{padding}\r\n\r\nmedia\r\n--boundary--{padding}\r\n"
        );
        for framing in FRAMINGS {
            let (sender, receiver) = chunk_body::channel();
            let bytes = body.clone().into_bytes();
            let producer = tokio::spawn(async move {
                for byte in bytes {
                    sender.send(Ok(Bytes::from(vec![byte]))).await.unwrap();
                }
            });
            let (metadata, media) = parse_with(receiver, framing).await.unwrap();
            assert_eq!(metadata, b"{}");
            assert_eq!(media, b"media");
            producer.await.unwrap();
        }
        for tail in [b"x\r\n".as_slice(), b"x", b"\r"] {
            let mut body = wire(b"{}", b"media");
            body.truncate(body.len() - 2);
            body.extend_from_slice(padding.as_bytes());
            body.extend_from_slice(tail);
            assert!(parse(Bytes::from(body)).await.is_err());
        }
    }

    #[tokio::test]
    async fn multipart_parser_streams_large_media_with_bounded_carry() {
        for framing in FRAMINGS {
            stream_large_media(framing).await;
        }
    }

    async fn stream_large_media(framing: Framing) {
        let (sender, receiver) = chunk_body::channel();
        let producer = tokio::spawn(async move {
            sender
                .send(Ok(Bytes::from_static(
                    b"--boundary\r\nContent-Type: application/json\r\n\r\n{}\r\n--boundary\r\n\r\n",
                )))
                .await
                .unwrap();
            for _ in 0..256 {
                sender
                    .send(Ok(Bytes::from(vec![b'x'; 64 * 1024])))
                    .await
                    .unwrap();
            }
            sender
                .send(Ok(Bytes::from_static(b"\r\n--boundary--")))
                .await
                .unwrap();
        });
        let mut parser = Multipart::with_framing(receiver, "boundary", framing);
        assert_eq!(parser.metadata().await.unwrap(), b"{}");
        parser.headers(PartKind::Media).await.unwrap();
        let mut size = 0;
        while let Some(chunk) = parser.next_chunk().await.unwrap() {
            assert!(chunk.len() <= HEADER_CAP);
            assert!(parser.buffer.len() <= HEADER_CAP + parser.delimiter.len());
            assert!(chunk.iter().all(|byte| *byte == b'x'));
            size += chunk.len();
        }
        assert_eq!(size, 16 * 1024 * 1024);
        parser.finish().await.unwrap();
        producer.await.unwrap();
    }

    #[test]
    fn multipart_boundary_accepts_quoted_rfc_values_and_rejects_invalid_parameters() {
        for value in [
            "multipart/related; boundary=boundary",
            "Multipart/Related; boundary=\"a b:c?\"; type=\"application/json\"",
            "multipart/related; boundary=\"a\\?b\"",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("content-type", value.parse().unwrap());
            multipart_boundary(&headers).unwrap();
        }
        // gcloud's single-quoted boundary drops its quotes; a single-quoted value that is a
        // valid RFC token keeps them, as the RFC reads it.
        for (value, expected) in [
            (
                "multipart/related; boundary='===============N=='",
                "===============N==",
            ),
            (
                "multipart/related; boundary='a b'; type=\"application/json\"",
                "a b",
            ),
            ("multipart/related; boundary='abc'", "'abc'"),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("content-type", value.parse().unwrap());
            assert_eq!(multipart_boundary(&headers).unwrap(), expected, "{value}");
        }
        for value in [
            "multipart/related; boundary='===unterminated",
            "multipart/related; boundary='==='x",
            "multipart/related; boundary='x '",
            "multipart/related; boundary='a\"b='",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("content-type", value.parse().unwrap());
            assert!(multipart_boundary(&headers).is_err(), "{value}");
        }
        // Only multipart/related uploads accept the single quotes.
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            "multipart/form-data; boundary='==='".parse().unwrap(),
        );
        assert!(form_data_boundary(&headers).is_err());
        for value in [
            "text/plain; boundary=x",
            "multipart/related",
            "multipart/related; boundary=",
            "multipart/related; boundary=\"\"",
            "multipart/related; boundary=\"x \"",
            "multipart/related; boundary=a:b",
            "multipart/related; boundary=\"x;z\"",
            "multipart/related; boundary=x; boundary=y",
            "multipart/related; boundary=\"unterminated",
            "multipart/related; boundary=x;",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("content-type", value.parse().unwrap());
            assert_eq!(
                multipart_boundary(&headers).unwrap_err().status,
                StatusCode::BAD_REQUEST,
                "{value}"
            );
        }
        for length in [70, 71] {
            let mut headers = HeaderMap::new();
            headers.insert(
                "content-type",
                format!("multipart/related; boundary={}", "x".repeat(length))
                    .parse()
                    .unwrap(),
            );
            assert_eq!(multipart_boundary(&headers).is_ok(), length == 70);
        }
    }

    #[test]
    fn form_data_boundary_accepts_utf8_forms_and_rejects_other_types_and_parameters() {
        for value in [
            "multipart/form-data; boundary=----WebKitFormBoundary7MA4YWxkTrZu0gW",
            "Multipart/Form-Data; boundary=\"a b\"; charset=UTF-8",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("content-type", value.parse().unwrap());
            form_data_boundary(&headers).unwrap();
        }
        for value in [
            "multipart/related; boundary=x",
            "multipart/form-data",
            "multipart/form-data; boundary=x; charset=latin1",
            "multipart/form-data; boundary=x; type=application/json",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("content-type", value.parse().unwrap());
            assert!(form_data_boundary(&headers).is_err(), "{value}");
        }
        assert!(form_data_boundary(&HeaderMap::new()).is_err());
    }

    #[tokio::test]
    async fn multipart_parser_bounds_preamble_epilogue_headers_and_metadata() {
        let base = wire(b"{}", b"");
        for length in [DISCARD_CAP, DISCARD_CAP + 1] {
            let mut preamble = vec![b'p'; length];
            preamble.extend_from_slice(b"\r\n");
            preamble.extend_from_slice(&base);
            assert_eq!(
                parse(Bytes::from(preamble)).await.is_ok(),
                length == DISCARD_CAP
            );
            let mut epilogue = base.clone();
            epilogue.extend(std::iter::repeat_n(b'e', length));
            assert_eq!(
                parse(Bytes::from(epilogue)).await.is_ok(),
                length == DISCARD_CAP
            );
        }
        for (length, framing) in [JSON_CAP, JSON_CAP + 1]
            .into_iter()
            .flat_map(|length| FRAMINGS.map(|framing| (length, framing)))
        {
            let mut parser = Multipart::with_framing(
                Full::new(Bytes::from(wire(&vec![b' '; length], b""))),
                "boundary",
                framing,
            );
            let result = parser.metadata().await;
            if length == JSON_CAP {
                assert_eq!(result.unwrap().len(), length);
            } else {
                assert_eq!(result.unwrap_err().status, StatusCode::PAYLOAD_TOO_LARGE);
            }
        }
        let mut body = b"--boundary\r\nX-Long: ".to_vec();
        body.extend(std::iter::repeat_n(b'x', HEADER_CAP));
        body.extend_from_slice(b"\r\n\r\n{}\r\n--boundary--");
        assert!(parse(Bytes::from(body)).await.is_err());
    }

    #[tokio::test]
    async fn multipart_parser_rejects_missing_parts_bad_headers_and_truncation() {
        let valid = wire(b"{}", b"media");
        for length in 0..valid.len() - 4 {
            assert!(
                parse(Bytes::copy_from_slice(&valid[..length]))
                    .await
                    .is_err(),
                "truncation {length}"
            );
        }
        for body in [b"--boundary--".as_slice(), b"--boundary\r\nContent-Type: text/plain\r\n\r\n{}\r\n--boundary--", b"--boundary\r\nBroken\r\n\r\n{}\r\n--boundary--", b"--boundary\r\nContent-Type: application/json\r\nContent-Type: application/json\r\n\r\n{}\r\n--boundary--"] {
            assert!(parse(Bytes::copy_from_slice(body)).await.is_err());
        }
        let third = String::from_utf8(valid.clone())
            .unwrap()
            .replace("--boundary--", "--boundary\r\n\r\nthird\r\n--boundary--");
        assert!(parse(Bytes::from(third)).await.is_err());
        let without_final_crlf = &valid[..valid.len() - 2];
        assert!(
            parse(Bytes::copy_from_slice(without_final_crlf))
                .await
                .is_ok()
        );
    }
}
