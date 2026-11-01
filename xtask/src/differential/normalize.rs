//! Normalization of responses into a comparable, secret-free form. Values that
//! change on every response become placeholders before anything is compared,
//! recorded or written to a report.

use super::case::BodyMode;
use super::transport::Response;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Infrastructure headers that differ per request and carry no API contract.
const DROPPED_HEADERS: [&str; 12] = [
    "date",
    "server",
    "alt-svc",
    "x-guploader-uploadid",
    "x-goog-gcs-idempotency-token",
    "content-length",
    "connection",
    "keep-alive",
    "transfer-encoding",
    "via",
    "x-goog-request-id",
    "x-goog-gcs-base-ts",
];

const TIME_KEYS: [&str; 9] = [
    "timeCreated",
    "updated",
    "timeStorageClassUpdated",
    "softDeleteTime",
    "hardDeleteTime",
    "retentionExpirationTime",
    "timeFinalized",
    "timeDeleted",
    "effectiveTime",
];

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NormBody {
    Empty,
    Json(Value),
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NormResponse {
    pub(crate) status: u16,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: NormBody,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Normalizer {
    literals: Vec<(String, String)>,
}

fn is_time(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b.last() == Some(&b'Z')
        && b[..4].iter().all(u8::is_ascii_digit)
}

/// Replaces `generation=<digits>` so URLs and ids compare by shape.
fn scrub_generation(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("generation=") {
        let (head, tail) = rest.split_at(at + "generation=".len());
        out.push_str(head);
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        if digits >= 10 {
            out.push_str("<generation>");
            rest = &tail[digits..];
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

impl Normalizer {
    /// Literals replace concrete names (buckets, project, hosts); longer
    /// literals first so a prefix never shadows the full value.
    pub(crate) fn add_literal(&mut self, value: &str, placeholder: &str) {
        if value.len() < 3 || self.literals.iter().any(|(v, _)| v == value) {
            return;
        }
        self.literals
            .push((value.to_owned(), placeholder.to_owned()));
        self.literals
            .sort_by_key(|(v, _)| std::cmp::Reverse(v.len()));
    }

    pub(crate) fn text(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for (value, placeholder) in &self.literals {
            out = out.replace(value.as_str(), placeholder);
        }
        scrub_generation(&out)
    }

    pub(crate) fn json(&self, value: &Value) -> Value {
        match value {
            Value::String(text) if is_time(text) => Value::String("<time>".into()),
            Value::String(text) => Value::String(self.text(text)),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.json(v)).collect()),
            Value::Object(map) => Value::Object(
                map.iter()
                    .map(|(key, item)| (key.clone(), self.field(key, item)))
                    .collect::<Map<_, _>>(),
            ),
            other => other.clone(),
        }
    }

    fn field(&self, key: &str, item: &Value) -> Value {
        let scalar = matches!(item, Value::String(_) | Value::Number(_));
        match key {
            _ if TIME_KEYS.contains(&key) && scalar => Value::String("<time>".into()),
            "etag" if scalar => Value::String("<etag>".into()),
            "generation" if scalar => Value::String("<generation>".into()),
            "projectNumber" if scalar => Value::String("<project-number>".into()),
            "id" => match item.as_str() {
                // Object ids end with the generation.
                Some(id) => {
                    let text = self.text(id);
                    match text.rsplit_once('/') {
                        Some((head, tail))
                            if tail.len() >= 10 && tail.bytes().all(|b| b.is_ascii_digit()) =>
                        {
                            Value::String(format!("{head}/<generation>"))
                        }
                        _ => Value::String(text),
                    }
                }
                None => self.json(item),
            },
            _ => self.json(item),
        }
    }

    fn header_value(&self, name: &str, value: &str) -> String {
        match name {
            "etag" => "<etag>".into(),
            "x-goog-generation" => "<generation>".into(),
            "last-modified" => "<time>".into(),
            // Only the fixed 1990 value is part of the contract; a current date varies.
            "expires" if value.ends_with(" GMT") && !value.contains("1990") => "<time>".into(),
            _ => self.text(value),
        }
    }

    pub(crate) fn response(&self, response: &Response) -> NormResponse {
        let mut headers = BTreeMap::new();
        for (name, value) in &response.headers {
            let name = name.to_ascii_lowercase();
            if DROPPED_HEADERS.contains(&name.as_str()) {
                continue;
            }
            let value = self.header_value(&name, value);
            headers
                .entry(name)
                .and_modify(|joined: &mut String| {
                    joined.push_str(", ");
                    joined.push_str(&value);
                })
                .or_insert(value);
        }
        let is_json = headers
            .get("content-type")
            .is_some_and(|t| t.contains("json"));
        let body = if response.body.is_empty() {
            NormBody::Empty
        } else if let (true, Ok(value)) = (is_json, serde_json::from_slice(&response.body)) {
            NormBody::Json(self.json(&value))
        } else {
            NormBody::Text(self.text(&String::from_utf8_lossy(&response.body)))
        };
        NormResponse {
            status: response.status,
            headers,
            body,
        }
    }
}

/// Finds every `projectNumber` value so it can be replaced on later responses.
pub(crate) fn project_numbers(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                match (key.as_str(), item) {
                    ("projectNumber", Value::String(n)) => found.push(n.clone()),
                    ("projectNumber", Value::Number(n)) => found.push(n.to_string()),
                    _ => project_numbers(item, found),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|i| project_numbers(i, found)),
        _ => {}
    }
}

/// Resolves a body to the value that is compared, honouring the step's mode.
pub(crate) fn body_value(body: &NormBody, mode: BodyMode) -> Option<Value> {
    match (body, mode) {
        (_, BodyMode::Ignore) | (NormBody::Empty, _) => None,
        (NormBody::Json(value), BodyMode::Text) => Some(Value::String(value.to_string())),
        (NormBody::Json(value), _) => Some(value.clone()),
        (NormBody::Text(text), BodyMode::Json) => {
            Some(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone())))
        }
        (NormBody::Text(text), _) => Some(Value::String(text.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn normalizer() -> Normalizer {
        let mut n = Normalizer::default();
        n.add_literal("gcsmimic-diff-abc-b", "<bucket>");
        n.add_literal("my-project", "<project>");
        n.add_literal("https://storage.googleapis.com", "<host>");
        n.add_literal("123456789", "<project-number>");
        n
    }

    #[test]
    fn replaces_volatile_fields_and_literals() {
        let input = json!({
            "id": "gcsmimic-diff-abc-b/o.txt/1730000000000001",
            "bucket": "gcsmimic-diff-abc-b",
            "generation": "1730000000000001",
            "metageneration": "1",
            "etag": "CAE=",
            "timeCreated": "2026-11-01T00:00:00.123Z",
            "mediaLink": "https://storage.googleapis.com/dl/b/gcsmimic-diff-abc-b/o/x?generation=1730000000000001&alt=media",
            "acl": [{"entity": "project-owners-123456789", "etag": "CAE="}],
            "message": "project my-project missing",
        });
        let out = normalizer().json(&input);
        assert_eq!(out["id"], "<bucket>/o.txt/<generation>");
        assert_eq!(out["generation"], "<generation>");
        assert_eq!(out["metageneration"], "1");
        assert_eq!(out["etag"], "<etag>");
        assert_eq!(out["timeCreated"], "<time>");
        assert_eq!(
            out["mediaLink"],
            "<host>/dl/b/<bucket>/o/x?generation=<generation>&alt=media"
        );
        assert_eq!(out["acl"][0]["entity"], "project-owners-<project-number>");
        assert_eq!(out["message"], "project <project> missing");
    }

    #[test]
    fn normalizes_headers_and_drops_infrastructure() {
        let response = Response {
            status: 200,
            headers: vec![
                ("Date".into(), "x".into()),
                ("X-GUploader-UploadID".into(), "u".into()),
                ("ETag".into(), "CAE=".into()),
                ("Vary".into(), "Origin".into()),
                ("vary".into(), "X-Origin".into()),
                (
                    "Content-Type".into(),
                    "application/json; charset=UTF-8".into(),
                ),
                ("X-Goog-Generation".into(), "1730000000000001".into()),
                ("Expires".into(), "Sun, 01 Nov 2026 00:17:44 GMT".into()),
                ("X-Goog-GCS-Base-TS".into(), "1a19".into()),
            ],
            body: br#"{"name":"gcsmimic-diff-abc-b"}"#.to_vec(),
        };
        let out = normalizer().response(&response);
        assert!(!out.headers.contains_key("date"));
        assert!(!out.headers.contains_key("x-guploader-uploadid"));
        assert_eq!(out.headers["etag"], "<etag>");
        assert_eq!(out.headers["vary"], "Origin, X-Origin");
        assert_eq!(out.headers["x-goog-generation"], "<generation>");
        assert_eq!(out.headers["expires"], "<time>");
        assert!(!out.headers.contains_key("x-goog-gcs-base-ts"));
        assert_eq!(out.body, NormBody::Json(json!({"name": "<bucket>"})));
    }

    #[test]
    fn short_numbers_are_not_mistaken_for_generations() {
        assert_eq!(scrub_generation("generation=42&x"), "generation=42&x");
        assert_eq!(
            scrub_generation("generation=1730000000000001"),
            "generation=<generation>"
        );
    }

    #[test]
    fn collects_project_numbers_and_resolves_body_modes() {
        let mut found = Vec::new();
        project_numbers(&json!({"items": [{"projectNumber": "42"}]}), &mut found);
        assert_eq!(found, ["42"]);
        let body = NormBody::Json(json!({"a": 1}));
        assert_eq!(body_value(&body, BodyMode::Ignore), None);
        assert_eq!(body_value(&body, BodyMode::Text), Some(json!("{\"a\":1}")));
        assert_eq!(
            body_value(&NormBody::Text("{\"a\":1}".into()), BodyMode::Json),
            Some(json!({"a": 1}))
        );
    }
}
