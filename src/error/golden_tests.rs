//! Shared golden fixtures for the three protocol adapters.

pub(crate) mod upload;

use http_body_util::{BodyExt, Full};
use hyper::{Response, body::Bytes};
use std::{collections::BTreeSet, fmt::Write as _, path::Path};

/// The variable that asks the golden tests to rewrite the files they compare.
pub(crate) const UPDATE_VARIABLE: &str = "GCSM_UPDATE_GOLDENS";

/// Bucket inputs that reached the adapters through the corresponding shared error.
pub(crate) fn bucket_service_errors() -> Vec<(&'static str, crate::service::ServiceError)> {
    use crate::{config::CompatProfile, service::parse_bucket};
    use serde_json::json;
    use std::time::{Duration, UNIX_EPOCH};

    let outside = UNIX_EPOCH + Duration::from_secs(253_402_300_800);
    [
        ("required name", json!({}), "project", UNIX_EPOCH),
        ("invalid name", json!({"name": 1}), "project", UNIX_EPOCH),
        ("unsupported acl", json!({"acl": []}), "project", UNIX_EPOCH),
        (
            "invalid location",
            json!({"name": "bucket", "location": "nowhere"}),
            "project",
            UNIX_EPOCH,
        ),
        ("invalid project", json!({}), "bad project", outside),
        (
            "invalid timestamp",
            json!({"name": "bucket"}),
            "project",
            outside,
        ),
    ]
    .into_iter()
    .map(|(label, body, project, now)| {
        let error = parse_bucket(
            body.as_object().unwrap(),
            project,
            now,
            CompatProfile::Native,
            "US",
        )
        .unwrap_err();
        crate::error::assert_distinct_causes(&error);
        (label, error)
    })
    .collect()
}

/// A golden transcript: each case's label and its renderings, in the order the test adds them.
pub(crate) struct Transcript {
    text: String,
    labels: BTreeSet<String>,
    /// The renderings of the current case, so that a repeat names the earlier one.
    renderings: Vec<(&'static str, String)>,
}

impl Transcript {
    /// Starts a transcript whose header says what it pins and which test writes it. `module` is
    /// the test's `module_path!()`.
    pub(crate) fn new(module: &str, test: &str, pins: &str) -> Self {
        let module = module.split_once("::").map_or(module, |(_, path)| path);
        Self {
            text: format!(
                "# {pins}\n# Written by {module}::{test}. Regenerate with {UPDATE_VARIABLE}=1 cargo test --lib {module},\n# which rewrites this file and fails; review the diff and run the test again without it.\n"
            ),
            labels: BTreeSet::new(),
            renderings: Vec::new(),
        }
    }

    pub(crate) fn case(&mut self, section: &str, label: &str) {
        let label = format!("{section}::{label}");
        assert!(self.labels.insert(label.clone()), "{label} is listed twice");
        self.renderings.clear();
        let _ = write!(self.text, "\n== {label}\n");
    }

    /// Adds a rendering of the current case. One equal to an earlier rendering of the case names
    /// that rendering instead of repeating it, so a difference between two paths stands out.
    pub(crate) fn render(&mut self, name: &'static str, rendering: String) {
        assert!(rendering.ends_with('\n'), "{name}: {rendering:?}");
        let earlier = self
            .renderings
            .iter()
            .find(|(_, earlier)| *earlier == rendering);
        if let Some((earlier, _)) = earlier {
            let _ = writeln!(self.text, "-- {name}: same as {earlier}");
        } else {
            let _ = writeln!(self.text, "-- {name}");
            self.text.push_str(&rendering);
        }
        self.renderings.push((name, rendering));
    }

    /// Compares the transcript with the golden file at `path`, relative to the crate root.
    pub(crate) fn assert_golden(&self, path: &str) {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
        let golden = match std::fs::read_to_string(&file) {
            Ok(golden) => golden,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => panic!("{path}: {error}"),
        };
        if golden == self.text {
            return;
        }
        if std::env::var(UPDATE_VARIABLE).as_deref() == Ok("1") {
            std::fs::write(&file, &self.text).unwrap();
            panic!(
                "rewrote {path}; review its diff, then run the test again without {UPDATE_VARIABLE}"
            );
        }
        panic!("{}", first_difference(path, &golden, &self.text));
    }
}

/// The first line where the golden file and the transcript differ, with the case it belongs to.
fn first_difference(path: &str, golden: &str, actual: &str) -> String {
    let advice = format!(
        "If the new answers are intended, run the test with {UPDATE_VARIABLE}=1, which rewrites the file, and review its diff."
    );
    let mut case = "the header";
    let (mut golden_lines, mut actual_lines) = (golden.lines(), actual.lines());
    for number in 1.. {
        let (expected, found) = (golden_lines.next(), actual_lines.next());
        if expected != found {
            return format!(
                "{path} differs at line {number}, in {case}:\n  golden: {expected:?}\n  actual: {found:?}\n{advice}"
            );
        }
        let Some(found) = found else { break };
        if let Some(label) = found.strip_prefix("== ") {
            case = label;
        }
    }
    format!(
        "{path} differs only in its line endings; .gitattributes must check it out with LF. {advice}"
    )
}

/// An HTTP answer as the golden files show it: the status line, every header in the order it is
/// sent, and the body's exact text.
pub(crate) async fn http_response(response: Response<Full<Bytes>>) -> String {
    let (parts, body) = response.into_parts();
    let Ok(collected) = body.collect().await;
    let bytes = collected.to_bytes();
    let mut text = format!("{}\n", parts.status);
    for (name, value) in &parts.headers {
        let _ = writeln!(text, "{name}: {}", value.to_str().unwrap());
    }
    let body = std::str::from_utf8(&bytes).expect("error bodies are text");
    if body.is_empty() || body.ends_with('\n') {
        let _ = writeln!(text, "body: {} bytes", bytes.len());
        text.push_str(body);
    } else {
        let _ = writeln!(text, "body: {} bytes, no final newline", bytes.len());
        text.push_str(body);
        text.push('\n');
    }
    text
}
