//! XML response documents: the declaration, a root element and escaped text elements.

use std::fmt;

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
        for ch in text.chars() {
            match (ch, self.quotes) {
                ('&', _) => self.body.push_str("&amp;"),
                ('<', _) => self.body.push_str("&lt;"),
                ('>', _) => self.body.push_str("&gt;"),
                ('"', Quotes::Escaped) => self.body.push_str("&quot;"),
                ('\'', Quotes::Escaped) => self.body.push_str("&apos;"),
                (ch, Quotes::Literal | Quotes::Escaped) => self.body.push(ch),
            }
        }
        self.close(name);
    }

    /// `<name>value</name>` for a number or boolean.
    pub(super) fn value(&mut self, name: &str, value: impl fmt::Display + Copy) {
        self.element(name, &value.to_string());
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
