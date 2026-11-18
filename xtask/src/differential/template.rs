//! Variable expansion and value capture for declarative case steps.

use super::transport::Response;
use serde_json::Value;
use std::{collections::BTreeMap, fmt, str::FromStr};

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

/// The template as the static rules read it: `{{` and `}}` unescaped and every
/// variable kept as its `{name}` reference, since no value is known yet.
pub(crate) fn literal(template: &str) -> Result<String> {
    let mut out = String::new();
    for part in parts(template)? {
        match part {
            Part::Text(text) => out.push_str(&text),
            Part::Var(name) => {
                out.push('{');
                out.push_str(&name);
                out.push('}');
            }
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

/// Where a step's capture takes its value: `body:a.b.0.c` (a JSON scalar at a dotted
/// path), `header:name` or `header:name?param` (a query parameter of the header value,
/// for example the upload ID in a resumable session `Location`), or `xml:Element` (the
/// text of the first element with that name in an XML body, such as an `UploadId`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Capture {
    Body {
        path: String,
    },
    Header {
        name: String,
        param: Option<String>,
    },
    Xml {
        element: String,
    },
    /// `alter-middle:SPEC` or `alter-last:SPEC`: what SPEC captures with one character
    /// changed and its length kept. Cloud Storage IDs are opaque, so this is how a case
    /// asks about an ID that is well formed but names nothing, such as an upload ID the
    /// service never issued.
    Altered {
        at: AlterAt,
        source: Box<Capture>,
    },
}

/// Which character an altered capture changes: the first ASCII letter or digit from the
/// middle of the value on, or the last one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AlterAt {
    Middle,
    Last,
}

impl AlterAt {
    const ALL: [Self; 2] = [Self::Middle, Self::Last];

    fn prefix(self) -> &'static str {
        match self {
            Self::Middle => "alter-middle:",
            Self::Last => "alter-last:",
        }
    }
}

/// `value` with one ASCII letter or digit changed and its length kept. A letter changes
/// case and a digit becomes a letter from `a` to `f`: the character stays in the base64,
/// base64url and hexadecimal alphabets, and in base64 it changes the two high bits of its
/// sextet, which even the last character of an encoding carries, so a decoder that ignores
/// padding bits cannot read the altered value as the original.
fn alter(value: &str, at: AlterAt) -> Option<String> {
    let mut chars: Vec<char> = value.chars().collect();
    let alterable = |c: &char| c.is_ascii_alphanumeric();
    let index = match at {
        AlterAt::Middle => {
            let middle = chars.len() / 2;
            middle + chars.iter().skip(middle).position(alterable)?
        }
        AlterAt::Last => chars.iter().rposition(alterable)?,
    };
    let slot = chars.get_mut(index)?;
    *slot = match slot.to_digit(10) {
        Some(digit) => char::from_digit(10 + digit % 6, 16)?,
        None if slot.is_ascii_uppercase() => slot.to_ascii_lowercase(),
        None => slot.to_ascii_uppercase(),
    };
    Some(chars.into_iter().collect())
}

/// Parsed when a case is loaded, so a malformed spec fails validation instead of
/// failing after earlier steps were already sent to the service.
impl FromStr for Capture {
    type Err = String;

    fn from_str(spec: &str) -> Result<Self> {
        let altered = AlterAt::ALL
            .into_iter()
            .find_map(|at| Some((at, spec.strip_prefix(at.prefix())?)));
        if let Some((at, source)) = altered {
            return match source.parse()? {
                Self::Altered { .. } => Err(format!(
                    "capture {spec}: an altered capture cannot be altered again"
                )),
                source => Ok(Self::Altered {
                    at,
                    source: Box::new(source),
                }),
            };
        }
        if let Some(path) = spec.strip_prefix("body:").filter(|path| !path.is_empty()) {
            return Ok(Self::Body {
                path: path.to_owned(),
            });
        }
        if let Some(element) = spec.strip_prefix("xml:").filter(|e| !e.is_empty()) {
            return Ok(Self::Xml {
                element: element.to_owned(),
            });
        }
        let rest = spec
            .strip_prefix("header:")
            .filter(|rest| !rest.is_empty() && !rest.starts_with('?'))
            .ok_or_else(|| {
                format!("capture {spec}: expected body:PATH, header:NAME or xml:ELEMENT")
            })?;
        Ok(match rest.split_once('?') {
            Some((name, param)) => Self::Header {
                name: name.to_owned(),
                param: Some(param.to_owned()),
            },
            None => Self::Header {
                name: rest.to_owned(),
                param: None,
            },
        })
    }
}

