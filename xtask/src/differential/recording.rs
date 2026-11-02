//! Sanitized recordings of the reference service, so a later run can compare
//! gcs-mimic against them without contacting the service again.

use super::normalize::{NormBody, NormResponse};
use super::runner::{CaseOutcome, StepOutcome};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;

type Result<T> = std::result::Result<T, String>;
const FORMAT: u64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Live,
    SelfCheck,
    Legacy,
}

pub(crate) struct Recording {
    pub(crate) generated: String,
    pub(crate) recorded_dates: BTreeSet<String>,
    pub(crate) source: Source,
    pub(crate) requests: u64,
    /// Tool versions behind the gRPC bodies; see `differential::tools_note`.
    pub(crate) tools: String,
    pub(crate) cases: Vec<CaseOutcome>,
}

/// Replace complete cases, retaining base order and counting all reference
/// requests spent on both recordings, including superseded observations.
pub(crate) fn merge(mut base: Recording, partial: Recording) -> Result<Recording> {
    if base.source == Source::SelfCheck || partial.source != Source::Live || partial.requests == 0 {
        return Err("merge requires a live reference and a partial with live provenance and nonzero requests".into());
    }
    if !crate::date(&base.generated)
        || !crate::date(&partial.generated)
        || partial.generated < base.generated
    {
        return Err("partial recording date must be valid and no older than the base".into());
    }
    let revision = base
        .tools
        .strip_prefix("storage.proto ")
        .and_then(|note| note.split_once(';'));
    if !revision
        .is_some_and(|(hash, _)| hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
        || base.tools != partial.tools
    {
        return Err(
            "recordings must have identical storage.proto revision and tools metadata".into(),
        );
    }
    let mut ids = BTreeSet::new();
    if !base.cases.iter().all(|case| ids.insert(case.id.clone())) {
        return Err("base recording has duplicate case ids".into());
    }
    let mut replacements = BTreeSet::new();
    if partial.cases.is_empty() {
        return Err("partial recording has no cases".into());
    }
    for case in &partial.cases {
        if !ids.contains(&case.id) || !replacements.insert(case.id.clone()) {
            return Err("partial recording has unknown or duplicate case ids".into());
        }
        if case.error.is_some() || case.steps.iter().any(|step| step.result.is_err()) {
            return Err("partial recording contains a failed case or step".into());
        }
    }
    base.requests = base
        .requests
        .checked_add(partial.requests)
        .ok_or("recording request count overflow")?;
    base.generated = partial.generated;
    base.recorded_dates.extend(partial.recorded_dates);
    base.source = Source::Live;
    for case in partial.cases {
        let slot = base
            .cases
            .iter_mut()
            .find(|existing| existing.id == case.id)
            .ok_or("partial case not found in base")?;
        *slot = case;
    }
    Ok(base)
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
    let mut value = json!({
        "format": FORMAT,
        "generated": recording.generated,
        "recorded_dates": recording.recorded_dates,
        "requests": recording.requests,
        "tools": recording.tools,
        "cases": cases,
    });
    let source = match recording.source {
        Source::Live => Some("live"),
        Source::SelfCheck => Some("self-check"),
        Source::Legacy => None,
    };
    if let Some(source) = source {
        value["source"] = json!(source);
    }
    value
}

fn recorded_dates(value: &Value, generated: &str) -> Result<BTreeSet<String>> {
    let dates = match value.get("recorded_dates") {
        None => BTreeSet::from([generated.to_owned()]),
        Some(dates) => dates
            .as_array()
            .ok_or("recorded_dates must be an array")?
            .iter()
            .map(|date| {
                date.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "recorded date must be a string".into())
            })
            .collect::<Result<BTreeSet<_>>>()?,
    };
    if dates.is_empty()
        || !dates.iter().all(|date| crate::date(date))
        || dates.last().map(String::as_str) != Some(generated)
    {
        return Err("recorded dates must be valid with generated as the latest date".into());
    }
    Ok(dates)
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
    let source = match value.get("source") {
        None => Source::Legacy,
        Some(Value::String(source)) if source == "live" => Source::Live,
        Some(Value::String(source)) if source == "self-check" => Source::SelfCheck,
        _ => return Err("unknown recording source".into()),
    };
    let generated = value["generated"].as_str().unwrap_or_default();
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
        generated: generated.to_owned(),
        recorded_dates: recorded_dates(value, generated)?,
        source,
        requests: value["requests"].as_u64().unwrap_or(0),
        tools: value["tools"].as_str().unwrap_or_default().to_owned(),
        cases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn fixture(ids: &[&str], generated: &str, requests: u64) -> Recording {
        Recording {
            generated: generated.into(),
            recorded_dates: BTreeSet::from([generated.into()]),
            source: Source::Live,
            requests,
            tools: format!("storage.proto {}; grpcurl 1.9.4", "a".repeat(40)),
            cases: ids
                .iter()
                .map(|id| CaseOutcome {
                    id: (*id).into(),
                    error: None,
                    bucket: Some("own".into()),
                    steps: vec![],
                })
                .collect(),
        }
    }

    #[test]
    fn merge_replaces_only_selected_cases_and_preserves_order_and_metadata() {
        let mut base = fixture(&["a", "b", "c"], "2026-11-01", 100);
        base.cases[1].bucket = Some("shared".into());
        let untouched = [base.cases[0].clone(), base.cases[2].clone()];
        let partial = fixture(&["b"], "2026-11-02", 12);
        let replacement = partial.cases[0].clone();
        let merged = merge(base, partial).unwrap();
        assert_eq!(
            merged.cases,
            [untouched[0].clone(), replacement, untouched[1].clone()]
        );
        assert_eq!(merged.generated, "2026-11-02");
        assert_eq!(merged.requests, 112);
        assert_eq!(merged.source, Source::Live);
        assert_eq!(
            merged.recorded_dates,
            BTreeSet::from(["2026-11-01".into(), "2026-11-02".into()])
        );
        assert_eq!(to_json(&merged)["format"], FORMAT);
        assert_eq!(from_json(&to_json(&merged)).unwrap().tools, merged.tools);
    }

    #[test]
    fn merge_requires_live_nonempty_provenance_but_accepts_a_legacy_base() {
        let base_json = to_json(&fixture(&["b"], "2026-11-01", 100));
        let mut legacy = base_json.clone();
        legacy.as_object_mut().unwrap().remove("source");
        legacy.as_object_mut().unwrap().remove("recorded_dates");
        let base = from_json(&legacy).unwrap();
        assert_eq!(base.source, Source::Legacy);
        assert!(merge(base, fixture(&["b"], "2026-11-02", 12)).is_ok());
        for source in [Source::Legacy, Source::SelfCheck] {
            let mut partial = fixture(&["b"], "2026-11-02", 12);
            partial.source = source;
            assert!(merge(from_json(&base_json).unwrap(), partial).is_err());
        }
        let mut self_check = fixture(&["b"], "2026-11-01", 100);
        self_check.source = Source::SelfCheck;
        assert!(merge(self_check, fixture(&["b"], "2026-11-02", 12)).is_err());
        assert!(
            merge(
                from_json(&base_json).unwrap(),
                fixture(&["b"], "2026-11-02", 0)
            )
            .is_err()
        );
        let mut unknown = base_json;
        unknown["source"] = json!("unknown");
        assert!(from_json(&unknown).is_err());
    }

    #[test]
    fn merge_refuses_old_partial_dates_and_failed_observations() {
        assert!(
            merge(
                fixture(&["b"], "2026-11-02", 100),
                fixture(&["b"], "2026-11-01", 12)
            )
            .is_err()
        );
        let mut failed_case = fixture(&["b"], "2026-11-02", 12);
        failed_case.cases[0].error = Some("setup failed".into());
        assert!(merge(fixture(&["b"], "2026-11-01", 100), failed_case).is_err());
        let mut failed_step = fixture(&["b"], "2026-11-02", 12);
        failed_step.cases[0].steps.push(StepOutcome {
            name: "get".into(),
            request: "GET /x".into(),
            result: Err("transport failed".into()),
            expectation: None,
            retries: 0,
            fingerprint: "00ff".into(),
        });
        assert!(merge(fixture(&["b"], "2026-11-01", 100), failed_step).is_err());
        let mut bad_dates = to_json(&fixture(&["b"], "2026-11-02", 12));
        bad_dates["recorded_dates"] = json!(["2026-11-03"]);
        assert!(from_json(&bad_dates).is_err());
    }

    #[test]
    fn merge_refuses_incompatible_metadata_and_ambiguous_cases() {
        for tools in [
            format!("storage.proto {}; grpcurl 1.9.4", "b".repeat(40)),
            format!("storage.proto {}; grpcurl 1.9.5", "a".repeat(40)),
            String::new(),
        ] {
            let mut partial = fixture(&["b"], "2026-11-02", 12);
            partial.tools = tools;
            assert!(merge(fixture(&["a", "b"], "2026-11-01", 100), partial).is_err());
        }
        for ids in [&[][..], &["unknown"][..], &["b", "b"][..]] {
            assert!(
                merge(
                    fixture(&["a", "b"], "2026-11-01", 100),
                    fixture(ids, "2026-11-02", 12)
                )
                .is_err()
            );
        }
        assert!(
            merge(
                fixture(&["b", "b"], "2026-11-01", 100),
                fixture(&["b"], "2026-11-02", 12)
            )
            .is_err()
        );
        assert!(
            merge(
                fixture(&["b"], "2026-11-01", u64::MAX),
                fixture(&["b"], "2026-11-02", 1)
            )
            .is_err()
        );
        let mut incompatible = to_json(&fixture(&["b"], "2026-11-02", 12));
        incompatible["format"] = json!(FORMAT + 1);
        assert!(from_json(&incompatible).is_err());
    }

    #[test]
    fn round_trips_responses_errors_and_expectations() {
        let recording = Recording {
            generated: "2026-11-01".into(),
            recorded_dates: BTreeSet::from(["2026-11-01".into()]),
            source: Source::Live,
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
