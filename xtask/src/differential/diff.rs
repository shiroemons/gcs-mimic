//! Difference computation between two normalized responses.

use super::case::CompareOpts;
use super::normalize::{NormResponse, body_value, time_placeholder_digits};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Difference {
    Status {
        reference: u16,
        mimic: u16,
    },
    Header {
        name: String,
        reference: Option<String>,
        mimic: Option<String>,
    },
    Body {
        path: String,
        reference: Option<Value>,
        mimic: Option<Value>,
    },
}

/// How a body writes the fractional seconds of a timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fractions {
    /// The server chose the digits, so their count is part of the contract.
    Exact,
    /// Protobuf JSON, as `grpcurl` prints it, drops trailing groups of three zero digits:
    /// a value stored to the microsecond shows six digits, or three in one case in a thousand.
    TrimmedGroups,
}

#[derive(Debug, Clone, PartialEq)]
enum Seg {
    Key(String),
    Index(usize),
    Any,
}

/// Parses `$.a.b[*].c` style paths; a pattern also covers everything below it.
fn parse_pattern(pattern: &str) -> Vec<Seg> {
    let rest = pattern.strip_prefix('$').unwrap_or(pattern);
    let mut segs = Vec::new();
    let mut key = String::new();
    let flush = |key: &mut String, segs: &mut Vec<Seg>| {
        if !key.is_empty() {
            segs.push(if key == "*" {
                Seg::Any
            } else {
                Seg::Key(std::mem::take(key))
            });
            key.clear();
        }
    };
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '.' => flush(&mut key, &mut segs),
            '[' => {
                flush(&mut key, &mut segs);
                let inner: String = chars.by_ref().take_while(|c| *c != ']').collect();
                segs.push(match inner.parse() {
                    Ok(index) => Seg::Index(index),
                    Err(_) => Seg::Any,
                });
            }
            c => key.push(c),
        }
    }
    flush(&mut key, &mut segs);
    segs
}

fn ignored(path: &[Seg], patterns: &[Vec<Seg>]) -> bool {
    patterns.iter().any(|pattern| {
        pattern.len() <= path.len()
            && pattern
                .iter()
                .zip(path)
                .all(|(p, s)| *p == Seg::Any || p == s)
    })
}

fn render(path: &[Seg]) -> String {
    let mut out = String::from("$");
    for seg in path {
        match seg {
            Seg::Key(key) if key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') => {
                out.push('.');
                out.push_str(key);
            }
            Seg::Key(key) => out.push_str(&format!("[{key:?}]")),
            Seg::Index(index) => out.push_str(&format!("[{index}]")),
            Seg::Any => out.push_str("[*]"),
        }
    }
    out
}

/// The recorded reference names the public API host `<www-host>`, while the
/// emulator answers with its own address, which is the correct self reference
/// for a local server. Both are the same place for a comparison.
fn same_after_host_alias(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (Some(Value::String(a)), Some(Value::String(b))) => {
            a.contains("<www-host>") && a.replace("<www-host>", "<host>") == *b
        }
        _ => false,
    }
}

/// Whether the emulator's timestamp is the reference's precision with trailing zero groups
/// dropped. A value with more digits than the reference is still a difference; the digits the
/// emulator keeps are pinned by its own unit tests, which a single sampled value cannot do.
fn same_after_zero_trimming(
    reference: Option<&Value>,
    mimic: Option<&Value>,
    fractions: Fractions,
) -> bool {
    let digits = |value: Option<&Value>| value?.as_str().and_then(time_placeholder_digits);
    match (fractions, digits(reference), digits(mimic)) {
        (Fractions::TrimmedGroups, Some(reference), Some(mimic)) => {
            mimic < reference && reference % 3 == 0 && mimic % 3 == 0
        }
        _ => false,
    }
}

