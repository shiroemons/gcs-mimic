//! A thiserror format that prints the same field its generated Error exposes as the source.

use super::{close_expression, close_group, ident_at, skip_space, words};

/// Split an argument, variant or field list at its own commas. Groups can contain commas
/// of their own, including in a generic field type, an attribute or a format expression.
fn entries(input: &str, types: bool) -> Vec<&str> {
    let bytes = input.as_bytes();
    let mut items = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < bytes.len() {
        match bytes[i] {
            b',' => {
                items.push(&input[start..i]);
                start = i + 1;
            }
            b'(' | b'[' | b'{' => {
                i = close_expression(bytes, i).unwrap_or(bytes.len());
                continue;
            }
            b'<' if types => {
                i = close_group(bytes, i).unwrap_or(bytes.len());
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    if !input[start..].trim().is_empty() {
        items.push(&input[start..]);
    }
    items
}

/// Attributes and visibility precede both a type declaration and a named source field.
fn declaration(mut input: &str) -> (&str, bool) {
    input = input.trim_start();
    let mut marked = false;
    while let Some(attr) = input.strip_prefix('#').map(str::trim_start)
        && attr.starts_with('[')
        && let Some(end) = close_expression(attr.as_bytes(), 0)
    {
        marked |= matches!(attr[1..end - 1].trim(), "source" | "from");
        input = attr[end..].trim_start();
    }
    if ident_at(input, 0) == "pub" {
        input = input[3..].trim_start();
        if input.starts_with('(')
            && let Some(end) = close_expression(input.as_bytes(), 0)
        {
            input = input[end..].trim_start();
        }
    }
    (input, marked)
}

fn prints(literal: &str, arguments: &[&str], field: &str) -> bool {
    // A method such as source.kind() reports a property of the cause. Printing the
    // field as a format argument (possibly under an alias) prints the cause itself.
    if arguments.iter().any(|arg| {
        let value = arg.split_once('=').map_or(*arg, |(_, value)| value);
        let value = value.trim().trim_start_matches(['&', '*', ' ']);
        let value = value.strip_suffix(".to_string()").unwrap_or(value);
        let value = value.strip_suffix(".as_ref()").unwrap_or(value);
        value
            .strip_prefix("self.")
            .or_else(|| value.strip_prefix('.'))
            .unwrap_or(value)
            == field
    }) {
        return true;
    }
    if arguments.iter().any(|arg| {
        arg.split_once('=')
            .is_some_and(|(name, _)| name.trim() == field)
    }) {
        return false;
    }
    let mut rest = literal;
    while let Some((_, after)) = rest.split_once('{') {
        if let Some(escaped) = after.strip_prefix('{') {
            rest = escaped;
            continue;
        }
        let Some((placeholder, tail)) = after.split_once('}') else {
            break;
        };
        if placeholder
            .split(':')
            .next()
            .is_some_and(|name| name.trim() == field)
        {
            return true;
        }
        rest = tail;
    }
    false
}

fn prints_source(item: &str, literal: &str, arguments: &[&str]) -> bool {
    let name = ident_at(item, 0);
    let mut fields = item[name.len()..].trim_start();
    if fields.starts_with('<')
        && let Some(end) = close_group(fields.as_bytes(), 0)
    {
        fields = fields[end..].trim_start();
    }
    if ident_at(fields, 0) == "where" {
        let Some(open) = fields
            .find(['{', ';'])
            .filter(|&offset| fields.as_bytes()[offset] == b'{')
        else {
            return false;
        };
        fields = &fields[open..];
    }
    let named = fields.starts_with('{');
    if !(named || fields.starts_with('(')) {
        return false;
    }
    let Some(end) = close_expression(fields.as_bytes(), 0) else {
        return false;
    };
    let fields = entries(&fields[1..end - 1], true);
    let mut implicit = false;
    for (index, field) in fields.into_iter().enumerate() {
        let (field, marked) = declaration(field);
        let name = ident_at(field, 0);
        if marked {
            return if named {
                prints(literal, arguments, name)
            } else {
                prints(literal, arguments, &index.to_string())
            };
        }
        implicit |= named && name == "source";
    }
    implicit && prints(literal, arguments, "source")
}

pub(super) fn findings(text: &str, masked: &str) -> Vec<usize> {
    let bytes = masked.as_bytes();
    let mut hits = Vec::new();
    for attr in words(masked, "error") {
        if !masked[..attr]
            .trim_end()
            .strip_suffix('[')
            .is_some_and(|s| s.trim_end().ends_with('#'))
        {
            continue;
        }
        let open = skip_space(bytes, attr + 5);
        if bytes.get(open) != Some(&b'(') {
            continue;
        }
        let Some(end) = close_expression(bytes, open) else {
            continue;
        };
        let args = entries(&masked[open + 1..end - 1], false);
        let Some(first) = args.first() else { continue };
        let Some(start_quote) = first.find('"') else {
            continue;
        };
        let Some(end_quote) = first.rfind('"').filter(|&i| i > start_quote) else {
            continue;
        };
        let literal = &text[open + 2 + start_quote..open + 1 + end_quote];
        let Some(tail) = masked[end..].trim_start().strip_prefix(']') else {
            continue;
        };
        let (item, _) = declaration(tail);
        let hit = match ident_at(item, 0) {
            "struct" => prints_source(item[6..].trim_start(), literal, &args[1..]),
            "enum" => {
                let Some((_, body)) = item[4..].split_once('{') else {
                    continue;
                };
                let open = item.len() - body.len() - 1;
                let Some(close) = close_expression(item.as_bytes(), open) else {
                    continue;
                };
                entries(&item[open + 1..close - 1], true)
                    .into_iter()
                    .any(|variant| {
                        let (decl, _) = declaration(variant);
                        let attrs = &variant[..variant.len() - decl.len()];
                        words(attrs, "error").next().is_none()
                            && prints_source(decl, literal, &args[1..])
                    })
            }
            _ => prints_source(item, literal, &args[1..]),
        };
        if hit {
            hits.push(attr);
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use crate::lint::{ERROR_DISPLAYS_SOURCE, scan_source};

    fn lines(path: &str, code: &str) -> Vec<usize> {
        scan_source(path, code)
            .findings
            .into_iter()
            .filter(|finding| finding.rule == ERROR_DISPLAYS_SOURCE)
            .map(|finding| finding.line)
            .collect()
    }

    #[test]
    fn rejects_interpolation_of_the_source_field() {
        let code = r##"enum Failure {
    #[error("prefix: {0}")] Io(#[source] io::Error),
    #[error("second: {1:?}")] Second(u8, #[from] io::Error),
    #[error(r#"bad {cause:?}"#)] Named { #[source] cause: io::Error },
    #[error("bad {}", .source)] Explicit { source: io::Error },
    #[error("{why}", why = .1)] Alias(String, #[source] io::Error),
}
#[error("bad {source}")] pub(crate) struct Outer { pub source: io::Error }
#[error("{0}")] enum Shared { Wrapped(#[source] io::Error), Other(String) }
#[error("{source}")] struct Generic<T> where T: Error { source: T }
#[error(
    "cannot read: {}",
    .reason.to_string()
)] struct Wrapped { #[from] reason: io::Error }
#[error("{}", .0.as_ref())] struct Boxed(#[source] Box<io::Error>);
#[error("{}", &*.source)] struct Borrowed { source: Box<io::Error> }
"##;
        assert_eq!(
            lines("src/example.rs", code),
            [2, 3, 4, 5, 6, 8, 9, 10, 11, 15, 16]
        );
    }

    #[test]
    fn ignores_context_other_fields_escaped_braces_and_transparent_errors() {
        let code = r#"
enum Error {
    #[error("{{source}}: {name}")] Named { name: String, source: io::Error },
    #[error(transparent)] Transparent(#[from] io::Error),
    #[error("{0}")] Other(String, #[source] io::Error),
    #[error("{}", .source.kind())] Kind { source: io::Error },
    #[error("{source}", source = .source.kind())] Alias { source: io::Error },
    #[error("{cause}")] Leaf { cause: String },
    #[error("{source}")] Explicit { source: String, #[source] cause: io::Error },
}
#[error("{0}")] enum Shared { #[error("context")] Override(#[source] io::Error), Leaf(String) }
// #[error("{0}")] struct Fake(#[source] io::Error);
#[cfg(test)]
mod tests { #[error("{0}")] struct Fixture(#[source] io::Error); }
"#;
        assert!(lines("src/example.rs", code).is_empty());
    }

    #[test]
    fn source_suppression_is_counted_and_expires_when_the_source_is_forwarded() {
        let code = r#"enum Error {
    // xtask-lint: allow(text::error_displays_source) -- Callers inspect and print this same error.
    #[error("{0}")] Io(#[source] io::Error),
    #[error("{0}")] Other(#[source] io::Error),
}"#;
        let scan = scan_source("src/example.rs", code);
        assert!(scan.problems.is_empty());
        assert_eq!(scan.suppressed.len(), 1);
        assert_eq!(scan.suppressed[0].rule, ERROR_DISPLAYS_SOURCE);
        assert_eq!(lines("src/example.rs", code), [4]);
        let fixed = code.replace(
            r#"#[error("{0}")] Io(#[source] io::Error)"#,
            "#[error(transparent)] Io(io::Error)",
        );
        let scan = scan_source("src/example.rs", &fixed);
        assert!(scan.suppressed.is_empty());
        assert_eq!(scan.problems.len(), 1);
        assert!(scan.problems[0].message.starts_with("stale suppression"));
    }
}
