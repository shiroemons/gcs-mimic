//! Normalization of responses into a comparable, secret-free form. Values that
//! change on every response become placeholders before anything is compared,
//! recorded or written to a report.

use super::base64;
use super::case::BodyMode;
use super::transport::Response;
use super::xml;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Infrastructure headers that differ per request and carry no API contract.
const DROPPED_HEADERS: [&str; 10] = [
    "date",
    "server",
    "alt-svc",
    "x-guploader-uploadid",
    "x-goog-gcs-idempotency-token",
    "connection",
    "keep-alive",
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

/// An RFC 3339 UTC timestamp. Returns the number of fractional-second digits
/// so a precision difference (`.123Z` against `.123456Z`) stays visible.
fn time_fraction_digits(text: &str) -> Option<usize> {
    let b = text.as_bytes();
    let shaped = b.len() >= 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b.last() == Some(&b'Z')
        && b[..4].iter().all(u8::is_ascii_digit);
    if !shaped {
        return None;
    }
    match b[19] {
        b'Z' if b.len() == 20 => Some(0),
        b'.' if b[20..b.len() - 1].iter().all(u8::is_ascii_digit) => Some(b.len() - 21),
        _ => None,
    }
}

/// `<time:f3>` for a string with three fractional digits, `<time:str>` for a
/// string that is no timestamp, `<time:num>` for a number: the value changes on
/// every run but its type and precision are part of the contract.
fn time_placeholder(item: &Value) -> String {
    match item {
        Value::Number(_) => "<time:num>".into(),
        Value::String(text) => match time_fraction_digits(text) {
            Some(digits) => format!("<time:f{digits}>"),
            None => "<time:str>".into(),
        },
        _ => "<time:other>".into(),
    }
}

/// The digit count a [`time_placeholder`] names, such as 3 for `<time:f3>`.
pub(crate) fn time_placeholder_digits(placeholder: &str) -> Option<usize> {
    placeholder
        .strip_prefix("<time:f")?
        .strip_suffix('>')?
        .parse()
        .ok()
}

/// `<name:str:16>` / `<name:num:16>`: the digit count of a numeric value is
/// kept, because GCS and a local emulator may differ in width.
fn shaped_placeholder(name: &str, item: &Value) -> String {
    match item {
        Value::Number(number) => format!("<{name}:num:{}>", number.to_string().len()),
        Value::String(text) => format!("<{name}:str:{}>", text.chars().count()),
        _ => format!("<{name}:other>"),
    }
}

/// Replaces the number in `project-owners-0` style entities. The emulator has no
/// project registry and renders `0`; a recorded real number is already
/// `<project-number>`, so only the team entity position is rewritten.
fn scrub_project_team_number(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("project-") {
        let (head, tail) = rest.split_at(at + "project-".len());
        out.push_str(head);
        rest = tail;
        for team in ["owners-", "editors-", "viewers-"] {
            if let Some(after) = tail.strip_prefix(team) {
                let digits = after.bytes().take_while(u8::is_ascii_digit).count();
                if &after[..digits] == "0" {
                    out.push_str(team);
                    out.push_str("<project-number>");
                    rest = &after[digits..];
                }
                break;
            }
        }
    }
    out.push_str(rest);
    out
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
            out.push_str(&format!("<generation:{digits}>"));
            rest = &tail[digits..];
        } else {
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

/// Replaces the value of every `upload_id=` / `uploadId=` query parameter: a
/// resumable session URL is a bearer credential for that session.
fn scrub_upload_ids(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    loop {
        let found = ["upload_id=", "uploadId="]
            .into_iter()
            .filter_map(|key| rest.find(key).map(|at| (at, key)))
            .min_by_key(|(at, _)| *at);
        let Some((at, key)) = found else { break };
        let (head, tail) = rest.split_at(at + key.len());
        out.push_str(head);
        // Already a placeholder (a captured session ID): keep it.
        if tail.starts_with('<') {
            rest = tail;
            continue;
        }
        let end = tail
            .find(|c: char| matches!(c, '&' | '"' | '\'' | '<' | '>') || c.is_whitespace())
            .unwrap_or(tail.len());
        out.push_str("<upload-id>");
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// An entity tag replaced by its quoting style: whether the quotes are there
/// (or a weak `W/` prefix) is part of the contract, the digest is not.
fn etag_shape(text: &str) -> &'static str {
    if text.starts_with("W/") {
        "<etag:weak>"
    } else if text.starts_with('"') {
        "<etag:quoted>"
    } else {
        "<etag:bare>"
    }
}

fn is_email_local(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_email_domain(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')
}

/// Whether the text holds something shaped like an email address.
pub(crate) fn contains_email(text: &str) -> bool {
    scrub_emails(text) != text
}

/// Replaces email addresses (service accounts, callers in ACL entries) with
/// `<email>`; encoded separators retain their shape so URL encoding differences
/// remain visible without exposing a caller's address.
pub(crate) fn scrub_emails(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        let (separator_len, placeholder) = if bytes[at] == b'@' {
            (1, "<email>")
        } else if bytes[at..].starts_with(b"%40") {
            (3, "<email:percent-encoded>")
        } else if bytes[at..].starts_with(b"%2540") {
            (5, "<email:double-percent-encoded>")
        } else {
            at += 1;
            continue;
        };
        let mut start = at;
        while start > copied && is_email_local(bytes[start - 1]) {
            start -= 1;
        }
        // Keep the entity kind of `user-a@b.c` and `serviceAccount:a@b.c`.
        for kind in [
            "user-",
            "group-",
            "user:",
            "group:",
            "serviceaccount:",
            "domain-",
        ] {
            let local = &text[start..at];
            if local.len() > kind.len() && local[..kind.len()].eq_ignore_ascii_case(kind) {
                start += kind.len();
                break;
            }
        }
        let domain_start = at + separator_len;
        let mut end = domain_start;
        while end < bytes.len() && is_email_domain(bytes[end]) {
            end += 1;
        }
        while end > domain_start && matches!(bytes[end - 1], b'.' | b'-') {
            end -= 1;
        }
        let domain = &text[domain_start..end];
        if start < at && domain.contains('.') && !domain.starts_with('.') {
            out.push_str(&text[copied..start]);
            // Live project-number substitution already preserves this service-agent shape.
            out.push_str(
                if &text[start..end] == "service-0@gs-project-accounts.iam.gserviceaccount.com" {
                    "service-<project-number>@gs-project-accounts.iam.gserviceaccount.com"
                } else {
                    placeholder
                },
            );
            copied = end;
            at = end;
        } else {
            at += 1;
        }
    }
    out.push_str(&text[copied..]);
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
        scrub_emails(&scrub_upload_ids(&scrub_generation(
            &scrub_project_team_number(&out),
        )))
    }

    pub(crate) fn json(&self, value: &Value) -> Value {
        match value {
            Value::String(text) if time_fraction_digits(text).is_some() => {
                Value::String(time_placeholder(value))
            }
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
            _ if TIME_KEYS.contains(&key) && scalar => Value::String(time_placeholder(item)),
            // The type is kept: a string and a number are different JSON.
            "etag" if scalar => Value::String(match item {
                Value::Number(_) => "<etag:num>".into(),
                _ => "<etag:str>".into(),
            }),
            "generation" if scalar => Value::String(shaped_placeholder("generation", item)),
            "projectNumber" if scalar => Value::String(match item {
                Value::Number(_) => "<project-number:num>".into(),
                _ => "<project-number:str>".into(),
            }),
            // ACL entries name the caller by an opaque ID next to the email.
            "entityId" if scalar => Value::String("<entity-id>".into()),
            // Only whether a next page exists is part of the contract.
            "nextPageToken" | "next_page_token" if scalar => Value::String("<page-token>".into()),
            // An HMAC key secret must never reach a recording.
            "secret" if scalar => Value::String("<secret>".into()),
            // gRPC buckets name their project as `projects/<number>`.
            "project" if item.as_str().and_then(project_resource_number).is_some() => {
                Value::String("projects/<project-number>".into())
            }
            "id" => match item.as_str() {
                // Object and ACL ids carry the generation as a path segment
                // (`bucket/name/<generation>` and `.../<generation>/entity`).
                Some(id) => Value::String(
                    self.text(id)
                        .split('/')
                        .map(|segment| {
                            if segment.len() >= 10 && segment.bytes().all(|b| b.is_ascii_digit()) {
                                format!("<generation:{}>", segment.len())
                            } else {
                                segment.to_owned()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("/"),
                ),
                None => self.json(item),
            },
            _ => self.json(item),
        }
    }

    fn xml_text(&self, tag: &str, text: &str) -> String {
        match tag {
            "ETag" if !text.is_empty() => etag_shape(text).into(),
            "Generation" if !text.is_empty() => format!("<generation:{}>", text.len()),
            "LastModified" | "Created" | "Updated" if !text.is_empty() => {
                time_placeholder(&Value::String(text.to_owned()))
            }
            // The owner's opaque ID and name identify the caller.
            "ID" | "DisplayName" if !text.is_empty() => "<owner>".into(),
            "UploadId" if !text.is_empty() => "<upload-id>".into(),
            _ if time_fraction_digits(text).is_some() => {
                time_placeholder(&Value::String(text.to_owned()))
            }
            _ => self.text(text),
        }
    }

    /// Normalizes an XML tree from `xml::parse`: leaf text is replaced by tag
    /// (`ETag`, `Generation`, timestamps) and otherwise like any other string.
    fn xml(&self, node: &Value) -> Value {
        let Value::Object(map) = node else {
            return node.clone();
        };
        let tag = map.get("name").and_then(Value::as_str).unwrap_or_default();
        let mut out = Map::new();
        for (key, item) in map {
            let value = match (key.as_str(), item) {
                ("text", Value::String(text)) => Value::String(self.xml_text(tag, text)),
                ("children", Value::Array(items)) => {
                    Value::Array(items.iter().map(|child| self.xml(child)).collect())
                }
                ("attrs", Value::Object(attrs)) => Value::Object(
                    attrs
                        .iter()
                        .map(|(k, v)| (k.clone(), self.json(v)))
                        .collect(),
                ),
                _ => item.clone(),
            };
            out.insert(key.clone(), value);
        }
        Value::Object(out)
    }

    fn header_value(&self, name: &str, value: &str) -> String {
        match name {
            "etag" => etag_shape(value).into(),
            "x-goog-generation" | "x-goog-copy-source-generation" => {
                format!("<generation:{}>", value.len())
            }
            "last-modified" => "<time:http>".into(),
            // Only the fixed 1990 value is part of the contract; a current date varies.
            "expires" if value.ends_with(" GMT") && !value.contains("1990") => "<time:http>".into(),
            _ => self.text(value),
        }
    }

    /// The response body's framing, compared as a shape: `content-length` must
    /// agree with the bytes actually received (a lying length is a bug in the
    /// service), and a chunked response is a different framing.
    fn framing(response: &Response, head: bool) -> Option<String> {
        if let Some(encoding) = response.header("transfer-encoding") {
            return Some(encoding.to_ascii_lowercase());
        }
        let Some(length) = response.header("content-length") else {
            // hyper never sends a length on `204`/`304`, while Cloud Storage sends
            // `content-length: 0`. Both are an empty body with no framing difference, so an
            // empty reply of that kind is framed as an agreeing length.
            return (matches!(response.status, 204 | 304) && response.body.is_empty())
                .then(|| "content-length:<ok>".into());
        };
        Some(if head {
            "content-length:<head>".into()
        } else {
            match length.trim().parse::<usize>() {
                Ok(declared) if declared == response.body.len() => "content-length:<ok>".into(),
                Ok(declared) => format!(
                    "content-length:<mismatch {declared} vs {}>",
                    response.body.len()
                ),
                Err(_) => "content-length:<invalid>".into(),
            }
        })
    }

    /// `head` is whether the request was HEAD, whose `content-length` describes
    /// a body that is not sent.
    pub(crate) fn response(&self, response: &Response, head: bool) -> NormResponse {
        let mut headers = BTreeMap::new();
        for (name, value) in &response.headers {
            let name = name.to_ascii_lowercase();
            if DROPPED_HEADERS.contains(&name.as_str())
                || matches!(name.as_str(), "content-length" | "transfer-encoding")
            {
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
        if let Some(framing) = Self::framing(response, head) {
            headers.insert("x-harness-body-framing".into(), framing);
        }
        let is_json = headers
            .get("content-type")
            .is_some_and(|t| t.contains("json"));
        let is_xml = headers
            .get("content-type")
            .is_some_and(|t| t.contains("xml"));
        let xml_tree = if is_xml {
            xml::parse(&String::from_utf8_lossy(&response.body)).ok()
        } else {
            None
        };
        let body = if response.body.is_empty() {
            NormBody::Empty
        } else if let (true, Ok(value)) = (is_json, serde_json::from_slice(&response.body)) {
            NormBody::Json(self.json(&value))
        } else if let Some(tree) = xml_tree {
            NormBody::Json(self.xml(&tree))
        } else {
            match std::str::from_utf8(&response.body) {
                Ok(text) => NormBody::Text(self.text(text)),
                // Bytes that are no text (a gzip download) are compared exactly.
                Err(_) => NormBody::Text(format!(
                    "<binary base64:{}>",
                    base64::encode(&response.body)
                )),
            }
        };
        NormResponse {
            status: response.status,
            headers,
            body,
        }
    }
}

/// The number in a `projects/<digits>` resource name.
fn project_resource_number(text: &str) -> Option<&str> {
    text.strip_prefix("projects/")
        .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Finds every `projectNumber` value so it can be replaced on later responses.
pub(crate) fn project_numbers(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                match (key.as_str(), item) {
                    ("project", Value::String(p)) if project_resource_number(p).is_some() => {
                        found.extend(project_resource_number(p).map(str::to_owned));
                    }
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
        assert_eq!(out["id"], "<bucket>/o.txt/<generation:16>");
        let acl = normalizer()
            .json(&json!({"id": "gcsmimic-diff-abc-b/o.txt/1730000000000001/project-owners-0"}));
        assert_eq!(
            acl["id"],
            "<bucket>/o.txt/<generation:16>/project-owners-<project-number>"
        );
        assert_eq!(out["generation"], "<generation:str:16>");
        assert_eq!(out["metageneration"], "1");
        assert_eq!(out["etag"], "<etag:str>");
        assert_eq!(out["timeCreated"], "<time:f3>");
        assert_eq!(
            out["mediaLink"],
            "<host>/dl/b/<bucket>/o/x?generation=<generation:16>&alt=media"
        );
        assert_eq!(out["acl"][0]["entity"], "project-owners-<project-number>");
        assert_eq!(out["message"], "project <project> missing");
    }

    /// The comparison reads back the digits this module writes, so the two must agree.
    #[test]
    fn a_time_placeholder_reads_back_its_digit_count() {
        for (time, digits) in [
            ("2026-11-01T00:00:00Z", 0),
            ("2026-11-01T00:00:00.123Z", 3),
            ("2026-11-01T00:00:00.123456Z", 6),
        ] {
            assert_eq!(
                time_placeholder_digits(&time_placeholder(&json!(time))),
                Some(digits)
            );
        }
        assert_eq!(time_placeholder_digits(&time_placeholder(&json!(1))), None);
        assert_eq!(time_placeholder_digits("<time:str>"), None);
    }

    #[test]
    fn placeholders_keep_the_type_width_and_time_precision() {
        let n = normalizer();
        let out = n.json(&json!({
            "generation": 1_730_000_000_000_001_u64,
            "etag": 7,
            "projectNumber": 123_456_789,
            "updated": "2026-11-01T00:00:00Z",
            "timeCreated": "2026-11-01T00:00:00.123456Z",
            "softDeleteTime": 1730000000,
            "timeDeleted": "not a time",
        }));
        assert_eq!(out["generation"], "<generation:num:16>");
        assert_eq!(out["etag"], "<etag:num>");
        assert_eq!(out["projectNumber"], "<project-number:num>");
        assert_eq!(out["updated"], "<time:f0>");
        assert_eq!(out["timeCreated"], "<time:f6>");
        assert_eq!(out["softDeleteTime"], "<time:num>");
        assert_eq!(out["timeDeleted"], "<time:str>");
        let string = n.json(&json!({"generation": "17300000000000011", "projectNumber": "1"}));
        assert_eq!(string["generation"], "<generation:str:17>");
        assert_eq!(string["projectNumber"], "<project-number:str>");
        assert_ne!(
            n.json(&json!({"generation": "1"})),
            n.json(&json!({"generation": 1}))
        );
    }

    #[test]
    fn encoded_emails_are_secret_but_keep_the_url_encoding_difference() {
        let encoded = "<host>/storage/v1/b/b/o/o/acl/user-local-user%40gcs-mimic.invalid";
        let raw = "<host>/storage/v1/b/b/o/o/acl/user-caller@example.com";
        assert!(contains_email(encoded));
        assert_eq!(
            scrub_emails(encoded),
            "<host>/storage/v1/b/b/o/o/acl/user-<email:percent-encoded>"
        );
        assert_eq!(
            scrub_emails(raw),
            "<host>/storage/v1/b/b/o/o/acl/user-<email>"
        );
        assert_ne!(scrub_emails(encoded), scrub_emails(raw));
        assert!(!contains_email(&scrub_emails(encoded)));
        assert_eq!(
            scrub_emails("user-a%40example.com a@b.org"),
            "user-<email:percent-encoded> <email>"
        );
        let double_encoded = encoded.replace("%40", "%2540");
        assert!(contains_email(&double_encoded));
        let scrubbed = scrub_emails(&double_encoded);
        assert_eq!(
            scrubbed,
            "<host>/storage/v1/b/b/o/o/acl/user-<email:double-percent-encoded>"
        );
        assert_ne!(scrubbed, scrub_emails(encoded));
        assert!(!contains_email(&scrubbed));
    }

    #[test]
    fn local_service_agent_matches_the_recorded_project_number_placeholder() {
        let n = normalizer();
        let shape = "service-<project-number>@gs-project-accounts.iam.gserviceaccount.com";
        assert_eq!(
            n.text("service-0@gs-project-accounts.iam.gserviceaccount.com"),
            shape
        );
        assert_eq!(
            n.text("service-123456789@gs-project-accounts.iam.gserviceaccount.com"),
            shape
        );
        assert_eq!(n.text("service-0@example.com"), "<email>");
        assert_eq!(
            n.text("service-0@gs-project-accounts.iam.gserviceaccount.com.evil"),
            "<email>"
        );
        assert_eq!(
            n.text("another-service-0@gs-project-accounts.iam.gserviceaccount.com"),
            "<email>"
        );
        assert_eq!(n.text("service-0"), "service-0");
    }

    #[test]
    fn hides_emails_entity_ids_and_upload_ids() {
        let n = normalizer();
        let out = n.json(&json!({
            "entity": "user-someone@example.com",
            "email": "svc@my-proj.iam.gserviceaccount.com",
            "entityId": "00b4903a97abcdef",
            "member": "serviceAccount:a.b@c.org",
        }));
        assert_eq!(out["entity"], "user-<email>");
        assert_eq!(out["email"], "<email>");
        assert_eq!(out["entityId"], "<entity-id>");
        assert_eq!(out["member"], "serviceAccount:<email>");
        assert!(!contains_email(&n.text("a@b and x@ and @y.z")));
        assert_eq!(n.text("not an email @ here"), "not an email @ here");
        assert_eq!(
            n.text("https://h/upload?uploadType=resumable&upload_id=ABCdef-123&x=1"),
            "https://h/upload?uploadType=resumable&upload_id=<upload-id>&x=1"
        );
    }

    #[test]
    fn emulator_project_number_is_hidden_only_in_team_entities() {
        let n = normalizer();
        let out = n.json(&json!({
            "entity": "project-owners-0",
            "id": "b/project-viewers-0",
            "selfLink": "<host>/b/x/acl/project-editors-0",
            "name": "obj-0",
            "other": "project-owners-05 and project-owners-",
        }));
        assert_eq!(out["entity"], "project-owners-<project-number>");
        assert_eq!(out["id"], "b/project-viewers-<project-number>");
        assert_eq!(
            out["selfLink"],
            "<host>/b/x/acl/project-editors-<project-number>"
        );
        assert_eq!(out["name"], "obj-0");
        assert_eq!(out["other"], "project-owners-05 and project-owners-");
    }

    #[test]
    fn xml_owner_and_upload_ids_are_hidden() {
        let xml = "<?xml version='1.0'?><ListBucketResult><Contents><Key>k</Key>\
            <Owner><ID>abc123</ID><DisplayName>Someone</DisplayName></Owner></Contents>\
            <InitiateMultipartUploadResult><UploadId>U1</UploadId></InitiateMultipartUploadResult>\
            </ListBucketResult>";
        let response = Response {
            status: 200,
            headers: vec![("Content-Type".into(), "application/xml".into())],
            body: xml.as_bytes().to_vec(),
        };
        let NormBody::Json(tree) = normalizer().response(&response, false).body else {
            panic!("xml body should become a tree");
        };
        let text = tree.to_string();
        assert!(!text.contains("abc123") && !text.contains("Someone") && !text.contains("U1\""));
        assert!(text.contains("<owner>") && text.contains("<upload-id>"));
    }

    #[test]
    fn content_length_is_compared_as_agreement_with_the_body() {
        let n = normalizer();
        let mut response = Response {
            status: 200,
            headers: vec![("Content-Length".into(), "5".into())],
            body: b"hello".to_vec(),
        };
        let framing =
            |r: &Response, head| n.response(r, head).headers["x-harness-body-framing"].clone();
        assert_eq!(framing(&response, false), "content-length:<ok>");
        response.body = b"hell".to_vec();
        assert_eq!(
            framing(&response, false),
            "content-length:<mismatch 5 vs 4>"
        );
        response.body.clear();
        assert_eq!(framing(&response, true), "content-length:<head>");
        response.headers = vec![("Transfer-Encoding".into(), "Chunked".into())];
        assert_eq!(framing(&response, false), "chunked");
        response.headers.clear();
        response.status = 204;
        response.body.clear();
        assert_eq!(framing(&response, false), "content-length:<ok>");
        response.status = 200;
        assert!(
            !n.response(&response, false)
                .headers
                .contains_key("x-harness-body-framing")
        );
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
        let out = normalizer().response(&response, false);
        assert!(!out.headers.contains_key("date"));
        assert!(!out.headers.contains_key("x-guploader-uploadid"));
        assert_eq!(out.headers["etag"], "<etag:bare>");
        assert_eq!(out.headers["vary"], "Origin, X-Origin");
        assert_eq!(out.headers["x-goog-generation"], "<generation:16>");
        assert_eq!(out.headers["expires"], "<time:http>");
        assert!(!out.headers.contains_key("x-goog-gcs-base-ts"));
        assert_eq!(out.body, NormBody::Json(json!({"name": "<bucket>"})));
    }

    #[test]
    fn short_numbers_are_not_mistaken_for_generations() {
        assert_eq!(scrub_generation("generation=42&x"), "generation=42&x");
        assert_eq!(
            scrub_generation("generation=1730000000000001"),
            "generation=<generation:16>"
        );
    }

    #[test]
    fn xml_bodies_become_normalized_ordered_trees() {
        let xml = "<?xml version='1.0' encoding='UTF-8'?><ListBucketResult>\
            <Name>gcsmimic-diff-abc-b</Name><Contents><Key>k</Key>\
            <Generation>1730000000000001</Generation>\
            <LastModified>2026-11-01T00:00:00.000Z</LastModified>\
            <ETag>\"abc\"</ETag><Size>5</Size></Contents></ListBucketResult>";
        let response = Response {
            status: 200,
            headers: vec![(
                "Content-Type".into(),
                "application/xml; charset=UTF-8".into(),
            )],
            body: xml.as_bytes().to_vec(),
        };
        let NormBody::Json(tree) = normalizer().response(&response, false).body else {
            panic!("xml body should become a tree");
        };
        assert_eq!(tree["children"][0]["text"], "<bucket>");
        let contents = &tree["children"][1]["children"];
        assert_eq!(contents[0]["text"], "k");
        assert_eq!(contents[1]["text"], "<generation:16>");
        assert_eq!(contents[2]["text"], "<time:f3>");
        assert_eq!(contents[3]["text"], "<etag:quoted>");
        assert_eq!(contents[4]["text"], "5");
        // A document that is not well formed stays plain text.
        let broken = Response {
            body: b"<a><b></a>".to_vec(),
            ..response
        };
        assert!(matches!(
            normalizer().response(&broken, false).body,
            NormBody::Text(_)
        ));
    }

    #[test]
    fn grpc_project_resource_names_are_normalized() {
        let value = json!({"name": "projects/_/buckets/b", "project": "projects/123456789"});
        let out = normalizer().json(&value);
        assert_eq!(out["project"], "projects/<project-number>");
        let mut found = Vec::new();
        project_numbers(&value, &mut found);
        assert_eq!(found, ["123456789"]);
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
