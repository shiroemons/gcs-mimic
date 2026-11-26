//! XML response documents: the declaration, a root element and escaped text elements.

use std::fmt::{self, Write};

/// The XML declaration Cloud Storage writes, with single-quoted pseudo-attributes.
pub(super) const DECLARATION: &str = "<?xml version='1.0' encoding='UTF-8'?>";

/// The namespace a response root declares, in the two spellings Cloud Storage uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Namespace {
    /// No `xmlns`: errors, `CopyObjectResult` and `PostResponse`.
    None,
    /// `http://doc.s3.amazonaws.com/2006-03-01`, on object and bucket listings.
    Listing,
    /// `http://s3.amazonaws.com/doc/2006-03-01/`, on multipart uploads.
    Multipart,
}

/// Which characters text escapes besides `&`, `<` and `>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Quotes {
    /// Literal, as Cloud Storage writes `<ETag>"…"</ETag>`.
    Literal,
    /// `"` and `'` become `&quot;` and `&apos;`, as the POST Object response writes them.
    Escaped,
}

struct Escaped<'a> {
    body: &'a mut String,
    quotes: Quotes,
}

impl Escaped<'_> {
    fn push_run(&mut self, mut run: &str) {
        if self.quotes == Quotes::Escaped {
            while let Some(index) = memchr::memchr2(b'"', b'\'', run.as_bytes()) {
                let (plain, quoted) = run.split_at(index);
                self.body.push_str(plain);
                let (quote, rest) = quoted.split_at(1);
                self.body
                    .push_str(if quote == "\"" { "&quot;" } else { "&apos;" });
                run = rest;
            }
        }
        self.body.push_str(run);
    }

    fn push_text(&mut self, mut text: &str) {
        while let Some(index) = memchr::memchr3(b'&', b'<', b'>', text.as_bytes()) {
            let (plain, escaped) = text.split_at(index);
            self.push_run(plain);
            let (special, rest) = escaped.split_at(1);
            self.body.push_str(match special {
                "&" => "&amp;",
                "<" => "&lt;",
                _ => "&gt;",
            });
            text = rest;
        }
        self.push_run(text);
    }
}

impl Write for Escaped<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.push_text(text);
        Ok(())
    }
}

/// One response document, written in order. Nested content goes through [`Self::group`] or
/// [`Self::try_group`], which pair the start and end tags; [`Self::open`] and [`Self::close`]
/// exist for content produced across an `.await`.
pub(super) struct XmlWriter {
    body: String,
    root: &'static str,
    quotes: Quotes,
}

impl XmlWriter {
    pub(super) fn new(root: &'static str, namespace: Namespace) -> Self {
        Self::with_quotes(root, namespace, Quotes::Literal)
    }

    pub(super) fn with_quotes(root: &'static str, namespace: Namespace, quotes: Quotes) -> Self {
        let xmlns = match namespace {
            Namespace::None => "",
            Namespace::Listing => " xmlns='http://doc.s3.amazonaws.com/2006-03-01'",
            Namespace::Multipart => " xmlns='http://s3.amazonaws.com/doc/2006-03-01/'",
        };
        Self {
            body: format!("{DECLARATION}<{root}{xmlns}>"),
            root,
            quotes,
        }
    }

    /// `<name>text</name>`, with `text` escaped.
    pub(super) fn element(&mut self, name: &str, text: &str) {
        self.open(name);
        self.escaped().push_text(text);
        self.close(name);
    }

    /// `<name>value</name>` for a number or boolean.
    pub(super) fn value(&mut self, name: &str, value: impl fmt::Display + Copy) {
        self.open(name);
        #[expect(
            clippy::expect_used,
            reason = "the numeric and boolean formatters only propagate errors from their writer, and this one always succeeds"
        )]
        write!(self.escaped(), "{value}").expect("writing XML text into a String");
        self.close(name);
    }

    fn escaped(&mut self) -> Escaped<'_> {
        Escaped {
            body: &mut self.body,
            quotes: self.quotes,
        }
    }

    /// `<name>…</name>` around what `content` writes.
    pub(super) fn group(&mut self, name: &str, content: impl FnOnce(&mut Self)) {
        self.open(name);
        content(self);
        self.close(name);
    }

    /// `<name>…</name>` around what `content` writes, unless it fails.
    pub(super) fn try_group<E>(
        &mut self,
        name: &str,
        content: impl FnOnce(&mut Self) -> Result<(), E>,
    ) -> Result<(), E> {
        self.open(name);
        content(self)?;
        self.close(name);
        Ok(())
    }

    pub(super) fn open(&mut self, name: &str) {
        self.body.push('<');
        self.body.push_str(name);
        self.body.push('>');
    }

    pub(super) fn close(&mut self, name: &str) {
        self.body.push_str("</");
        self.body.push_str(name);
        self.body.push('>');
    }

    /// The document with its root closed.
    pub(super) fn finish(mut self) -> String {
        self.close(self.root);
        self.body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_display_escape_runs_at_utf8_boundaries() {
        for (quotes, expected) in [
            (Quotes::Literal, "&lt;猫&amp;&gt;\"'é&lt;"),
            (Quotes::Escaped, "&lt;猫&amp;&gt;&quot;&apos;é&lt;"),
        ] {
            let mut writer = XmlWriter::with_quotes("Root", Namespace::None, quotes);
            let text = "<猫&>\"'é<";
            writer.element("Text", text);
            writer.value("Value", text);
            assert_eq!(
                writer.finish(),
                format!(
                    "{DECLARATION}<Root><Text>{expected}</Text><Value>{expected}</Value></Root>"
                )
            );
        }
    }
}
