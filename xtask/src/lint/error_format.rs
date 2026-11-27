//! A thiserror format that prints the same field its generated Error exposes as the source.

use super::{close_expression, close_group, ident_at, skip_space, words};

// These legacy single-field variants display and expose the very same error.
const RETAINED: &[(&str, &str, &[&str])] = &[
    ("src/fsutil.rs", "CloneFailure", &["NotCreated", "Created"]),
    ("src/config_input/options.rs", "DurationError", &["Number"]),
    (
        "src/config/notifications.rs",
        "EventSettingsError",
        &["Endpoint", "Topic", "ObjectPrefix"],
    ),
    (
        "src/bucket_init/generate.rs",
        "GenerateError",
        &["Rejected"],
    ),
    (
        "src/storage/catalog/resumable/writers.rs",
        "ClaimError",
        &["Catalog"],
    ),
    (
        "src/tls.rs",
        "ServerIdentityError",
        &["Certificate", "PrivateKey", "Identity", "Rejected"],
    ),
];

// These diagnostics embed the exposed source in a larger message. Matching their exact
// format and field count keeps other interpolations subject to the rule.
const PREFIXED: &[(&str, &str, &str, usize, &str)] = &[
    (
        "src/auth/policy.rs",
        "PolicyDecodeError",
        "Json",
        1,
        "invalid stored policy: {0}",
    ),
    (
        "src/auth/mod.rs",
        "",
        "IdentityError",
        2,
        "{setting}: {source}",
    ),
    (
        "src/storage/catalog/admin/purge.rs",
        "PurgeError",
        "Admin",
        1,
        "bucket purge: {0}",
    ),
    (
        "src/buckets_cli/prompts/names.rs",
        "NameError",
        "Invalid",
        2,
        "{name}: {error}",
    ),
    (
        "src/buckets_cli/prompts/lists.rs",
        "",
        "InvalidSeconds",
        1,
        "Cannot read the seconds: {0}",
    ),
    (
        "src/buckets_cli.rs",
        "SaveError",
        "Create",
        1,
        "Cannot create that file: {0}.",
    ),
    (
        "src/buckets_cli.rs",
        "SaveError",
        "Write",
        1,
        "Cannot write that file, so it was removed: {0}.",
    ),
];

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
        let value = value.trim().trim_start_matches('&').trim_start();
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
    }) || field.parse::<usize>().is_ok_and(|index| {
        index
            < arguments
                .iter()
                .take_while(|arg| !arg.contains('='))
                .count()
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

fn prints_source(file: &str, owner: &str, item: &str, literal: &str, arguments: &[&str]) -> bool {
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
    if arguments.is_empty() && PREFIXED.contains(&(file, owner, name, fields.len(), literal)) {
        return false;
    }
    if !named
        && fields.len() == 1
        && literal == "{0}"
        && arguments.is_empty()
        && RETAINED
            .iter()
            .any(|&(path, ty, variants)| path == file && ty == owner && variants.contains(&name))
    {
        return false;
    }
    fields.into_iter().enumerate().any(|(index, field)| {
        let (field, marked) = declaration(field);
        if named {
            let name = ident_at(field, 0);
            (marked || name == "source") && prints(literal, arguments, name)
        } else {
            marked && prints(literal, arguments, &index.to_string())
        }
    })
}

pub(super) fn findings(file: &str, text: &str, masked: &str) -> Vec<usize> {
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
            "struct" => prints_source(file, "", item[6..].trim_start(), literal, &args[1..]),
            "enum" => {
                let Some((header, body)) = item[4..].split_once('{') else {
                    continue;
                };
                let owner = ident_at(header.trim_start(), 0);
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
                            && prints_source(file, owner, decl, literal, &args[1..])
                    })
            }
            _ => {
                let owner = words(&masked[..attr], "enum")
                    .last()
                    .map_or("", |offset| ident_at(masked, skip_space(bytes, offset + 4)));
                prints_source(file, owner, item, literal, &args[1..])
            }
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
"##;
        assert_eq!(lines("src/example.rs", code), [2, 3, 4, 5, 6, 8, 9, 10]);
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
    #[error("{0}", .0.kind())] Positional(#[source] io::Error),
    #[error("{cause}")] Leaf { cause: String },
}
#[error("{0}")] enum Shared { #[error("context")] Override(#[source] io::Error), Leaf(String) }
// #[error("{0}")] struct Fake(#[source] io::Error);
#[cfg(test)]
mod tests { #[error("{0}")] struct Fixture(#[source] io::Error); }
"#;
        assert!(lines("src/example.rs", code).is_empty());
    }

    #[test]
    fn compatibility_requires_the_file_type_and_single_field_variant() {
        let code = r#"enum ServerIdentityError {
    #[error("{0}")] Certificate(#[source] io::Error),
    #[error("{0}")] Future(#[source] io::Error),
    #[error("{0}")] Identity(#[source] io::Error, usize),
    #[error("new prefix: {0}")] Rejected(#[source] io::Error),
}"#;
        assert_eq!(lines("src/tls.rs", code), [3, 4, 5]);
        assert_eq!(lines("src/other.rs", code), [2, 3, 4, 5]);
        let purge =
            r#"enum PurgeError { #[error("bucket purge: {0}")] Admin(#[source] AdminError) }"#;
        assert!(lines("src/storage/catalog/admin/purge.rs", purge).is_empty());
        assert_eq!(
            lines(
                "src/storage/catalog/admin/purge.rs",
                &purge.replace("bucket purge:", "new:")
            ),
            [1]
        );
        assert_eq!(
            lines("src/tls.rs", &code.replace("ServerIdentityError", "Other")),
            [2, 3, 4, 5]
        );
    }
}
