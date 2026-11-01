//! Executes cases against one side (live reference service or local gcs-mimic)
//! and returns normalized outcomes.

use super::bucket;
use super::case::{Body, BucketMode, Case, Protocol, fingerprint};
use super::normalize::{NormResponse, Normalizer, project_numbers};
use super::template::{Vars, expand, expand_json, extract, percent_encode};
use super::transport::{Request, Transport};
use serde_json::Value;

pub(crate) struct Names {
    pub(crate) bucket: String,
    pub(crate) new_bucket: String,
    pub(crate) missing_bucket: String,
    /// Prefix of the per-case `{bucket}` of a case with `"bucket": "own"`.
    pub(crate) own_bucket: String,
    /// The run's name prefix; every bucket of the run starts with it.
    pub(crate) run_id: String,
}

impl Names {
    /// The `{new_bucket}` of the case at `index`: one name per case, so a case
    /// never meets a name another case just deleted and the live service has
    /// no reuse delay to trip over.
    pub(crate) fn new_bucket_for(&self, index: usize) -> String {
        format!("{}-{index}", self.new_bucket)
    }
}

pub(crate) struct Side<'a> {
    pub(crate) transport: &'a dyn Transport,
    /// The `grpcurl` transport; `None` when it is unavailable, in which case
    /// the caller has already dropped the gRPC cases.
    pub(crate) grpc: Option<&'a dyn Transport>,
    pub(crate) project: &'a str,
    /// Base URLs and hosts that appear in links and are replaced by `<host>`.
    pub(crate) hosts: Vec<String>,
    /// Longest wait before retrying a 5xx; short in tests.
    pub(crate) server_error_wait: std::time::Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StepOutcome {
    pub(crate) name: String,
    pub(crate) request: String,
    pub(crate) result: Result<NormResponse, String>,
    /// Set when the step's `expect_status` was not met.
    pub(crate) expectation: Option<String>,
    /// 429 responses retried for this step.
    pub(crate) retries: u32,
    /// Hash of the step's request definition, see `case::fingerprint`.
    pub(crate) fingerprint: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CaseOutcome {
    pub(crate) id: String,
    pub(crate) steps: Vec<StepOutcome>,
    /// A harness failure that stopped the case.
    pub(crate) error: Option<String>,
}

pub(crate) struct SideRun {
    pub(crate) cases: Vec<CaseOutcome>,
    /// One line per bucket cleanup, with the confirmation that it is gone.
    pub(crate) cleanup: Vec<String>,
    pub(crate) failed_cleanup: bool,
    /// Project numbers seen in responses, for the redaction guard.
    pub(crate) project_numbers: Vec<String>,
}

fn base_normalizer(side: &Side<'_>, names: &Names) -> Normalizer {
    let mut norm = Normalizer::default();
    norm.add_literal(&names.bucket, "<bucket>");
    norm.add_literal(&names.missing_bucket, "<missing_bucket>");
    norm.add_literal(&names.run_id, "<run>");
    norm.add_literal(side.project, "<project>");
    for host in &side.hosts {
        let placeholder = if host.contains("www.googleapis.com") {
            "<www-host>"
        } else {
            "<host>"
        };
        norm.add_literal(host, placeholder);
    }
    norm
}

/// A gRPC step's `method` is the RPC name; the body is one JSON message, or
/// each element of an array as its own message for a client-streaming call.
fn build_grpc_request(step: &super::case::Step, vars: &Vars) -> Result<Request, String> {
    let mut headers = Vec::new();
    for (name, value) in &step.headers {
        headers.push((expand(name, vars)?, expand(value, vars)?));
    }
    let messages = match &step.body {
        Body::Json(Value::Array(items)) => items.clone(),
        Body::Json(message) => vec![message.clone()],
        Body::None => vec![Value::Object(serde_json::Map::new())],
        Body::Text(_) | Body::Bytes(_) => return Err("a gRPC step has no text body".into()),
    };
    let mut body = Vec::new();
    for message in &messages {
        body.extend(expand_json(message, vars)?.to_string().into_bytes());
        body.push(b'\n');
    }
    Ok(Request {
        method: step.method.clone(),
        target: String::new(),
        headers,
        body: Some(body),
    })
}

pub(crate) fn build_request(step: &super::case::Step, vars: &Vars) -> Result<Request, String> {
    if step.protocol == Protocol::Grpc {
        return build_grpc_request(step, vars);
    }
    let mut target = expand(&step.path, vars)?;
    let mut separator = '?';
    for (name, value) in &step.query {
        let value = expand(value, vars)?;
        target.push_str(&format!(
            "{separator}{}={}",
            percent_encode(name),
            percent_encode(&value)
        ));
        separator = '&';
    }
    let mut headers = Vec::new();
    for (name, value) in &step.headers {
        headers.push((expand(name, vars)?, expand(value, vars)?));
    }
    let has_type = headers
        .iter()
        .any(|(n, _)| n.eq_ignore_ascii_case("content-type"));
    let body = match &step.body {
        Body::None => None,
        Body::Json(value) => {
            if !has_type {
                headers.push(("Content-Type".into(), "application/json".into()));
            }
            Some(expand_json(value, vars)?.to_string().into_bytes())
        }
        Body::Text(text) => Some(expand(text, vars)?.into_bytes()),
        Body::Bytes(bytes) => Some(bytes.clone()),
    };
    Ok(Request {
        method: step.method.clone(),
        target,
        headers,
        body,
    })
}

const SKIPPED: &str = "skipped: an earlier step of the case failed";

fn skipped_step(step: &super::case::Step) -> StepOutcome {
    StepOutcome {
        name: step.name.clone(),
        request: String::new(),
        result: Err(SKIPPED.into()),
        expectation: None,
        retries: 0,
        fingerprint: fingerprint(step),
    }
}

/// A case that could not start (its bucket was not created): every step is
/// recorded as skipped so the outcome still lines up with the case.
fn failed_case(case: &Case, error: String) -> CaseOutcome {
    CaseOutcome {
        id: case.id.clone(),
        steps: case.steps.iter().map(skipped_step).collect(),
        error: Some(error),
    }
}

fn record_numbers(json: &Value, norm: &mut Normalizer, seen: &mut Vec<String>) {
    let mut found = Vec::new();
    project_numbers(json, &mut found);
    for number in found {
        norm.add_literal(&number, "<project-number>");
        seen.push(number);
    }
}

/// Runs one step. Returns its outcome and, when the step failed the case, the
/// (redacted) error.
fn run_step(
    side: &Side<'_>,
    step: &super::case::Step,
    vars: &mut Vars,
    norm: &mut Normalizer,
    seen: &mut Vec<String>,
) -> (StepOutcome, Option<String>) {
    let mut outcome = StepOutcome {
        name: step.name.clone(),
        request: String::new(),
        result: Err(String::new()),
        expectation: None,
        retries: 0,
        fingerprint: fingerprint(step),
    };
    let request = match build_request(step, vars) {
        Ok(request) => request,
        Err(error) => {
            let error = norm.text(&error);
            outcome.result = Err(error.clone());
            return (outcome, Some(error));
        }
    };
    outcome.request = if step.protocol == Protocol::Grpc {
        format!("GRPC {}", request.method)
    } else {
        format!("{} {}", request.method, norm.text(&request.target))
    };
    let transport = match (step.protocol, side.grpc) {
        (Protocol::Grpc, Some(grpc)) => grpc,
        (Protocol::Grpc, None) => {
            outcome.result = Err("no gRPC transport".into());
            return (outcome, Some("no gRPC transport".into()));
        }
        _ => side.transport,
    };
    let sent = transport.send(&request);
    outcome.retries = transport.take_retries();
    let response = match sent {
        Ok(response) => response,
        Err(error) => {
            let error = norm.text(&error);
            outcome.result = Err(error.clone());
            return (outcome, Some(error));
        }
    };
    if let Ok(json) = serde_json::from_slice(&response.body) {
        record_numbers(&json, norm, seen);
    }
    let mut capture_error = None;
    for (name, spec) in &step.capture {
        match extract(spec, &response) {
            Ok(value) => {
                if value.len() >= 6 {
                    norm.add_literal(&value, &format!("<{name}>"));
                }
                vars.insert(name.clone(), value);
            }
            Err(error) => {
                capture_error = Some(norm.text(&error));
                break;
            }
        }
    }
    outcome.expectation = step
        .expect_status
        .filter(|expected| *expected != response.status)
        .map(|expected| format!("expected status {expected}, got {}", response.status));
    outcome.result = Ok(norm.response(&response, request.method == "HEAD"));
    (outcome, capture_error)
}

fn run_case(
    side: &Side<'_>,
    case: &Case,
    buckets: &CaseBuckets<'_>,
    names: &Names,
    base: &Normalizer,
    seen: &mut Vec<String>,
) -> CaseOutcome {
    let mut vars = Vars::from([
        ("project".to_owned(), side.project.to_owned()),
        ("bucket".to_owned(), buckets.bucket.to_owned()),
        ("new_bucket".to_owned(), buckets.new_bucket.to_owned()),
        ("missing_bucket".to_owned(), names.missing_bucket.clone()),
        ("object".to_owned(), format!("{}-1", case.id)),
        ("object2".to_owned(), format!("{}-2", case.id)),
    ]);
    let mut norm = base.clone();
    norm.add_literal(buckets.bucket, "<bucket>");
    norm.add_literal(buckets.new_bucket, "<new_bucket>");
    let mut outcome = CaseOutcome {
        id: case.id.clone(),
        steps: Vec::new(),
        error: None,
    };
    let mut failed = false;
    for step in &case.steps {
        // After a failure only steps marked `always` run (restoring state so
        // the bucket can be deleted); the rest are recorded as skipped.
        if failed && !step.always {
            outcome.steps.push(skipped_step(step));
            continue;
        }
        let (step_outcome, error) = run_step(side, step, &mut vars, &mut norm, seen);
        outcome.steps.push(step_outcome);
        if let Some(error) = error {
            failed = true;
            outcome
                .error
                .get_or_insert_with(|| format!("step {:?}: {error}", step.name));
        }
    }
    outcome
}

/// The bucket names a case sees as `{bucket}` and `{new_bucket}`.
struct CaseBuckets<'a> {
    bucket: &'a str,
    new_bucket: &'a str,
}

