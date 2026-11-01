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
    norm.add_literal(&names.new_bucket, "<new_bucket>");
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
    base: &Normalizer,
    project_numbers_seen: &mut Vec<String>,
) -> CaseOutcome {
    let mut vars = Vars::from([
        ("project".to_owned(), side.project.to_owned()),
        ("bucket".to_owned(), names.bucket.clone()),
        ("new_bucket".to_owned(), names.new_bucket.clone()),
        ("missing_bucket".to_owned(), names.missing_bucket.clone()),
        ("object".to_owned(), format!("{}-1", case.id)),
        ("object2".to_owned(), format!("{}-2", case.id)),
    ]);
    let mut norm = base.clone();
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
    let needs_new = cases.iter().any(|c| c.uses_variable("new_bucket"));
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
    for case in cases {
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
            .push(run_case(side, case, names, &base, &mut seen));
    }
    let mut targets = Vec::new();
    if needs_shared {
        targets.push(("shared bucket", &names.bucket));
    }
    if needs_new {
        targets.push(("lifecycle bucket", &names.new_bucket));
    }
    for (label, name) in targets {
        match bucket::cleanup(side, name) {
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