/// The spec as written: the step fingerprint hashes this text, so it must give back
/// exactly what `from_str` read.
impl fmt::Display for Capture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Body { path } => write!(f, "body:{path}"),
            Self::Header { name, param: None } => write!(f, "header:{name}"),
            Self::Header {
                name,
                param: Some(param),
            } => write!(f, "header:{name}?{param}"),
            Self::Xml { element } => write!(f, "xml:{element}"),
            Self::Altered { at, source } => write!(f, "{}{source}", at.prefix()),
        }
    }
}

/// Extracts a captured value from a response.
pub(crate) fn extract(capture: &Capture, response: &Response) -> Result<String> {
    match capture {
        Capture::Body { path } => {
            let json: Value = serde_json::from_slice(&response.body)
                .map_err(|e| format!("capture {capture}: body is not JSON: {e}"))?;
            json_get(&json, path)
                .and_then(json_scalar)
                .ok_or_else(|| format!("capture {capture}: no scalar at that path"))
        }
        Capture::Header { name, param } => {
            let value = response
                .header(name)
                .ok_or_else(|| format!("capture {capture}: header absent"))?;
            let Some(param) = param else {
                return Ok(value.to_owned());
            };
            let query = value.split_once('?').map_or("", |(_, query)| query);
            query
                .split('&')
                .find_map(|pair| pair.strip_prefix(param.as_str())?.strip_prefix('='))
                .map(str::to_owned)
                .ok_or_else(|| format!("capture {capture}: query parameter absent"))
        }
        // Read whatever the content type says: Cloud Storage sends some XML documents
        // as text/html.
        Capture::Xml { element } => {
            let tree = super::xml::parse(&String::from_utf8_lossy(&response.body))
                .map_err(|e| format!("capture {capture}: body is not XML: {e}"))?;
            let found = super::xml::find(&tree, element)
                .ok_or_else(|| format!("capture {capture}: element absent"))?;
            super::xml::text(found)
                .map(str::to_owned)
                .ok_or_else(|| format!("capture {capture}: element has child elements"))
        }
        Capture::Altered { at, source } => alter(&extract(source, response)?, *at)
            .ok_or_else(|| format!("capture {capture}: no ASCII letter or digit to change")),
    }
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
    fn literal_unescapes_braces_and_keeps_variable_references() {
        assert_eq!(literal("{{\"a\":\"{b}\"}}").unwrap(), "{\"a\":\"{b}\"}");
        // Read as the expansion reads it: an escaped brace on each side of a variable.
        assert_eq!(literal("{{{x}}}").unwrap(), "{{x}}");
        assert!(literal("{bad").is_err());
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
        let capture = |spec: &str| extract(&spec.parse().unwrap(), &response);
        assert_eq!(capture("body:generation").unwrap(), "7");
        assert_eq!(capture("body:items.0.name").unwrap(), "a");
        assert_eq!(capture("header:location?upload_id").unwrap(), "XYZ");
        assert_eq!(
            capture("body:missing").unwrap_err(),
            "capture body:missing: no scalar at that path"
        );
        assert_eq!(
            capture("header:etag").unwrap_err(),
            "capture header:etag: header absent"
        );
        assert_eq!(
            capture("header:location?session").unwrap_err(),
            "capture header:location?session: query parameter absent"
        );
    }

    #[test]
    fn captures_the_first_matching_xml_element_whatever_the_content_type() {
        let response = |body: &str| Response {
            status: 200,
            headers: vec![("content-type".into(), "text/html; charset=UTF-8".into())],
            body: body.as_bytes().to_vec(),
        };
        let listing = response(
            "<?xml version='1.0' encoding='UTF-8'?><ListMultipartUploadsResult>\
             <NextKeyMarker></NextKeyMarker><Upload><Key>a</Key><UploadId>U1</UploadId></Upload>\
             <Upload><Key>b</Key><UploadId>U2</UploadId></Upload></ListMultipartUploadsResult>",
        );
        let capture = |spec: &str, response: &Response| extract(&spec.parse().unwrap(), response);
        assert_eq!(capture("xml:UploadId", &listing).unwrap(), "U1");
        assert_eq!(capture("xml:NextKeyMarker", &listing).unwrap(), "");
        assert_eq!(
            capture("xml:Upload", &listing).unwrap_err(),
            "capture xml:Upload: element has child elements"
        );
        assert_eq!(
            capture("xml:NextUploadIdMarker", &listing).unwrap_err(),
            "capture xml:NextUploadIdMarker: element absent"
        );
        let error = capture("xml:UploadId", &response(r#"{"uploadId":"U1"}"#)).unwrap_err();
        assert!(
            error.starts_with("capture xml:UploadId: body is not XML: "),
            "{error}"
        );
    }

    #[test]
    fn an_altered_capture_changes_one_letter_or_digit_and_keeps_the_length() {
        let response = Response {
            status: 200,
            headers: Vec::new(),
            body: br#"{"a":"b/0123abcd","b":"QUJD+w==","c":"ab/c=","d":"x/-="}"#.to_vec(),
        };
        let capture = |spec: &str| extract(&spec.parse().unwrap(), &response);
        // From the middle on: index 5 holds the digit 3, which becomes the letter d.
        assert_eq!(capture("alter-middle:body:a").unwrap(), "b/012dabcd");
        assert_eq!(capture("alter-last:body:a").unwrap(), "b/0123abcD");
        // Padding and other symbols are skipped, so the encoding stays well formed.
        assert_eq!(capture("alter-last:body:b").unwrap(), "QUJD+W==");
        assert_eq!(capture("alter-middle:body:c").unwrap(), "ab/C=");
        assert_eq!(
            capture("alter-middle:body:d").unwrap_err(),
            "capture alter-middle:body:d: no ASCII letter or digit to change"
        );
        assert_eq!(
            capture("alter-last:body:missing").unwrap_err(),
            "capture body:missing: no scalar at that path"
        );
        for digit in '0'..='9' {
            let altered = alter(&digit.to_string(), AlterAt::Last).unwrap();
            assert!(matches!(
                altered.as_str(),
                "a" | "b" | "c" | "d" | "e" | "f"
            ));
        }
    }

    #[test]
    fn capture_specs_print_as_written_and_reject_other_forms() {
        for (spec, capture) in [
            (
                "body:items.0.name",
                Capture::Body {
                    path: "items.0.name".into(),
                },
            ),
            (
                "header:etag",
                Capture::Header {
                    name: "etag".into(),
                    param: None,
                },
            ),
            (
                "header:a?b?c",
                Capture::Header {
                    name: "a".into(),
                    param: Some("b?c".into()),
                },
            ),
            (
                "xml:UploadId",
                Capture::Xml {
                    element: "UploadId".into(),
                },
            ),
            (
                "alter-middle:body:uploadId",
                Capture::Altered {
                    at: AlterAt::Middle,
                    source: Box::new(Capture::Body {
                        path: "uploadId".into(),
                    }),
                },
            ),
            (
                "alter-last:header:location?upload_id",
                Capture::Altered {
                    at: AlterAt::Last,
                    source: Box::new(Capture::Header {
                        name: "location".into(),
                        param: Some("upload_id".into()),
                    }),
                },
            ),
        ] {
            assert_eq!(spec.parse::<Capture>().unwrap(), capture);
            assert_eq!(capture.to_string(), spec);
        }
        assert_eq!(
            "alter-last:alter-middle:body:a"
                .parse::<Capture>()
                .unwrap_err(),
            "capture alter-last:alter-middle:body:a: an altered capture cannot be altered again"
        );
        for spec in ["alter-middle:", "alter-last:cookie:x"] {
            assert!(spec.parse::<Capture>().is_err(), "{spec}");
        }
        for spec in ["body:", "header:", "header:?x", "xml:", "cookie:x", ""] {
            assert_eq!(
                spec.parse::<Capture>().unwrap_err(),
                format!("capture {spec}: expected body:PATH, header:NAME or xml:ELEMENT")
            );
        }
    }
}