fn cleanup_line(side: &Side<'_>, norm: &Normalizer, label: String, name: &str, run: &mut SideRun) {
    match bucket::cleanup(side, norm, name) {
        Ok(note) => run.cleanup.push(format!("{label}: {note}")),
        Err(error) => {
            run.failed_cleanup = true;
            run.cleanup
                .push(format!("{label}: CLEANUP FAILED: {error}"));
        }
    }
}

/// Runs one case with its own buckets and cleans them up right away, so at most
/// one bucket per kind exists at a time.
fn run_one(
    side: &Side<'_>,
    case: &Case,
    index: usize,
    names: &Names,
    base: &Normalizer,
    shared_error: Option<&String>,
    run: &mut SideRun,
) -> CaseOutcome {
    let new_bucket = names.new_bucket_for(index);
    let own = (case.bucket == BucketMode::Own).then(|| format!("{}{index}", names.own_bucket));
    let bucket_name = own.clone().unwrap_or_else(|| names.bucket.clone());
    // Cleanup and creation messages name these buckets, so every one is hidden.
    let mut norm = base.clone();
    if let Some(name) = &own {
        norm.add_literal(name, "<bucket>");
    }
    norm.add_literal(&new_bucket, "<new_bucket>");
    if let (BucketMode::Shared, Some(error)) = (case.bucket, shared_error) {
        return failed_case(case, error.clone());
    }
    if let Some(name) = &own {
        match bucket::create(side, &norm, name) {
            Ok(numbers) => run.project_numbers.extend(numbers),
            Err(error) => return failed_case(case, format!("create the case bucket: {error}")),
        }
    }
    let buckets = CaseBuckets {
        bucket: &bucket_name,
        new_bucket: &new_bucket,
    };
    let outcome = run_case(side, case, &buckets, names, base, &mut run.project_numbers);
    if let Some(name) = &own {
        cleanup_line(side, &norm, format!("bucket of {}", case.id), name, run);
    }
    if case.uses_variable("new_bucket") {
        cleanup_line(
            side,
            &norm,
            format!("new bucket of {}", case.id),
            &new_bucket,
            run,
        );
    }
    outcome
}

