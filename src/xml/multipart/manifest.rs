use super::*;
use crate::xml::document::{self, Node, malformed};

#[derive(Clone, Copy)]
enum Scope {
    Before,
    Root,
    Part,
    Number,
    Etag,
    Done,
}

#[derive(Default)]
struct Part {
    number: Option<PartNumber>,
    etag: Option<String>,
    text: String,
}

impl Part {
    fn start(&mut self, scope: Scope, name: &str) -> Result<Scope, Error> {
        Ok(match (scope, name) {
            (Scope::Before, "CompleteMultipartUpload") => Scope::Root,
            (Scope::Root, "Part") => {
                *self = Self::default();
                Scope::Part
            }
            (Scope::Part, "PartNumber") if self.number.is_none() => {
                self.text.clear();
                Scope::Number
            }
            (Scope::Part, "ETag") if self.etag.is_none() => {
                self.text.clear();
                Scope::Etag
            }
            _ => return Err(malformed()),
        })
    }

    fn end(
        &mut self,
        scope: Scope,
        parts: &mut Vec<storage::CompletedPart>,
    ) -> Result<Scope, Error> {
        Ok(match scope {
            Scope::Number => {
                self.number = Some(self.text.trim().parse()?);
                Scope::Part
            }
            Scope::Etag => {
                let text = self.text.trim();
                if text.is_empty() || text.len() > 128 {
                    return Err(malformed());
                }
                self.etag = Some(text.to_owned());
                Scope::Part
            }
            Scope::Part => {
                if parts.len() == storage::MAX_PARTS {
                    return Err(Error::invalid(
                        "A multipart manifest may contain at most 10000 parts",
                    ));
                }
                parts.push(storage::CompletedPart {
                    number: self.number.take().ok_or_else(malformed)?.get(),
                    etag: self.etag.take().ok_or_else(malformed)?,
                });
                Scope::Root
            }
            Scope::Root => Scope::Done,
            Scope::Before | Scope::Done => return Err(malformed()),
        })
    }

    fn text(&mut self, scope: Scope, text: &str) -> Result<(), Error> {
        match scope {
            Scope::Number | Scope::Etag => self.text.push_str(text),
            Scope::Before | Scope::Root | Scope::Part | Scope::Done => {
                if !text
                    .chars()
                    .all(|ch| matches!(ch, ' ' | '\t' | '\r' | '\n'))
                {
                    return Err(malformed());
                }
            }
        }
        Ok(())
    }
}

/// Only the current scalar and bounded part metadata are retained; no tree or object body.
/// `Before` and `Done` are the scopes outside the root, so they match the walk's depth 0.
pub(super) fn parse(bytes: &[u8]) -> Result<Vec<storage::CompletedPart>, Error> {
    let mut scope = Scope::Before;
    let mut part = Part::default();
    let mut parts = Vec::new();
    document::walk(bytes, MANIFEST_LIMIT, |node| {
        match node {
            Node::Start(name) => scope = part.start(scope, name)?,
            Node::End => scope = part.end(scope, &mut parts)?,
            Node::Text(text) => part.text(scope, text)?,
        }
        Ok(())
    })?;
    if matches!(scope, Scope::Done) && !parts.is_empty() {
        Ok(parts)
    } else {
        Err(malformed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_accepts_namespace_sparse_parts_and_escaped_etags() {
        let parts=parse(b"<?xml version=\"1.0\"?><CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Part><PartNumber>2</PartNumber><ETag>&quot;abc&quot;</ETag></Part><Part><ETag>\"def\"</ETag><PartNumber>8</PartNumber></Part></CompleteMultipartUpload>").unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].number, 2);
        assert_eq!(parts[0].etag, "\"abc\"");
        assert_eq!(parts[1].number, 8);
    }

    #[test]
    fn manifest_rejects_ambiguous_or_unbounded_grammar() {
        for body in [
            "",
            "<!DOCTYPE x><CompleteMultipartUpload></CompleteMultipartUpload>",
            "<CompleteMultipartUpload/>",
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>&external;</ETag></Part></CompleteMultipartUpload>",
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><PartNumber>2</PartNumber><ETag>x</ETag></Part></CompleteMultipartUpload>",
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>x</ETag><Ignored/></Part></CompleteMultipartUpload>",
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber></Part></CompleteMultipartUpload>",
            "<?xml garbage?><CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>x</ETag></Part></CompleteMultipartUpload>",
            "<CompleteMultipartUploadxmlns=\"http://doc.s3.amazonaws.com/2006-03-01\"><Part><PartNumber>1</PartNumber><ETag>x</ETag></Part></CompleteMultipartUploadxmlns>",
        ] {
            assert!(parse(body.as_bytes()).is_err(), "{body}");
        }
    }

    #[test]
    fn manifest_keeps_ten_thousand_parts_and_two_megabyte_limits() {
        let mut body = String::from("<CompleteMultipartUpload>");
        for number in 1..=10000 {
            body.push_str(&format!(
                "<Part><PartNumber>{number}</PartNumber><ETag>x</ETag></Part>"
            ));
        }
        let end = "</CompleteMultipartUpload>";
        let full = format!("{body}{end}");
        let parts = parse(full.as_bytes()).unwrap();
        assert_eq!(parts.len(), 10000);
        assert_eq!(parts.last().unwrap().number, 10000);
        body.push_str("<Part><PartNumber>1</PartNumber><ETag>x</ETag></Part>");
        assert!(parse(format!("{body}{end}").as_bytes()).is_err());
        let prefix =
            "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber><ETag>x</ETag></Part>";
        let boundary = format!(
            "{prefix}{}{end}",
            " ".repeat(MANIFEST_LIMIT - prefix.len() - end.len())
        );
        assert_eq!(parse(boundary.as_bytes()).unwrap().len(), 1);
        assert!(parse(format!("{boundary} ").as_bytes()).is_err());
    }
}
