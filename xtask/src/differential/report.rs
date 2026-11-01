//! Comparison results, Markdown and JSON rendering, and the redaction guard.

use super::case::{Case, fingerprint};
use super::diff::{Difference, compare};
use super::runner::{CaseOutcome, StepOutcome};
use serde_json::{Value, json};

type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// No difference from the reference (or, in self-check mode, all steps ran).
    Match,
    Differs,
    Error,
}

impl Outcome {
    fn label(self, reference: bool) -> &'static str {
        match (self, reference) {
            (Self::Match, true) => "match",
            (Self::Match, false) => "ok",
            (Self::Differs, _) => "differs",
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
    let has_notes = steps.iter().any(|s| !s.notes.is_empty());
    let outcome = if !errors.is_empty() || (has_notes && !has_diffs) {
        Outcome::Error
    } else if has_diffs {
        Outcome::Differs
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
    for (label, lines) in [
        ("mimic", header.cleanup),
        ("reference", header.reference_cleanup),
    ] {
        for line in lines {
            out.push_str(&format!("- {label} {line}\n"));
        }
    }
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

/// Refuses to write text that still holds a known secret, a name from the live
/// run or a local path. Labels, not values, go into the error.
pub(crate) fn ensure_clean(text: &str, secrets: &[(String, &str)]) -> Result<()> {
    for (value, label) in secrets {
        if value.len() >= 3 && text.contains(value.as_str()) {
            return Err(format!("output still contains the {label}"));
        }
    }
    if super::normalize::contains_email(text) {
        return Err("output still contains an email address".into());
    }
    for path in ["/Users/", "/private/", "/var/folders", "/tmp/", "/home/"] {
        if text.contains(path) {
            return Err(format!("output contains a local path ({path})"));
        }
    }
    Ok(())
}

pub(crate) fn has_failures(reports: &[CaseReport], fail_on_diff: bool) -> bool {
    reports.iter().any(|r| match r.outcome {
        Outcome::Error => true,
        Outcome::Differs => fail_on_diff,
        Outcome::Match => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_rejects_secrets_and_local_paths() {
        let secrets = vec![("my-project".to_owned(), "project")];
        assert!(ensure_clean("clean <project>", &secrets).is_ok());
        assert_eq!(
            ensure_clean("x my-project", &secrets).unwrap_err(),
            "output still contains the project"
        );
        assert!(ensure_clean("see /Users/a/b", &secrets).is_err());
        assert_eq!(
            ensure_clean("mail someone@example.com", &secrets).unwrap_err(),
            "output still contains an email address"
        );
        assert!(ensure_clean("see /var/folders/x", &secrets).is_err());
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
        assert!(md.contains("Requests sent to the live reference service (when recorded): 9"));
        assert!(has_failures(&[report], true));
    }
}
