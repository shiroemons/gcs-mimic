//! Comparison results, Markdown and JSON rendering, and the redaction guard.

use super::case::{Case, fingerprint};
use super::diff::{Difference, compare};
use super::runner::{CaseOutcome, StepOutcome};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// No difference from the reference (or, in self-check mode, all steps ran).
    Match,
    Differs,
    /// Only steps missed their `expect_status` on the gcs-mimic side: a gap in
    /// gcs-mimic that the run reports, not a harness failure.
    Noted,
    Error,
}

impl Outcome {
    fn label(self, reference: bool) -> &'static str {
        match (self, reference) {
            (Self::Match, true) => "match",
            (Self::Match, false) => "ok",
            (Self::Differs, _) => "differs",
            (Self::Noted, _) => "note",
            (Self::Error, _) => "error",
        }
    }
}

pub(crate) struct StepReport {
    pub(crate) name: String,
    pub(crate) request: String,
    pub(crate) differences: Vec<Difference>,
    pub(crate) notes: Vec<String>,
}

pub(crate) struct CaseReport {
    pub(crate) id: String,
    pub(crate) description: String,
    pub(crate) outcome: Outcome,
    pub(crate) errors: Vec<String>,
    pub(crate) steps: Vec<StepReport>,
}

fn note_expectation(label: &str, step: &StepOutcome, notes: &mut Vec<String>) {
    if let Some(expectation) = &step.expectation {
        notes.push(format!("{label}: {expectation}"));
    }
}

fn compare_case(case: &Case, mimic: &CaseOutcome, reference: Option<&CaseOutcome>) -> CaseReport {
    let mut errors = Vec::new();
    if let Some(error) = &mimic.error {
        errors.push(format!("mimic: {error}"));
    }
    if let Some(reference) = reference {
        if let Some(error) = &reference.error {
            errors.push(format!("reference: {error}"));
        }
        // A case that moved between the shared bucket and its own one answers a
        // different question, though its steps are unchanged.
        if let Some(recorded) = &reference.bucket
            && recorded != case.bucket.label()
        {
            errors.push(format!(
                "bucket mode changed from '{recorded}' to '{}' since the reference was recorded; record again",
                case.bucket.label()
            ));
        }
        let expected: Vec<_> = case.steps.iter().map(|s| s.name.clone()).collect();
        let recorded: Vec<_> = reference.steps.iter().map(|s| s.name.clone()).collect();
        if recorded != expected {
            errors.push("reference recording does not match the case's steps; record again".into());
        } else if let Some(step) = case
            .steps
            .iter()
            .zip(&reference.steps)
            .find(|(spec, recorded)| fingerprint(spec) != recorded.fingerprint)
            .map(|(spec, _)| &spec.name)
        {
            // Checked even when the recording holds an error: a changed
            // request means the reference answered a different question.
            errors.push(format!(
                "step {step:?} changed since the reference was recorded; record again"
            ));
        }
    }
    let mut steps = Vec::new();
    for (index, spec) in case.steps.iter().enumerate() {
        let mimic_step = mimic.steps.get(index);
        let reference_step = reference.and_then(|r| r.steps.get(index));
        let mut report = StepReport {
            name: spec.name.clone(),
            request: mimic_step.map_or_else(String::new, |s| s.request.clone()),
            differences: Vec::new(),
            notes: Vec::new(),
        };
        if let Some(step) = mimic_step {
            note_expectation("mimic", step, &mut report.notes);
        }
        if let Some(step) = reference_step {
            note_expectation("reference", step, &mut report.notes);
        }
        if let (Some(opts), Some(Ok(m)), Some(Ok(r))) = (
            spec.compare.as_ref(),
            mimic_step.map(|s| &s.result),
            reference_step.map(|s| &s.result),
        ) {
            report.differences = compare(r, m, opts);
        }
        if mimic_step.is_some() || reference_step.is_some() {
            steps.push(report);
        }
    }
    let has_diffs = steps.iter().any(|s| !s.differences.is_empty());
    // An unmet expectation on the real service means the case itself is wrong,
    // which fails the run unless differences are reported anyway; on gcs-mimic
    // it is a finding, like a difference.
    let reference_note = steps
        .iter()
        .flat_map(|s| &s.notes)
        .any(|n| n.starts_with("reference:"));
    let has_notes = steps.iter().any(|s| !s.notes.is_empty());
    let outcome = if !errors.is_empty() || (reference_note && !has_diffs) {
        Outcome::Error
    } else if has_diffs {
        Outcome::Differs
    } else if has_notes {
        Outcome::Noted
    } else {
        Outcome::Match
    };
    CaseReport {
        id: case.id.clone(),
        description: case.description.clone(),
        outcome,
        errors,
        steps,
    }
}

