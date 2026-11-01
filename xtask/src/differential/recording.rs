//! Sanitized recordings of the reference service, so a later run can compare
//! gcs-mimic against them without contacting the service again.

use super::normalize::{NormBody, NormResponse};
use super::runner::{CaseOutcome, StepOutcome};
use serde_json::{Map, Value, json};

type Result<T> = std::result::Result<T, String>;
const FORMAT: u64 = 2;

pub(crate) struct Recording {
    pub(crate) generated: String,
    pub(crate) requests: u64,
    /// Tool versions behind the gRPC bodies; see `differential::tools_note`.
    pub(crate) tools: String,
    pub(crate) cases: Vec<CaseOutcome>,
}

fn body_json(body: &NormBody) -> Value {
    match body {
        NormBody::Empty => json!({"kind": "empty"}),
        NormBody::Json(value) => json!({"kind": "json", "value": value}),
        NormBody::Text(text) => json!({"kind": "text", "value": text}),
    }
}

fn step_json(step: &StepOutcome) -> Value {
    let mut map = Map::new();
    map.insert("name".into(), json!(step.name));
    map.insert("request".into(), json!(step.request));
    map.insert("fingerprint".into(), json!(step.fingerprint));
    if step.retries > 0 {
        map.insert("retries".into(), json!(step.retries));
    }
    if let Some(expectation) = &step.expectation {
        map.insert("expectation".into(), json!(expectation));
    }
    match &step.result {
        Ok(response) => {
            map.insert(
                "response".into(),
                json!({
                    "status": response.status,
                    "headers": response.headers,
                    "body": body_json(&response.body),
                }),
            );
        }
        Err(error) => {
            map.insert("error".into(), json!(error));
        }
    }
    Value::Object(map)
}

pub(crate) fn to_json(recording: &Recording) -> Value {
    let cases: Vec<Value> = recording
        .cases
        .iter()
        .map(|case| {
            json!({
                "id": case.id,
                "error": case.error,
                "bucket": case.bucket,
                "steps": case.steps.iter().map(step_json).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "format": FORMAT,
        "generated": recording.generated,
        "requests": recording.requests,
        "tools": recording.tools,
        "cases": cases,
    })
}

fn parse_body(value: &Value) -> Result<NormBody> {
    match value["kind"].as_str() {
        Some("empty") => Ok(NormBody::Empty),
        Some("json") => Ok(NormBody::Json(value["value"].clone())),
        Some("text") => Ok(NormBody::Text(
            value["value"]
                .as_str()
                .ok_or("text body without value")?
                .to_owned(),
        )),
        _ => Err("unknown body kind".into()),
    }
}

fn parse_step(value: &Value) -> Result<StepOutcome> {
    let text = |key: &str| -> Result<String> {
        value[key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("step without {key}"))
    };
    let result = match value.get("response") {
        Some(response) => Ok(NormResponse {
            status: response["status"]
                .as_u64()
                .and_then(|s| u16::try_from(s).ok())
                .ok_or("response without status")?,
            headers: response["headers"]
                .as_object()
                .ok_or("response without headers")?
                .iter()
                .map(|(k, v)| {
                    Ok((
                        k.clone(),
                        v.as_str().ok_or("header not a string")?.to_owned(),
                    ))
                })
                .collect::<Result<_>>()?,
            body: parse_body(&response["body"])?,
        }),
        None => Err(text("error")?),
    };
    Ok(StepOutcome {
        name: text("name")?,
        request: text("request")?,
        result,
        expectation: value["expectation"].as_str().map(str::to_owned),
        retries: value["retries"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(0),
        fingerprint: text("fingerprint")?,
    })
}

pub(crate) fn from_json(value: &Value) -> Result<Recording> {
    if value["format"].as_u64() != Some(FORMAT) {
        return Err(format!("unsupported recording format (expected {FORMAT})"));
    }
    let cases = value["cases"]
        .as_array()
        .ok_or("recording without cases")?
        .iter()
        .map(|case| {
            Ok(CaseOutcome {
                id: case["id"].as_str().ok_or("case without id")?.to_owned(),
                error: case["error"].as_str().map(str::to_owned),
                bucket: case["bucket"].as_str().map(str::to_owned),
                steps: case["steps"]
                    .as_array()
                    .ok_or("case without steps")?
                    .iter()
                    .map(parse_step)
                    .collect::<Result<_>>()?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(Recording {
        generated: value["generated"].as_str().unwrap_or_default().to_owned(),
        requests: value["requests"].as_u64().unwrap_or(0),
        tools: value["tools"].as_str().unwrap_or_default().to_owned(),
        cases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn round_trips_responses_errors_and_expectations() {
        let recording = Recording {
            generated: "2026-11-01".into(),
            requests: 7,
            tools: "grpcurl 1.9.4".into(),
            cases: vec![CaseOutcome {
                id: "c".into(),
                error: Some("boom".into()),
                bucket: Some("own".into()),
                steps: vec![
                    StepOutcome {
                        name: "a".into(),
                        request: "GET /x".into(),
                        result: Ok(NormResponse {
                            status: 404,
                            headers: BTreeMap::from([("vary".to_owned(), "Origin".to_owned())]),
                            body: NormBody::Json(json!({"error": {"code": 404}})),
                        }),
                        expectation: Some("expected status 200, got 404".into()),
                        retries: 2,
                        fingerprint: "00ff".into(),
                    },
                    StepOutcome {
                        name: "b".into(),
                        request: "GET /y".into(),
                        result: Err("curl failed".into()),
                        expectation: None,
                        retries: 0,
                        fingerprint: "11aa".into(),
                    },
                ],
            }],
        };
        let parsed = from_json(&to_json(&recording)).unwrap();
        assert_eq!(parsed.cases, recording.cases);
        assert_eq!(parsed.requests, 7);
        assert!(from_json(&json!({"format": 2})).is_err());
    }
}