fn walk(
    reference: Option<&Value>,
    mimic: Option<&Value>,
    path: &mut Vec<Seg>,
    patterns: &[Vec<Seg>],
    fractions: Fractions,
    out: &mut Vec<Difference>,
) {
    if ignored(path, patterns) {
        return;
    }
    match (reference, mimic) {
        (None, None) => {}
        (Some(Value::Object(a)), Some(Value::Object(b))) => {
            let mut keys: Vec<_> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                path.push(Seg::Key(key.clone()));
                walk(a.get(key), b.get(key), path, patterns, fractions, out);
                path.pop();
            }
        }
        (Some(Value::Array(a)), Some(Value::Array(b))) => {
            for index in 0..a.len().max(b.len()) {
                path.push(Seg::Index(index));
                walk(a.get(index), b.get(index), path, patterns, fractions, out);
                path.pop();
            }
        }
        (a, b)
            if a == b
                || same_after_host_alias(a, b)
                || same_after_zero_trimming(a, b, fractions) => {}
        (a, b) => out.push(Difference::Body {
            path: render(path),
            reference: a.cloned(),
            mimic: b.cloned(),
        }),
    }
}

pub(crate) fn diff_json(
    reference: Option<&Value>,
    mimic: Option<&Value>,
    ignore: &[String],
    fractions: Fractions,
) -> Vec<Difference> {
    let patterns: Vec<_> = ignore.iter().map(|p| parse_pattern(p)).collect();
    let mut out = Vec::new();
    walk(
        reference,
        mimic,
        &mut Vec::new(),
        &patterns,
        fractions,
        &mut out,
    );
    out
}