/// Runs every case, each with the bucket its `bucket` mode names, and always
/// cleans up: after each case for its own buckets, at the end for the shared
/// one, and finally by listing every bucket with the run's prefix.
pub(crate) fn run_side(side: &Side<'_>, cases: &[Case], names: &Names) -> SideRun {
    let mut base = base_normalizer(side, names);
    let mut run = SideRun {
        cases: Vec::new(),
        cleanup: Vec::new(),
        failed_cleanup: false,
        project_numbers: Vec::new(),
    };
    let needs_shared = cases.iter().any(|c| c.bucket == BucketMode::Shared);
    let mut shared_error = None;
    if needs_shared {
        match bucket::create(side, &base, &names.bucket) {
            Ok(numbers) => {
                for number in &numbers {
                    base.add_literal(number, "<project-number>");
                }
                run.project_numbers.extend(numbers);
            }
            Err(error) => shared_error = Some(format!("create shared bucket: {error}")),
        }
    }
    for (index, case) in cases.iter().enumerate() {
        let outcome = run_one(
            side,
            case,
            index,
            names,
            &base,
            shared_error.as_ref(),
            &mut run,
        );
        run.cases.push(outcome);
    }
    if needs_shared {
        cleanup_line(
            side,
            &base,
            "shared bucket".to_owned(),
            &names.bucket,
            &mut run,
        );
    }
    let (lines, failed) = bucket::sweep(side, &base, &format!("{}-", names.run_id));
    run.failed_cleanup |= failed;
    run.cleanup.extend(lines);
    run
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::differential::case::parse_case;
    use crate::differential::transport::Response;
    use serde_json::json;
    use std::cell::RefCell;
    use std::time::Duration;

    /// Answers every GET with 404 (so cleanup sees the bucket as gone), every
    /// DELETE with 204 and everything else with an empty 200. A request whose
    /// target contains `fail_on` gets a transport error.
    #[derive(Default)]
    struct Recorder {
        sent: RefCell<Vec<String>>,
        fail_on: Option<&'static str>,
    }

    impl Transport for Recorder {
        fn send(&self, request: &Request) -> Result<Response, String> {
            self.sent
                .borrow_mut()
                .push(format!("{} {}", request.method, request.target));
            if self
                .fail_on
                .is_some_and(|needle| request.target.contains(needle))
            {
                return Err("curl failed (exit 7)".into());
            }
            let status = match request.method.as_str() {
                "DELETE" => 204,
                "GET" if request.target.starts_with("/storage/v1/b?") => 200,
                "GET" => 404,
                _ => 200,
            };
            Ok(Response {
                status,
                headers: Vec::new(),
                body: b"{}".to_vec(),
            })
        }

        fn requests(&self) -> u64 {
            self.sent.borrow().len() as u64
        }
    }

    fn names() -> Names {
        Names {
            bucket: "run-b".into(),
            new_bucket: "run-n".into(),
            missing_bucket: "run-m".into(),
            own_bucket: "run-c".into(),
            run_id: "run".into(),
        }
    }

    fn side(transport: &Recorder) -> Side<'_> {
        Side {
            transport,
            grpc: None,
            project: "proj-x",
            hosts: Vec::new(),
            server_error_wait: Duration::ZERO,
        }
    }

    fn case(id: &str) -> Case {
        let value = json!({
            "id": id,
            "description": "test",
            "bucket": false,
            "steps": [
                {"name": "create", "method": "POST", "path": "/storage/v1/b/{new_bucket}"},
                {"name": "delete", "method": "DELETE", "path": "/storage/v1/b/{new_bucket}"}
            ]
        });
        parse_case(&value, id).expect("test case parses")
    }

    #[test]
    fn each_case_gets_its_own_new_bucket_that_is_normalized_and_cleaned() {
        let names = names();
        let transport = Recorder::default();
        let run = run_side(&side(&transport), &[case("one"), case("two")], &names);
        let sent = transport.sent.borrow();
        let created: Vec<&String> = sent.iter().filter(|l| l.starts_with("POST")).collect();
        assert_eq!(created.len(), 2);
        assert_ne!(created[0], created[1], "cases must not share a bucket name");
        for outcome in &run.cases {
            for step in &outcome.steps {
                assert!(step.request.contains("<new_bucket>"), "{}", step.request);
                assert!(!step.request.contains("run-n"), "{}", step.request);
            }
        }
        assert_eq!(run.cleanup.len(), 3, "{:?}", run.cleanup);
        for name in [names.new_bucket_for(0), names.new_bucket_for(1)] {
            assert!(
                sent.iter()
                    .any(|l| l.starts_with(&format!("GET /storage/v1/b/{name}?fields=name"))),
                "no cleanup confirmation for {name}"
            );
        }
        assert!(
            sent.iter()
                .any(|l| l.starts_with("GET /storage/v1/b?project=proj-x&prefix=run-")),
            "the run must end by listing its prefix: {sent:?}"
        );
    }

    #[test]
    fn a_case_with_its_own_bucket_creates_labels_and_deletes_it() {
        let value = json!({"id": "own", "description": "d", "bucket": "own",
            "steps": [{"name": "get", "method": "GET", "path": "/storage/v1/b/{bucket}"}]});
        let own = parse_case(&value, "own").unwrap();
        let transport = Recorder::default();
        let run = run_side(&side(&transport), &[own], &names());
        let sent = transport.sent.borrow();
        assert!(
            sent.iter()
                .any(|l| l == "POST /storage/v1/b?project=proj-x")
        );
        assert!(sent.iter().any(|l| l == "GET /storage/v1/b/run-c0"));
        assert!(
            sent.iter()
                .any(|l| l.starts_with("GET /storage/v1/b/run-c0?fields=name"))
        );
        assert!(
            !sent.iter().any(|l| l.contains("run-b")),
            "no shared bucket without a shared case"
        );
        assert_eq!(run.cases[0].steps[0].request, "GET /storage/v1/b/<bucket>");
    }

    #[test]
    fn errors_about_a_case_bucket_do_not_name_it() {
        struct Denied;
        impl Transport for Denied {
            fn send(&self, _: &Request) -> Result<Response, String> {
                Ok(Response {
                    status: 403,
                    headers: Vec::new(),
                    body: b"no access to run-c0".to_vec(),
                })
            }
            fn requests(&self) -> u64 {
                0
            }
        }
        let value = json!({"id": "own", "description": "d", "bucket": "own",
            "steps": [{"name": "get", "method": "GET", "path": "/storage/v1/b/{bucket}"}]});
        let own = parse_case(&value, "own").unwrap();
        let denied = Denied;
        let side = Side {
            transport: &denied,
            grpc: None,
            project: "proj-x",
            hosts: Vec::new(),
            server_error_wait: Duration::ZERO,
        };
        let run = run_side(&side, &[own], &names());
        let error = run.cases[0].error.clone().unwrap_or_default();
        assert!(error.contains("create the case bucket"), "{error}");
        assert!(!error.contains("run-c0"), "{error}");
        assert!(error.contains("<bucket>"), "{error}");
    }
    #[test]
    fn a_failed_step_skips_the_rest_except_always_steps_and_cleanup_still_runs() {
        let value = json!({"id": "brk", "description": "d", "bucket": false, "steps": [
            {"name": "boom", "method": "GET", "path": "/storage/v1/b/{new_bucket}/boom"},
            {"name": "after", "method": "GET", "path": "/after"},
            {"name": "restore", "method": "PATCH", "path": "/storage/v1/b/{new_bucket}", "body": {}, "always": true}
        ]});
        let broken = parse_case(&value, "brk").unwrap();
        let transport = Recorder {
            fail_on: Some("/boom"),
            ..Recorder::default()
        };
        let run = run_side(&side(&transport), &[broken], &names());
        let steps = &run.cases[0].steps;
        assert_eq!(steps.len(), 3, "every step is recorded, skipped or not");
        assert_eq!(steps[1].result, Err(SKIPPED.into()));
        assert!(steps[2].result.is_ok(), "the always step must run");
        let sent = transport.sent.borrow();
        assert!(!sent.iter().any(|l| l == "GET /after"));
        assert!(
            sent.iter()
                .any(|l| l.starts_with("PATCH /storage/v1/b/run-n-0"))
        );
        assert!(
            run.cases[0]
                .error
                .as_ref()
                .is_some_and(|e| e.contains("boom"))
        );
        assert!(
            !run.cleanup.is_empty() && !run.failed_cleanup,
            "{:?}",
            run.cleanup
        );
    }
}