pub(crate) fn build(
    cases: &[Case],
    mimic: &[CaseOutcome],
    reference: Option<&[CaseOutcome]>,
) -> Vec<CaseReport> {
    cases
        .iter()
        .map(|case| {
            let mimic_outcome = mimic.iter().find(|o| o.id == case.id);
            let reference_outcome = reference.and_then(|r| r.iter().find(|o| o.id == case.id));
            let missing = CaseOutcome {
                id: case.id.clone(),
                steps: Vec::new(),
                error: Some("case did not run".into()),
                bucket: None,
            };
            let mut report =
                compare_case(case, mimic_outcome.unwrap_or(&missing), reference_outcome);
            if reference.is_some() && reference_outcome.is_none() {
                report
                    .errors
                    .push("reference has no recording for this case".into());
                report.outcome = Outcome::Error;
            }
            report
        })
        .collect()
}

pub(crate) struct Header<'a> {
    pub(crate) mode: &'a str,
    pub(crate) generated: &'a str,
    pub(crate) reference_requests: Option<u64>,
    /// 429 responses the live run waited out and retried, over all steps.
    pub(crate) reference_retries: u32,
    pub(crate) cleanup: &'a [String],
    pub(crate) reference_cleanup: &'a [String],
    /// Cases not run (for example gRPC cases without `grpcurl`), one line each.
    pub(crate) skipped: &'a [String],
}

fn show(value: &Option<Value>) -> String {
    let mut text = value
        .as_ref()
        .map_or_else(|| "(absent)".to_owned(), Value::to_string);
    if text.chars().count() > 160 {
        text = text.chars().take(157).collect::<String>() + "...";
    }
    text.replace('`', "'")
}

fn show_header(value: &Option<String>) -> String {
    value
        .as_ref()
        .map_or_else(|| "(absent)".to_owned(), |v| v.replace('`', "'"))
}

fn describe(difference: &Difference) -> String {
    match difference {
        Difference::Status { reference, mimic } => {
            format!("status: reference `{reference}`, mimic `{mimic}`")
        }
        Difference::Header {
            name,
            reference,
            mimic,
        } => format!(
            "header `{name}`: reference `{}`, mimic `{}`",
            show_header(reference),
            show_header(mimic)
        ),
        Difference::Body {
            path,
            reference,
            mimic,
        } => format!(
            "body `{path}`: reference `{}`, mimic `{}`",
            show(reference),
            show(mimic)
        ),
    }
}

