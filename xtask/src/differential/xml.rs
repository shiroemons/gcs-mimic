//! A small XML reader for Cloud Storage XML API responses (`ListBucketResult`,
//! `Error`, `CopyObjectResult`). It turns a document into an ordered JSON tree
//! so the harness compares element order and nesting instead of raw text, and
//! so the usual path-based ignore patterns keep working.
//!
//! Shape of the tree: every element is `{"name": TAG}` plus `"attrs"` (object,
//! only when attributes exist) and either `"text"` (a leaf) or `"children"` (an
//! ordered array). Text between child elements is dropped. Namespaces are not
//! resolved; a prefix stays part of the name. DTDs and processing instructions
//! other than the declaration are not supported and make the parse fail, so the
//! caller falls back to a plain text comparison.

use serde_json::{Map, Value, json};

type Result<T> = std::result::Result<T, String>;

struct Reader<'a> {
    text: &'a str,
    at: usize,
}

impl<'a> Reader<'a> {
    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn starts_with(&self, prefix: &str) -> bool {
        self.rest().starts_with(prefix)
    }

    fn skip_space(&mut self) {
        let trimmed = self.rest().trim_start();
        self.at = self.text.len() - trimmed.len();
    }

    fn take_until(&mut self, end: &str) -> Result<&'a str> {
        let rest = self.rest();
        let found = rest
            .find(end)
            .ok_or_else(|| format!("unterminated construct, expected {end:?}"))?;
        self.at += found + end.len();
        Ok(&rest[..found])
    }

    fn name(&mut self) -> Result<String> {
        let rest = self.rest();
        let length = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '>' | '/' | '='))
            .unwrap_or(rest.len());
        if length == 0 {
            return Err("expected a name".into());
        }
        self.at += length;
        Ok(rest[..length].to_owned())
    }
}

fn decode_entities(text: &str) -> Result<String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let end = tail.find(';').ok_or("unterminated entity")?;
        let entity = &tail[..end];
        let decoded = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let number = entity.strip_prefix('#').ok_or("unknown entity")?;
                let code = match number.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16),
                    None => number.parse(),
                }
                .map_err(|_| "bad character reference")?;
                char::from_u32(code).ok_or("bad character reference")?
            }
        };
        out.push(decoded);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn attribute_value(reader: &mut Reader<'_>) -> Result<String> {
    let quote = reader
        .rest()
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\''))
        .ok_or("attribute value must be quoted")?;
    reader.at += 1;
    let raw = reader.take_until(&quote.to_string())?;
    decode_entities(raw)
}

fn element(reader: &mut Reader<'_>, depth: usize) -> Result<Value> {
    if depth > 64 {
        return Err("document nests too deeply".into());
    }
    reader.at += 1; // '<'
    let name = reader.name()?;
    let mut attrs = Map::new();
    loop {
        reader.skip_space();
        if reader.starts_with("/>") {
            reader.at += 2;
            return Ok(finish(&name, attrs, String::new(), Vec::new()));
        }
        if reader.starts_with(">") {
            reader.at += 1;
            break;
        }
        let key = reader.name()?;
        reader.skip_space();
        if !reader.starts_with("=") {
            return Err(format!("attribute {key:?} has no value"));
        }
        reader.at += 1;
        reader.skip_space();
        attrs.insert(key, Value::String(attribute_value(reader)?));
    }
    let mut text = String::new();
    let mut children = Vec::new();
    loop {
        let rest = reader.rest();
        let Some(next) = rest.find('<') else {
            return Err(format!("element {name:?} is not closed"));
        };
        text.push_str(&decode_entities(&rest[..next])?);
        reader.at += next;
        if reader.starts_with("</") {
            reader.at += 2;
            let closing = reader.name()?;
            reader.skip_space();
            if closing != name || !reader.starts_with(">") {
                return Err(format!("mismatched closing tag {closing:?} for {name:?}"));
            }
            reader.at += 1;
            return Ok(finish(&name, attrs, text, children));
        }
        if reader.starts_with("<!--") {
            reader.at += 4;
            reader.take_until("-->")?;
        } else if reader.starts_with("<![CDATA[") {
            reader.at += 9;
            text.push_str(reader.take_until("]]>")?);
        } else if reader.starts_with("<?") || reader.starts_with("<!") {
            return Err("unsupported markup inside an element".into());
        } else {
            children.push(element(reader, depth + 1)?);
        }
    }
}

fn finish(name: &str, attrs: Map<String, Value>, text: String, children: Vec<Value>) -> Value {
    let mut map = Map::new();
    map.insert("name".into(), json!(name));
    if !attrs.is_empty() {
        map.insert("attrs".into(), Value::Object(attrs));
    }
    if children.is_empty() {
        map.insert("text".into(), Value::String(text));
    } else {
        map.insert("children".into(), Value::Array(children));
    }
    Value::Object(map)
}

/// Parses a document. The declaration, when present, is kept as a canonical
/// `declaration` string on the root so a missing or different one shows up.
pub(crate) fn parse(text: &str) -> Result<Value> {
    let mut reader = Reader { text, at: 0 };
    reader.skip_space();
    let mut declaration = None;
    if reader.starts_with("<?xml") {
        reader.at += 2;
        let body = reader.take_until("?>")?;
        declaration = Some(
            body.replace('\'', "\"")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    reader.skip_space();
    if !reader.starts_with("<") || reader.starts_with("<!") || reader.starts_with("<?") {
        return Err("no root element".into());
    }
    let mut root = element(&mut reader, 0)?;
    reader.skip_space();
    if !reader.rest().is_empty() {
        return Err("content after the root element".into());
    }
    if let (Some(declaration), Value::Object(map)) = (declaration, &mut root) {
        map.insert("declaration".into(), Value::String(declaration));
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_order_nesting_attributes_and_entities() {
        let tree = parse(
            "<?xml version='1.0' encoding='UTF-8'?>\n<R a='1'><B>x &amp; &#65;</B><C><D/></C><B><![CDATA[<y>]]></B></R>",
        )
        .unwrap();
        assert_eq!(
            tree["declaration"],
            "xml version=\"1.0\" encoding=\"UTF-8\""
        );
        assert_eq!(tree["name"], "R");
        assert_eq!(tree["attrs"]["a"], "1");
        let children = tree["children"].as_array().unwrap();
        assert_eq!(children[0]["text"], "x & A");
        assert_eq!(children[1]["children"][0]["text"], "");
        assert_eq!(children[2]["text"], "<y>");
    }

    #[test]
    fn element_order_is_part_of_the_structure() {
        let a = parse("<R><A>1</A><B>2</B></R>").unwrap();
        let b = parse("<R><B>2</B><A>1</A></R>").unwrap();
        assert_ne!(a, b);
        assert_eq!(
            parse("<R><A></A></R>").unwrap(),
            parse("<R><A/></R>").unwrap()
        );
    }

    #[test]
    fn rejects_malformed_documents() {
        for bad in [
            "",
            "plain text",
            "<R><A></R>",
            "<R>",
            "<R></R><S/>",
            "<R a=1/>",
            "<R>&bogus;</R>",
            "<!DOCTYPE x><R/>",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
