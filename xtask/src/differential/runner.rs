//! Executes cases against one side (live reference service or local gcs-mimic)
//! and returns normalized outcomes.

use super::bucket;
use super::case::{Body, Case, Protocol};
use super::normalize::{NormResponse, Normalizer, project_numbers};
use super::template::{Vars, expand, expand_json, extract, percent_encode};
use super::transport::{Request, Transport};
use serde_json::Value;

pub(crate) struct Names {
    pub(crate) bucket: String,
    pub(crate) new_bucket: String,
    pub(crate) missing_bucket: String,
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
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StepOutcome {
    pub(crate) name: String,
    pub(crate) request: String,
    pub(crate) result: Result<NormResponse, String>,
    /// Set when the step's `expect_status` was not met.
    pub(crate) expectation: Option<String>,
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
}

fn base_normalizer(side: &Side<'_>, names: &Names) -> Normalizer {
    let mut norm = Normalizer::default();
    norm.add_literal(&names.bucket, "<bucket>");
    norm.add_literal(&names.missing_bucket, "<missing_bucket>");
    norm.add_literal(side.project, "<project>");
    for host in &side.hosts {
        norm.add_literal(host, "<host>");
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
        Body::Text(_) => return Err("a gRPC step has no text body".into()),
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

fn build_request(step: &super::case::Step, vars: &Vars) -> Result<Request, String> {
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
    };
    Ok(Request {
        method: step.method.clone(),
        target,
        headers,
        body,
    })
}

fn run_case(
    side: &Side<'_>,
    case: &Case,
    names: &Names,
    new_bucket: &str,
    base: &Normalizer,
    project_numbers_seen: &mut Vec<String>,
) -> CaseOutcome {
    let mut vars = Vars::from([
        ("project".to_owned(), side.project.to_owned()),
        ("bucket".to_owned(), names.bucket.clone()),
        ("new_bucket".to_owned(), new_bucket.to_owned()),
        ("missing_bucket".to_owned(), names.missing_bucket.clone()),
        ("object".to_owned(), format!("{}-1", case.id)),
        ("object2".to_owned(), format!("{}-2", case.id)),
    ]);
    let mut norm = base.clone();
    norm.add_literal(new_bucket, "<new_bucket>");
    let mut outcome = CaseOutcome {
        id: case.id.clone(),
        steps: Vec::new(),
        error: None,
    };
    for step in &case.steps {
        let request = match build_request(step, &vars) {
            Ok(request) => request,
            Err(error) => {
                outcome.error = Some(format!("step {:?}: {error}", step.name));
                break;
            }
        };
        let line = if step.protocol == Protocol::Grpc {
            format!("GRPC {}", request.method)
        } else {
            format!("{} {}", request.method, norm.text(&request.target))
        };
        let transport = match (step.protocol, side.grpc) {
            (Protocol::Grpc, Some(grpc)) => grpc,
            (Protocol::Grpc, None) => {
                outcome.error = Some(format!("step {:?}: no gRPC transport", step.name));
                break;
            }
            _ => side.transport,
        };
        let response = match transport.send(&request) {
            Ok(response) => response,
            Err(error) => {
                outcome.steps.push(StepOutcome {
                    name: step.name.clone(),
                    request: line,
                    result: Err(error.clone()),
                    expectation: None,
                });
                outcome.error = Some(format!("step {:?}: {error}", step.name));
                break;
            }
        };
        if let Ok(json) = serde_json::from_slice(&response.body) {
            let mut found = Vec::new();
            project_numbers(&json, &mut found);
            for number in found {
                norm.add_literal(&number, "<project-number>");
                project_numbers_seen.push(number);
            }
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
                    capture_error = Some(error);
                    break;
                }
            }
        }
        let expectation = step
            .expect_status
            .filter(|expected| *expected != response.status)
            .map(|expected| format!("expected status {expected}, got {}", response.status));
        outcome.steps.push(StepOutcome {
            name: step.name.clone(),
            request: line,
            result: Ok(norm.response(&response)),
            expectation,
        });
        if let Some(error) = capture_error {
            outcome.error = Some(format!("step {:?}: {error}", step.name));
            break;
        }
    }
    outcome
}

/// Runs every case against one shared bucket (plus the lifecycle bucket a case
/// creates itself) and always cleans both up.
pub(crate) fn run_side(side: &Side<'_>, cases: &[Case], names: &Names) -> SideRun {
    let mut base = base_normalizer(side, names);
    let mut run = SideRun {
        cases: Vec::new(),
        cleanup: Vec::new(),
        failed_cleanup: false,
    };
    let needs_shared = cases.iter().any(|c| c.bucket);
    let mut seen = Vec::new();
    let mut shared_error = None;
    if needs_shared {
        match bucket::create(side, &names.bucket) {
            Ok(numbers) => {
                for number in numbers {
                    base.add_literal(&number, "<project-number>");
                }
            }
            Err(error) => shared_error = Some(format!("create shared bucket: {error}")),
        }
    }
    let mut new_targets = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let new_bucket = names.new_bucket_for(index);
        if case.uses_variable("new_bucket") {
            new_targets.push((case.id.clone(), new_bucket.clone()));
        }
        if case.bucket
            && let Some(error) = &shared_error
        {
            run.cases.push(CaseOutcome {
                id: case.id.clone(),
                steps: Vec::new(),
                error: Some(error.clone()),
            });
            continue;
        }
        run.cases
            .push(run_case(side, case, names, &new_bucket, &base, &mut seen));
    }
    let mut targets: Vec<(String, String)> = Vec::new();
    if needs_shared {
        targets.push(("shared bucket".to_owned(), names.bucket.clone()));
    }
    for (id, name) in new_targets {
        targets.push((format!("bucket of {id}"), name));
    }
    for (label, name) in targets {
        match bucket::cleanup(side, &name) {
            Ok(note) => run.cleanup.push(format!("{label}: {note}")),
            Err(error) => {
                run.failed_cleanup = true;
                run.cleanup
                    .push(format!("{label}: CLEANUP FAILED: {error}"));
            }
        }
    }
    run
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::differential::case::parse_case;
    use crate::differential::transport::Response;
    use serde_json::json;
    use std::cell::RefCell;

    /// Answers every GET with 404 (so cleanup sees the bucket as gone), every
    /// DELETE with 204 and everything else with an empty 200.
    #[derive(Default)]
    struct Recorder {
        sent: RefCell<Vec<String>>,
    }

    impl Transport for Recorder {
        fn send(&self, request: &Request) -> Result<Response, String> {
            self.sent
                .borrow_mut()
                .push(format!("{} {}", request.method, request.target));
            let status = match request.method.as_str() {
                "DELETE" => 204,
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
        let names = Names {
            bucket: "run-b".into(),
            new_bucket: "run-n".into(),
            missing_bucket: "run-m".into(),
        };
        let transport = Recorder::default();
        let side = Side {
            transport: &transport,
            project: "proj-x",
            hosts: Vec::new(),
        };
        let run = run_side(&side, &[case("one"), case("two")], &names);
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
        assert_eq!(run.cleanup.len(), 2, "{:?}", run.cleanup);
        for name in [names.new_bucket_for(0), names.new_bucket_for(1)] {
            assert!(
                sent.iter()
                    .any(|l| l.starts_with(&format!("GET /storage/v1/b/{name}/o?"))),
                "no cleanup confirmation for {name}"
            );
        }
    }
}