pub(crate) fn markdown(header: &Header<'_>, reports: &[CaseReport]) -> String {
    let reference = header.reference_requests.is_some() || header.mode.starts_with("replay");
    let mut out = String::from(
        "<!-- markdownlint-configure-file {\"MD013\": false} -->\n\n# Differential comparison report\n\n",
    );
    out.push_str(&format!(
        "- Mode: {}\n- Generated: {}\n",
        header.mode, header.generated
    ));
    if let Some(requests) = header.reference_requests {
        out.push_str(&format!(
            "- Requests sent to the live reference service (when recorded): {requests}\n- HTTP 429 responses retried by the live run: {}\n", header.reference_retries
        ));
    }
    out.push_str("\n## Summary\n\n| Case | Result | Differing fields |\n| --- | --- | --- |\n");
    for report in reports {
        let count: usize = report.steps.iter().map(|s| s.differences.len()).sum();
        out.push_str(&format!(
            "| {} | {} | {count} |\n",
            report.id,
            report.outcome.label(reference)
        ));
    }
    out.push_str("\n## Details\n");
    if reports
        .iter()
        .all(|r| r.outcome == Outcome::Match && r.errors.is_empty())
    {
        out.push_str("\nNo differences or errors.\n");
    }
    for report in reports {
        if report.outcome == Outcome::Match && report.errors.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "\n### {}\n\n{}\n\n",
            report.id, report.description
        ));
        for error in &report.errors {
            out.push_str(&format!("- error: {}\n", error.replace('`', "'")));
        }
        for step in &report.steps {
            if step.differences.is_empty() && step.notes.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "- step `{}` (`{}`)\n",
                step.name,
                step.request.replace('`', "'")
            ));
            for note in &step.notes {
                out.push_str(&format!("  - {note}\n"));
            }
            for difference in &step.differences {
                out.push_str(&format!("  - {}\n", describe(difference)));
            }
        }
    }
    if !header.skipped.is_empty() {
        out.push_str("\n## Skipped\n\n");
        for line in header.skipped {
            out.push_str(&format!("- {line}\n"));
        }
    }
    out.push_str("\n## Cleanup\n\n");
    // Raw error text (URLs, JSON, angle brackets) would break the Markdown
    // lint outside a code block.
    out.push_str("```text\n");
    for (label, lines) in [
        ("mimic", header.cleanup),
        ("reference", header.reference_cleanup),
    ] {
        for line in lines {
            out.push_str(&format!("- {label} {}\n", line.replace("```", "'''")));
        }
    }
    out.push_str("```\n");
    out
}

fn difference_json(difference: &Difference) -> Value {
    match difference {
        Difference::Status { reference, mimic } => {
            json!({"kind": "status", "reference": reference, "mimic": mimic})
        }
        Difference::Header {
            name,
            reference,
            mimic,
        } => {
            json!({"kind": "header", "name": name, "reference": reference, "mimic": mimic})
        }
        Difference::Body {
            path,
            reference,
            mimic,
        } => {
            json!({"kind": "body", "path": path, "reference": reference, "mimic": mimic})
        }
    }
}

