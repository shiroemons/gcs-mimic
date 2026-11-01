//! Variable expansion and value capture for declarative case steps.

use super::transport::Response;
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) type Vars = BTreeMap<String, String>;
type Result<T> = std::result::Result<T, String>;

#[derive(Debug, PartialEq)]
enum Part {
    Text(String),
    Var(String),
}

/// Splits a template into literal text and `{name}` references. `{{` and `}}`
/// write literal braces so request bodies can still contain them.
fn parts(template: &str) -> Result<Vec<Part>> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                text.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                text.push('}');
            }
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) if c.is_ascii_alphanumeric() || c == '_' => name.push(c),
                        _ => return Err(format!("invalid variable reference in {template:?}")),
                    }
                }
                if name.is_empty() {
                    return Err(format!("empty variable reference in {template:?}"));
                }
                if !text.is_empty() {
                    parts.push(Part::Text(std::mem::take(&mut text)));
                }
                parts.push(Part::Var(name));
            }
            c => text.push(c),
        }
    }
    if !text.is_empty() {
        parts.push(Part::Text(text));
    }
    Ok(parts)
}

pub(crate) fn expand(template: &str, vars: &Vars) -> Result<String> {
    let mut out = String::new();
    for part in parts(template)? {
        match part {
            Part::Text(text) => out.push_str(&text),
            Part::Var(name) => out.push_str(
                vars.get(&name)
                    .ok_or_else(|| format!("undefined variable {{{name}}}"))?,
            ),
        }
    }
    Ok(out)
}

pub(crate) fn variables(template: &str) -> Result<Vec<String>> {
    Ok(parts(template)?
        .into_iter()
        .filter_map(|part| match part {
            Part::Var(name) => Some(name),
            Part::Text(_) => None,
        })
        .collect())
}

/// Expands string values only; object keys stay literal so the document
/// structure cannot depend on a captured value.
pub(crate) fn expand_json(value: &Value, vars: &Vars) -> Result<Value> {
    Ok(match value {
        Value::String(text) => Value::String(expand(text, vars)?),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| expand_json(item, vars))
                .collect::<Result<_>>()?,
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| Ok((key.clone(), expand_json(item, vars)?)))
                .collect::<Result<_>>()?,
        ),
        other => other.clone(),
    })
}

pub(crate) fn json_variables(value: &Value) -> Result<Vec<String>> {
    match value {
        Value::String(text) => variables(text),
        Value::Array(items) => {
            let mut all = Vec::new();
            for item in items {
                all.extend(json_variables(item)?);
            }
            Ok(all)
        }
        Value::Object(map) => {
            let mut all = Vec::new();
            for item in map.values() {
                all.extend(json_variables(item)?);
            }
            Ok(all)
        }
        _ => Ok(Vec::new()),
    }
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub(crate) fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Decodes `%XX` escapes; an invalid escape stays as written. Used to inspect
/// the request that will actually be sent, not the spelling a case chose.
pub(crate) fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let decoded = (bytes[at] == b'%' && at + 2 < bytes.len())
            .then(|| text.get(at + 1..at + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match decoded {
            Some(byte) => {
                out.push(byte);
                at += 3;
            }
            None => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn json_scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// Walks a dotted path; numeric segments index arrays.
pub(crate) fn json_get<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in path.split('.').filter(|s| !s.is_empty()) {
        current = match current {
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            Value::Object(map) => map.get(segment)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Extracts a value from a response. Supported specs: `body:a.b.0.c`,
/// `header:name` and `header:name?param` (a query parameter of the header value,
/// for example the upload ID in a resumable session `Location`).
pub(crate) fn extract(spec: &str, response: &Response) -> Result<String> {
    if let Some(path) = spec.strip_prefix("body:") {
        let json: Value = serde_json::from_slice(&response.body)
            .map_err(|e| format!("capture {spec}: body is not JSON: {e}"))?;
        return json_get(&json, path)
            .and_then(json_scalar)
            .ok_or_else(|| format!("capture {spec}: no scalar at that path"));
    }
    if let Some(rest) = spec.strip_prefix("header:") {
        let (name, param) = match rest.split_once('?') {
            Some((name, param)) => (name, Some(param)),
            None => (rest, None),
        };
        let value = response
            .header(name)
            .ok_or_else(|| format!("capture {spec}: header absent"))?;
        let Some(param) = param else {
            return Ok(value.to_owned());
        };
        let query = value.split_once('?').map_or("", |(_, query)| query);
        return query
            .split('&')
            .find_map(|pair| pair.strip_prefix(param)?.strip_prefix('='))
            .map(str::to_owned)
            .ok_or_else(|| format!("capture {spec}: query parameter absent"));
    }
    Err(format!("capture {spec}: expected body:PATH or header:NAME"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars {
        Vars::from([("bucket".into(), "b1".into()), ("gen".into(), "42".into())])
    }

    #[test]
    fn expands_variables_and_escapes() {
        assert_eq!(
            expand("/b/{bucket}/o?g={gen}", &vars()).unwrap(),
            "/b/b1/o?g=42"
        );
        assert_eq!(
            expand("{{\"a\":\"{bucket}\"}}", &vars()).unwrap(),
            "{\"a\":\"b1\"}"
        );
    }

    #[test]
    fn rejects_undefined_and_malformed_references() {
        assert!(expand("{nope}", &vars()).unwrap_err().contains("undefined"));
        assert!(expand("{bucket", &vars()).is_err());
        assert!(expand("{}", &vars()).is_err());
        assert!(expand("{a b}", &vars()).is_err());
    }

    #[test]
    fn lists_variables_in_json_values_but_not_keys() {
        let value = serde_json::json!({"{k}": ["{a}", {"x": "p{b}q"}], "n": 1});
        assert_eq!(json_variables(&value).unwrap(), ["a", "b"]);
        let expanded = expand_json(&serde_json::json!({"n": "{bucket}"}), &vars()).unwrap();
        assert_eq!(expanded["n"], "b1");
    }

    #[test]
    fn percent_encodes_reserved_bytes() {
        assert_eq!(percent_encode("a b/é~"), "a%20b%2F%C3%A9~");
    }

    #[test]
    fn captures_from_body_and_header() {
        let response = Response {
            status: 200,
            headers: vec![(
                "location".into(),
                "https://h/upload?uploadType=resumable&upload_id=XYZ".into(),
            )],
            body: br#"{"generation":"7","items":[{"name":"a"}]}"#.to_vec(),
        };
        assert_eq!(extract("body:generation", &response).unwrap(), "7");
        assert_eq!(extract("body:items.0.name", &response).unwrap(), "a");
        assert_eq!(
            extract("header:location?upload_id", &response).unwrap(),
            "XYZ"
        );
        assert!(extract("body:missing", &response).is_err());
        assert!(extract("header:etag", &response).is_err());
        assert!(extract("cookie:x", &response).is_err());
    }
}
