//! Bounded XML metadata parsing without DTDs, external entities or recursive expansion.

use super::{Error, XmlCode};
use quick_xml::{
    XmlVersion,
    events::{BytesDecl, BytesStart, Event},
    name::ResolveResult,
    reader::NsReader,
};

pub(super) struct Element {
    pub(super) name: String,
    pub(super) text: String,
    pub(super) children: Vec<Element>,
}

pub(super) fn malformed() -> Error {
    Error::new(
        XmlCode::MalformedXml,
        "Expected one well-formed UTF-8 XML metadata document",
    )
}

/// One step through a metadata document, after the checks every document shares.
pub(super) enum Node<'a> {
    /// A start tag's local name. An empty element also produces `End`.
    Start(&'a str),
    End,
    /// Character data, CDATA or one expanded standard entity inside the root element.
    Text(&'a str),
}

/// Reads one bounded XML document and passes each node to `visit`. Shared rules: at most
/// `limit` bytes of UTF-8 XML 1.0 characters; the declaration only first; no DTD; attributes
/// only namespace declarations; one namespace throughout; no `]]>` in text; standard
/// entities only; only whitespace outside the root. Every failure here is `malformed`, so
/// the order of these checks within one event is not observable.
pub(super) fn walk(
    bytes: &[u8],
    limit: usize,
    mut visit: impl FnMut(Node<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    let mut reader = reader(bytes, limit)?;
    let mut namespace = None;
    let mut depth = 0_usize;
    let mut declaration_allowed = true;
    loop {
        let (resolved, event) = reader.read_resolved_event().map_err(|_| malformed())?;
        match event {
            Event::Start(start) => {
                validate_attributes(&start)?;
                let current = resolved_namespace(&resolved)?;
                if namespace.as_ref().is_some_and(|root| root != &current) {
                    return Err(malformed());
                }
                namespace.get_or_insert(current);
                visit(Node::Start(start.local_name().as_ref()))?;
                depth += 1;
            }
            Event::End(_) => {
                depth = depth.checked_sub(1).ok_or_else(malformed)?;
                visit(Node::End)?;
            }
            Event::Text(text) if !text.contains("]]>") => {
                text_node(depth, &text.xml10_content(), &mut visit)?;
            }
            Event::CData(text) if depth > 0 => {
                text_node(depth, &text.xml10_content(), &mut visit)?;
            }
            Event::GeneralRef(reference) if depth > 0 => {
                let entity = format!("&{};", &*reference);
                let text = quick_xml::escape::unescape(&entity).map_err(|_| malformed())?;
                text_node(depth, &text, &mut visit)?;
            }
            Event::Decl(declaration) if declaration_allowed => validate_declaration(&declaration)?,
            Event::Comment(_) => {}
            Event::PI(instruction) if !instruction.target().eq_ignore_ascii_case("xml") => {}
            Event::Eof if depth == 0 => return Ok(()),
            // DTDs are forbidden even when they define only internal entities. The reader
            // expands empty elements, so `Empty` never arrives; the rest failed a guard above.
            Event::DocType(_)
            | Event::Empty(_)
            | Event::Text(_)
            | Event::CData(_)
            | Event::GeneralRef(_)
            | Event::Decl(_)
            | Event::PI(_)
            | Event::Eof => return Err(malformed()),
        }
        declaration_allowed = false;
    }
}

/// Text inside the root goes to `visit`; outside it, only whitespace is allowed.
fn text_node(
    depth: usize,
    text: &str,
    visit: &mut impl FnMut(Node<'_>) -> Result<(), Error>,
) -> Result<(), Error> {
    if !text.chars().all(xml_char) {
        return Err(malformed());
    }
    if depth > 0 {
        visit(Node::Text(text))
    } else if text.chars().all(|c| matches!(c, ' ' | '\t' | '\r' | '\n')) {
        Ok(())
    } else {
        Err(malformed())
    }
}

/// The document as a tree of at most 256 elements nested at most 8 deep.
pub(super) fn read(bytes: &[u8]) -> Result<Element, Error> {
    let mut stack: Vec<Element> = Vec::new();
    let mut root = None;
    let mut count = 0;
    walk(bytes, crate::json::RESOURCE_BODY_LIMIT, |node| {
        match node {
            Node::Start(name) => {
                if root.is_some() || stack.len() >= 8 || count >= 256 {
                    return Err(malformed());
                }
                stack.push(Element {
                    name: name.to_owned(),
                    text: String::new(),
                    children: Vec::new(),
                });
                count += 1;
            }
            Node::End => {
                let element = stack.pop().ok_or_else(malformed)?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(element),
                    None => root = Some(element),
                }
            }
            Node::Text(text) => stack.last_mut().ok_or_else(malformed)?.text.push_str(text),
        }
        Ok(())
    })?;
    root.ok_or_else(malformed)
}

fn reader(bytes: &[u8], limit: usize) -> Result<NsReader<&[u8]>, Error> {
    if bytes.len() > limit {
        return Err(malformed());
    }
    let input = std::str::from_utf8(bytes).map_err(|_| malformed())?;
    if !input.chars().all(xml_char) {
        return Err(malformed());
    }
    let mut reader = NsReader::from_str(input);
    reader.config_mut().expand_empty_elements = true;
    reader.config_mut().check_comments = true;
    reader.resolver_mut().set_max_namespace_bindings(128);
    Ok(reader)
}

fn resolved_namespace(resolved: &ResolveResult<'_>) -> Result<String, Error> {
    let namespace = match resolved {
        ResolveResult::Unbound => String::new(),
        ResolveResult::Bound(value) => quick_xml::escape::unescape(value.as_ref())
            .map_err(|_| malformed())?
            .into_owned(),
        ResolveResult::Unknown(_) => return Err(malformed()),
    };
    if !matches!(
        namespace.as_str(),
        "" | "http://doc.s3.amazonaws.com/2006-03-01" | "http://s3.amazonaws.com/doc/2006-03-01/"
    ) {
        return Err(malformed());
    }
    Ok(namespace)
}

fn xml_char(character: char) -> bool {
    matches!(character, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')
}

fn validate_attributes(start: &BytesStart<'_>) -> Result<(), Error> {
    for (index, attribute) in start.attributes().enumerate() {
        let attribute = attribute.map_err(|_| malformed())?;
        let name = attribute.key.as_ref();
        if index >= 16
            || !(name == "xmlns"
                || name
                    .strip_prefix("xmlns:")
                    .is_some_and(|name| !name.is_empty()))
        {
            return Err(malformed());
        }
        let value = attribute
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|_| malformed())?;
        if !value.chars().all(xml_char) {
            return Err(malformed());
        }
    }
    Ok(())
}

fn validate_declaration(declaration: &BytesDecl<'_>) -> Result<(), Error> {
    let start = BytesStart::from_content(declaration.as_ref(), 3);
    let mut previous = 0;
    for (index, attribute) in start.attributes().enumerate() {
        let attribute = attribute.map_err(|_| malformed())?;
        let order = match attribute.key.as_ref() {
            "version" if index == 0 && attribute.value == "1.0" => 1,
            "encoding" if attribute.value.eq_ignore_ascii_case("UTF-8") => 2,
            "standalone" if matches!(attribute.value.as_ref(), "yes" | "no") => 3,
            _ => return Err(malformed()),
        };
        if order <= previous || (index == 0 && order != 1) {
            return Err(malformed());
        }
        previous = order;
    }
    if previous == 0 {
        return Err(malformed());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_expands_only_standard_entities_once() {
        let root = read(b"<root>&amp;lt;&lt;&gt;&quot;&apos;&#x41;&#65;</root>").unwrap();
        assert_eq!(root.text, "&lt;<>\"'AA");
        for input in [
            "<root>&unknown;</root>",
            "<root>&#1;</root>",
            "<root>&#0;</root>",
            "<root>\u{1}</root>",
            "<root>]]></root>",
        ] {
            assert!(read(input.as_bytes()).is_err(), "{input}");
        }
    }

    #[test]
    fn document_bounds_depth_elements_and_namespace_attributes() {
        for (depth, pass) in [(8, true), (9, false)] {
            let input = format!("{}{}", "<node>".repeat(depth), "</node>".repeat(depth));
            assert_eq!(read(input.as_bytes()).is_ok(), pass);
        }
        for (children, pass) in [(255, true), (256, false)] {
            let input = format!("<root>{}</root>", "<child/>".repeat(children));
            assert_eq!(read(input.as_bytes()).is_ok(), pass);
        }
        for (count, pass) in [(16, true), (17, false)] {
            let attributes = (0..count)
                .map(|index| format!(" xmlns:p{index}='urn:{index}'"))
                .collect::<String>();
            assert_eq!(
                read(format!("<root{attributes}/>").as_bytes()).is_ok(),
                pass
            );
        }
    }
}
