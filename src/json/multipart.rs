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

// Quoted parameter values may contain semicolons and escaped quotes.
fn content_type(value: &str) -> Result<(String, Vec<(String, String)>), JsonError> {
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
    let (mime, params) = content_type(value)?;
    if mime != "multipart/related" {
        return Err(invalid());
    }
    let boundary = boundary(&params)?;
    // RFC 2387 permits type and start; reordered roots are outside this ordered subset.
    for (key, value) in &params {
        if key != "boundary" && !(key == "type" && value.eq_ignore_ascii_case("application/json")) {
            return Err(JsonError::unsupported_object(key));
        }
    }
    Ok(boundary)
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

pub(crate) struct Multipart<B> {
    body: B,
    pending: Bytes,
    buffer: BytesMut,
    delimiter: Vec<u8>,
    eof: bool,
    closed: bool,
}

impl<B: Body<Data = Bytes> + Unpin> Multipart<B> {
    pub(crate) fn new(body: B, boundary: &str) -> Self {
        Self {
            body,
            pending: Bytes::new(),
            // A synthetic CRLF lets the first boundary use the same search as later boundaries.
            buffer: BytesMut::from(&b"\r\n"[..]),
            delimiter: format!("\r\n--{boundary}").into_bytes(),
            eof: false,
            closed: false,
        }
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
            if let Some(end) = self.buffer[scanned..].windows(2).position(|v| v == b"\r\n") {
                let end = scanned + end;
                if end > cap {
                    return Err(invalid());
                }
                let line = self.buffer.split_to(end).to_vec();
                self.buffer.advance(2);
                return Ok(line);
            }
            // A trailing CR may become the line terminator in the next frame.
            if self.buffer.len() > cap
                && (self.eof || self.buffer.len() != cap + 1 || !self.buffer.ends_with(b"\r"))
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
            if let Some(index) = self
                .buffer
                .windows(self.delimiter.len())
                .position(|v| v == self.delimiter)
            {
                discarded += index;
                if discarded > DISCARD_CAP + 2 {
                    return Err(invalid());
                }
                self.buffer.advance(index);
                self.delimiter().await?;
                return if self.closed { Err(invalid()) } else { Ok(()) };
            }
            let discard = self.buffer.len().saturating_sub(self.delimiter.len() - 1);
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
            size += line.len() + 2;
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
            if let Some(index) = self
                .buffer
                .windows(self.delimiter.len())
                .position(|v| v == self.delimiter)
            {
                if index > 0 {
                    return Ok(Some(self.buffer.split_to(index).freeze()));
                }
                self.delimiter().await?;
                return Ok(None);
            }
            let length = self.buffer.len().saturating_sub(self.delimiter.len() - 1);
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
        if self.closed {
            return Err(invalid());
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
    } = parse_object_metadata(bytes, super::WriteMode::Patch)?;
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
            .ok_or_else(|| {
                JsonError::new(StatusCode::BAD_REQUEST, "required", "Missing required name")
            })?
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

    async fn parse<B: Body<Data = Bytes> + Unpin>(
        body: B,
    ) -> Result<(Vec<u8>, Vec<u8>), JsonError> {
        let mut parser = Multipart::new(body, "boundary");
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
        for width in 1..=body.len() {
            let (sender, receiver) = chunk_body::channel();
            let chunks: Vec<_> = body.chunks(width).map(Bytes::copy_from_slice).collect();
            let producer = tokio::spawn(async move {
                for chunk in chunks {
                    sender.send(Ok(chunk)).await.unwrap();
                }
            });
            let (metadata, media) = parse(receiver).await.unwrap();
            assert_eq!(metadata, b"{}");
            assert_eq!(media, payload);
            producer.await.unwrap();
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
        let (sender, receiver) = chunk_body::channel();
        let producer = tokio::spawn(async move {
            for byte in body.into_bytes() {
                sender.send(Ok(Bytes::from(vec![byte]))).await.unwrap();
            }
        });
        let (metadata, media) = parse(receiver).await.unwrap();
        assert_eq!(metadata, b"{}");
        assert_eq!(media, b"media");
        producer.await.unwrap();
        for tail in [b"x\r\n".as_slice(), b"x", b"\r"] {
            let mut body = wire(b"{}", b"media");
            body.truncate(body.len() - 2);
            body.extend_from_slice(padding.as_bytes());
            body.extend_from_slice(tail);
            assert!(parse(Full::new(Bytes::from(body))).await.is_err());
        }
    }

    #[tokio::test]
    async fn multipart_parser_streams_large_media_with_bounded_carry() {
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
        let mut parser = Multipart::new(receiver, "boundary");
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
                parse(Full::new(Bytes::from(preamble))).await.is_ok(),
                length == DISCARD_CAP
            );
            let mut epilogue = base.clone();
            epilogue.extend(std::iter::repeat_n(b'e', length));
            assert_eq!(
                parse(Full::new(Bytes::from(epilogue))).await.is_ok(),
                length == DISCARD_CAP
            );
        }
        for length in [JSON_CAP, JSON_CAP + 1] {
            let mut parser = Multipart::new(
                Full::new(Bytes::from(wire(&vec![b' '; length], b""))),
                "boundary",
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
        assert!(parse(Full::new(Bytes::from(body))).await.is_err());
    }

    #[tokio::test]
    async fn multipart_parser_rejects_missing_parts_bad_headers_and_truncation() {
        let valid = wire(b"{}", b"media");
        for length in 0..valid.len() - 4 {
            assert!(
                parse(Full::new(Bytes::copy_from_slice(&valid[..length])))
                    .await
                    .is_err(),
                "truncation {length}"
            );
        }
        for body in [b"--boundary--".as_slice(), b"--boundary\r\nContent-Type: text/plain\r\n\r\n{}\r\n--boundary--", b"--boundary\r\nBroken\r\n\r\n{}\r\n--boundary--", b"--boundary\r\nContent-Type: application/json\r\nContent-Type: application/json\r\n\r\n{}\r\n--boundary--"] {
            assert!(parse(Full::new(Bytes::copy_from_slice(body))).await.is_err());
        }
        let third = String::from_utf8(valid.clone())
            .unwrap()
            .replace("--boundary--", "--boundary\r\n\r\nthird\r\n--boundary--");
        assert!(parse(Full::new(Bytes::from(third))).await.is_err());
        let without_final_crlf = &valid[..valid.len() - 2];
        assert!(
            parse(Full::new(Bytes::copy_from_slice(without_final_crlf)))
                .await
                .is_ok()
        );
    }
}