pub(crate) fn compare(
    reference: &NormResponse,
    mimic: &NormResponse,
    opts: &CompareOpts,
    fractions: Fractions,
) -> Vec<Difference> {
    let mut out = Vec::new();
    if reference.status != mimic.status {
        out.push(Difference::Status {
            reference: reference.status,
            mimic: mimic.status,
        });
    }
    let keep = |name: &String| {
        let lower = name.to_ascii_lowercase();
        !opts
            .ignore_headers
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&lower))
            && opts
                .only_headers
                .as_ref()
                .is_none_or(|only| only.iter().any(|h| h.eq_ignore_ascii_case(&lower)))
    };
    let mut names: Vec<_> = reference
        .headers
        .keys()
        .chain(mimic.headers.keys())
        .collect();
    names.sort();
    names.dedup();
    for name in names.into_iter().filter(|n| keep(n)) {
        let (a, b) = (reference.headers.get(name), mimic.headers.get(name));
        if a != b {
            out.push(Difference::Header {
                name: name.clone(),
                reference: a.cloned(),
                mimic: b.cloned(),
            });
        }
    }
    let a = body_value(&reference.body, opts.body);
    let b = body_value(&mimic.body, opts.body);
    out.extend(diff_json(
        a.as_ref(),
        b.as_ref(),
        &opts.ignore_body_paths,
        fractions,
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::super::normalize::NormBody;
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn response(status: u16, headers: &[(&str, &str)], body: NormBody) -> NormResponse {
        NormResponse {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect::<BTreeMap<_, _>>(),
            body,
        }
    }

    #[test]
    fn reports_paths_for_missing_extra_and_changed_fields() {
        let a = json!({"a": 1, "b": {"c": [1, 2]}, "only_ref": true});
        let b = json!({"a": 2, "b": {"c": [1]}, "only_mimic": "x"});
        let diffs = diff_json(Some(&a), Some(&b), &[], Fractions::Exact);
        let paths: Vec<_> = diffs
            .iter()
            .map(|d| match d {
                Difference::Body { path, .. } => path.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(paths, ["$.a", "$.b.c[1]", "$.only_mimic", "$.only_ref"]);
    }

    #[test]
    fn ignore_patterns_cover_subtrees_and_wildcards() {
        let a = json!({"items": [{"id": 1, "n": "a"}, {"id": 2, "n": "b"}], "x": {"y": 1}});
        let b = json!({"items": [{"id": 9, "n": "a"}, {"id": 8, "n": "b"}], "x": {"y": 2}});
        let ignore = vec!["$.items[*].id".to_owned(), "$.x".to_owned()];
        assert!(diff_json(Some(&a), Some(&b), &ignore, Fractions::Exact).is_empty());
        assert_eq!(
            diff_json(Some(&a), Some(&b), &["$.x".to_owned()], Fractions::Exact).len(),
            2
        );
    }

    #[test]
    fn compares_status_headers_and_bodies_with_overrides() {
        let r = response(
            404,
            &[("vary", "Origin"), ("cache-control", "no-cache")],
            NormBody::Json(json!({"e": 1})),
        );
        let m = response(
            400,
            &[("vary", "Origin"), ("pragma", "no-cache")],
            NormBody::Json(json!({"e": 1})),
        );
        let diffs = compare(&r, &m, &CompareOpts::default(), Fractions::Exact);
        assert_eq!(diffs.len(), 3);
        let opts = CompareOpts {
            only_headers: Some(vec!["vary".into()]),
            ..CompareOpts::default()
        };
        assert_eq!(
            compare(&r, &m, &opts, Fractions::Exact),
            [Difference::Status {
                reference: 404,
                mimic: 400
            }]
        );
        let opts = CompareOpts {
            ignore_headers: vec!["Cache-Control".into(), "pragma".into()],
            ..CompareOpts::default()
        };
        assert_eq!(compare(&r, &m, &opts, Fractions::Exact).len(), 1);
    }

    #[test]
    fn the_recorded_api_host_equals_the_emulator_host() {
        let r = response(
            200,
            &[],
            NormBody::Json(json!({"selfLink": "<www-host>/storage/v1/b/x"})),
        );
        let m = response(
            200,
            &[],
            NormBody::Json(json!({"selfLink": "<host>/storage/v1/b/x"})),
        );
        assert!(compare(&r, &m, &CompareOpts::default(), Fractions::Exact).is_empty());
        let other = response(
            200,
            &[],
            NormBody::Json(json!({"selfLink": "<host>/storage/v1/b/y"})),
        );
        assert_eq!(
            compare(&r, &other, &CompareOpts::default(), Fractions::Exact).len(),
            1
        );
        let acl = |host: &str, email: &str| json!({"acl": [{"selfLink": format!("{host}/storage/v1/b/x/o/y/acl/user-{email}")}]});
        let reference = acl("<www-host>", "<email>");
        assert!(
            diff_json(
                Some(&reference),
                Some(&acl("<host>", "<email>")),
                &[],
                Fractions::Exact
            )
            .is_empty()
        );
        assert_eq!(
            diff_json(
                Some(&reference),
                Some(&acl("<host>", "<email:percent-encoded>")),
                &[],
                Fractions::Exact
            )
            .len(),
            1
        );
    }

    /// One microsecond value in a thousand ends in three zeros, which protobuf JSON drops. Only
    /// gRPC bodies get that allowance, and never for more digits than the reference shows.
    #[test]
    fn grpc_timestamps_may_drop_trailing_zero_groups() {
        let body = |time: &str| json!({"finalizeTime": time, "createTime": "<time:f3>"});
        let reference = body("<time:f6>");
        for (mimic, fractions, differences) in [
            (body("<time:f3>"), Fractions::TrimmedGroups, 0),
            (body("<time:f0>"), Fractions::TrimmedGroups, 0),
            (body("<time:f6>"), Fractions::TrimmedGroups, 0),
            (body("<time:f3>"), Fractions::Exact, 1),
            (body("<time:f9>"), Fractions::TrimmedGroups, 1),
            (body("<time:f5>"), Fractions::TrimmedGroups, 1),
            (body("<time:str>"), Fractions::TrimmedGroups, 1),
        ] {
            assert_eq!(
                diff_json(Some(&reference), Some(&mimic), &[], fractions).len(),
                differences,
                "{mimic} under {fractions:?}"
            );
        }
        let more_digits = json!({"finalizeTime": "<time:f6>", "createTime": "<time:f6>"});
        assert_eq!(
            diff_json(
                Some(&reference),
                Some(&more_digits),
                &[],
                Fractions::TrimmedGroups
            ),
            [Difference::Body {
                path: "$.createTime".into(),
                reference: Some(json!("<time:f3>")),
                mimic: Some(json!("<time:f6>")),
            }]
        );
    }

    #[test]
    fn empty_versus_present_body_is_a_difference() {
        let r = response(200, &[], NormBody::Empty);
        let m = response(200, &[], NormBody::Text("x".into()));
        assert_eq!(
            compare(&r, &m, &CompareOpts::default(), Fractions::Exact).len(),
            1
        );
        let opts = CompareOpts {
            body: super::super::case::BodyMode::Ignore,
            ..CompareOpts::default()
        };
        assert!(compare(&r, &m, &opts, Fractions::Exact).is_empty());
    }
}