pub(crate) fn json_report(header: &Header<'_>, reports: &[CaseReport]) -> Value {
    let reference = header.reference_requests.is_some() || header.mode.starts_with("replay");
    json!({
        "mode": header.mode,
        "generated": header.generated,
        "reference_requests": header.reference_requests,
        "reference_retries": header.reference_retries,
        "cleanup": header.cleanup,
        "reference_cleanup": header.reference_cleanup,
        "skipped": header.skipped,
        "cases": reports.iter().map(|r| json!({
            "id": r.id,
            "result": r.outcome.label(reference),
            "errors": r.errors,
            "steps": r.steps.iter().map(|s| json!({
                "name": s.name,
                "request": s.request,
                "notes": s.notes,
                "differences": s.differences.iter().map(difference_json).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// Local path prefixes; a path is redacted from its prefix to the next
/// whitespace, quote or backslash, so the user name and everything after it go too.
const PATH_PREFIXES: [&str; 5] = ["/Users/", "/private/", "/var/folders", "/tmp/", "/home/"];

fn path_ends_at(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '\\')
}

/// The text with every local path replaced as a whole by `<path>`.
fn redact_paths(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = PATH_PREFIXES
        .iter()
        .filter_map(|prefix| rest.find(prefix))
        .min()
    {
        out.push_str(&rest[..start]);
        out.push_str("<path>");
        let tail = &rest[start..];
        let end = tail.find(path_ends_at).unwrap_or(tail.len());
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// What is wrong with one piece of text, if anything: a label, never the value.
fn leak_of(text: &str, secrets: &[(String, &str)]) -> Option<String> {
    if let Some((_, label)) = secrets
        .iter()
        .find(|(value, _)| value.len() >= 3 && text.contains(value.as_str()))
    {
        return Some(format!("the {label}"));
    }
    if super::normalize::contains_email(text) {
        return Some("an email address".into());
    }
    PATH_PREFIXES
        .into_iter()
        .find(|path| text.contains(path))
        .map(|path| format!("a local path ({path})"))
}

/// A piece of text with every secret, email and local path replaced.
fn redact_text(text: &str, secrets: &[(String, &str)]) -> String {
    let mut out = text.to_owned();
    for (value, _) in secrets {
        if value.len() >= 3 {
            out = out.replace(value.as_str(), "<redacted>");
        }
    }
    out = super::normalize::scrub_emails(&out).replace("<email>", "<redacted>");
    redact_paths(&out)
}

/// A readable name for an array element: its `id` or `name`, else its index.
/// A label that would itself leak is replaced by the index.
fn element_label(index: usize, item: &Value, secrets: &[(String, &str)]) -> String {
    ["id", "name"]
        .into_iter()
        .find_map(|key| item[key].as_str())
        .filter(|label| leak_of(label, secrets).is_none())
        .map_or_else(|| index.to_string(), str::to_owned)
}

fn key_label(key: &str, secrets: &[(String, &str)]) -> String {
    if leak_of(key, secrets).is_some() {
        "<redacted-key>".into()
    } else {
        key.to_owned()
    }
}

fn walk_leaks(value: &Value, path: &str, secrets: &[(String, &str)], out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if let Some(leak) = leak_of(text, secrets) {
                out.push(format!("{}: {leak}", redact_text(path, secrets)));
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                let label = element_label(index, item, secrets);
                walk_leaks(item, &format!("{path}[{label}]"), secrets, out);
            }
        }
        Value::Object(map) => {
            for (key, item) in map {
                let label = key_label(key, secrets);
                if let Some(leak) = leak_of(key, secrets) {
                    out.push(format!(
                        "{}: a key holds {leak}",
                        redact_text(path, secrets)
                    ));
                }
                walk_leaks(item, &format!("{path}.{label}"), secrets, out);
            }
        }
        _ => {}
    }
}

/// Where known secrets, emails or local paths remain in `text`: a JSON path
/// (cases and steps by id or name, headers by name) for a JSON document, line
/// numbers otherwise. Values never appear in the result, and the locations
/// themselves are redacted too.
pub(crate) fn find_leaks(text: &str, secrets: &[(String, &str)]) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        walk_leaks(&value, "$", secrets, &mut out);
    } else {
        for (index, line) in text.lines().enumerate() {
            if let Some(leak) = leak_of(line, secrets) {
                out.push(format!("line {}: {leak}", index + 1));
            }
        }
    }
    // Object key order depends on whether serde_json's `preserve_order` is unified into the
    // build (it is in the workspace, not in xtask alone), so sort for a stable report.
    out.sort();
    out
}

fn redact_value(value: &Value, secrets: &[(String, &str)]) -> Value {
    match value {
        Value::String(text) => Value::String(redact_text(text, secrets)),
        Value::Array(items) => {
            Value::Array(items.iter().map(|i| redact_value(i, secrets)).collect())
        }
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (redact_text(key, secrets), redact_value(item, secrets)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// The text with every remaining secret, email and local path replaced, so it
/// can be kept for inspection after a leak was found. A JSON document is
/// redacted value by value (detection and replacement see the same string);
/// anything else as plain text.
pub(crate) fn redact_leaks(text: &str, secrets: &[(String, &str)]) -> String {
    match serde_json::from_str::<Value>(text) {
        Ok(value) => {
            let mut out = redact_value(&value, secrets).to_string();
            if text.ends_with('\n') {
                out.push('\n');
            }
            out
        }
        Err(_) => redact_text(text, secrets),
    }
}

/// Which findings fail the run; harness errors always do.
#[derive(Clone, Copy, Default)]
pub(crate) struct FailOn {
    pub(crate) diff: bool,
    pub(crate) note: bool,
}

pub(crate) fn has_failures(reports: &[CaseReport], fail_on: FailOn) -> bool {
    reports.iter().any(|r| match r.outcome {
        Outcome::Error => true,
        Outcome::Differs => fail_on.diff,
        Outcome::Noted => fail_on.note,
        Outcome::Match => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_rejects_secrets_and_local_paths() {
        let secrets = vec![("my-project".to_owned(), "project")];
        assert!(find_leaks("clean <project>", &secrets).is_empty());
        assert_eq!(
            find_leaks("x my-project", &secrets),
            ["line 1: the project"]
        );
        assert_eq!(find_leaks("see /Users/a/b", &secrets).len(), 1);
        assert_eq!(
            find_leaks("mail someone@example.com", &secrets),
            ["line 1: an email address"]
        );
        assert_eq!(find_leaks("see /var/folders/x", &secrets).len(), 1);
    }

    #[test]
    fn replay_reports_a_case_that_changed_since_the_recording() {
        use super::super::case::{fingerprint, parse_case};
        use super::super::normalize::{NormBody, NormResponse};
        use std::collections::BTreeMap;
        let case_json = |path: &str| {
            json!({"id": "c", "description": "d", "bucket": false,
                   "steps": [{"name": "s", "method": "GET", "path": path}]})
        };
        let recorded_case = parse_case(&case_json("/old"), "c").unwrap();
        let current = parse_case(&case_json("/new"), "c").unwrap();
        let outcome = |case: &Case, error: Option<&str>| CaseOutcome {
            id: "c".into(),
            bucket: Some(case.bucket.label().into()),
            error: error.map(str::to_owned),
            steps: vec![StepOutcome {
                name: "s".into(),
                request: "GET /x".into(),
                result: Ok(NormResponse {
                    status: 200,
                    headers: BTreeMap::new(),
                    body: NormBody::Empty,
                }),
                expectation: None,
                retries: 0,
                fingerprint: fingerprint(&case.steps[0]),
            }],
        };
        let mimic = outcome(&current, None);
        let clean = compare_case(&current, &mimic, Some(&outcome(&current, None)));
        assert!(clean.errors.is_empty(), "{:?}", clean.errors);
        // A recording taken from the old definition is refused, also when it holds an error.
        for error in [None, Some("transport failed")] {
            let stale = compare_case(&current, &mimic, Some(&outcome(&recorded_case, error)));
            assert!(
                stale.errors.iter().any(|e| e.contains("changed since")),
                "{:?}",
                stale.errors
            );
            assert_eq!(stale.outcome, Outcome::Error);
        }
    }

    #[test]
    fn leaks_are_located_by_case_step_and_header_without_their_values() {
        let secrets = vec![("my-project".to_owned(), "project")];
        let text = json!({"cases": [{"id": "c1", "steps": [
            {"name": "s1", "response": {"headers": {"location": "https://x/my-project/o"},
                                         "body": {"owner": "someone@example.com"}}}]}]})
        .to_string();
        let leaks = find_leaks(&text, &secrets);
        assert_eq!(
            leaks,
            [
                "$.cases[c1].steps[s1].response.body.owner: an email address",
                "$.cases[c1].steps[s1].response.headers.location: the project",
            ]
        );
        assert!(
            leaks
                .iter()
                .all(|l| !l.contains("my-project/") && !l.contains("someone"))
        );
        let redacted = redact_leaks(&text, &secrets);
        assert!(find_leaks(&redacted, &secrets).is_empty(), "{redacted}");
        assert_eq!(
            find_leaks("a\nsee /Users/x\nb", &secrets),
            ["line 2: a local path (/Users/)"]
        );
    }
    #[test]
    fn local_paths_are_replaced_as_a_whole_and_labels_that_leak_become_indices() {
        let secrets = vec![("my-project".to_owned(), "project")];
        let text = json!({"cases": [{"id": "my-project-case", "steps": [
            {"name": "s", "request": "GET /x", "error": "open /Users/alice/src/x.rs failed",
             "my-project": {"e": "a@b.co"}}]}]})
        .to_string();
        let leaks = find_leaks(&text, &secrets);
        assert_eq!(leaks.len(), 4, "{leaks:?}");
        assert!(
            leaks
                .iter()
                .all(|l| !l.contains("my-project") && !l.contains("alice")),
            "{leaks:?}"
        );
        assert!(
            leaks
                .iter()
                .any(|l| l.starts_with("$.cases[0].steps[s].error"))
        );
        assert!(leaks.iter().any(|l| l.contains("<redacted-key>")));
        let redacted = redact_leaks(&text, &secrets);
        assert!(
            !redacted.contains("alice") && !redacted.contains("src/x.rs"),
            "{redacted}"
        );
        assert!(redacted.contains("open <path> failed"), "{redacted}");
        assert!(find_leaks(&redacted, &secrets).is_empty(), "{redacted}");
        assert_eq!(
            redact_leaks("see '/home/bob/a b' and /tmp/z.", &secrets),
            "see '<path> b' and <path>"
        );
    }

    #[test]
    fn renders_differences_with_paths() {
        let report = CaseReport {
            id: "c".into(),
            description: "d".into(),
            outcome: Outcome::Differs,
            errors: vec![],
            steps: vec![StepReport {
                name: "s".into(),
                request: "GET /x".into(),
                differences: vec![
                    Difference::Status {
                        reference: 404,
                        mimic: 400,
                    },
                    Difference::Body {
                        path: "$.a".into(),
                        reference: Some(json!("x")),
                        mimic: None,
                    },
                ],
                notes: vec![],
            }],
        };
        let header = Header {
            mode: "live",
            generated: "2026-11-01",
            reference_requests: Some(9),
            reference_retries: 0,
            cleanup: &[],
            reference_cleanup: &[],
            skipped: &[],
        };
        let md = markdown(&header, std::slice::from_ref(&report));
        assert!(md.contains("status: reference `404`, mimic `400`"));
        assert!(md.contains("body `$.a`: reference `\"x\"`, mimic `(absent)`"));
        assert!(md.contains("## Cleanup\n\n```text\n```\n"), "{md}");
        assert!(md.contains("Requests sent to the live reference service (when recorded): 9"));
        let fail_on_diff = FailOn {
            diff: true,
            note: false,
        };
        assert!(has_failures(&[report], fail_on_diff));
    }

    #[test]
    fn a_mimic_expectation_note_is_not_a_failure_unless_asked_but_a_reference_one_is() {
        use super::super::case::{fingerprint, parse_case};
        use super::super::normalize::{NormBody, NormResponse};
        use std::collections::BTreeMap;
        let case = parse_case(
            &json!({"id": "c", "description": "d", "bucket": false,
                    "steps": [{"name": "s", "method": "GET", "path": "/x"}]}),
            "c",
        )
        .unwrap();
        let outcome = |note: Option<&str>| CaseOutcome {
            id: "c".into(),
            bucket: Some("none".into()),
            error: None,
            steps: vec![StepOutcome {
                name: "s".into(),
                request: "GET /x".into(),
                result: Ok(NormResponse {
                    status: 200,
                    headers: BTreeMap::new(),
                    body: NormBody::Empty,
                }),
                expectation: note.map(str::to_owned),
                retries: 0,
                fingerprint: fingerprint(&case.steps[0]),
            }],
        };
        let noted = compare_case(&case, &outcome(Some("expected 404")), None);
        assert_eq!(noted.outcome, Outcome::Noted);
        assert_eq!(noted.outcome.label(false), "note");
        assert!(!has_failures(
            std::slice::from_ref(&noted),
            FailOn::default()
        ));
        let strict = FailOn {
            diff: false,
            note: true,
        };
        assert!(has_failures(&[noted], strict));
        let wrong_case = compare_case(&case, &outcome(None), Some(&outcome(Some("expected 404"))));
        assert_eq!(wrong_case.outcome, Outcome::Error);
        // A recording of the same steps under another bucket mode is refused; one
        // made before the mode was recorded still replays.
        for (recorded, refused) in [(Some("shared"), true), (Some("none"), false), (None, false)] {
            let reference = CaseOutcome {
                bucket: recorded.map(str::to_owned),
                ..outcome(None)
            };
            let report = compare_case(&case, &outcome(None), Some(&reference));
            assert_eq!(
                report
                    .errors
                    .iter()
                    .any(|e| e.contains("bucket mode changed")),
                refused,
                "{recorded:?}: {:?}",
                report.errors
            );
        }
        let harness = CaseOutcome {
            error: Some("boom".into()),
            ..outcome(None)
        };
        let failed = compare_case(&case, &harness, None);
        assert!(has_failures(&[failed], FailOn::default()));
    }
}
