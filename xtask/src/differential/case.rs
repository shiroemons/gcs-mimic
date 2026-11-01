//! Declarative case files: parsing and static validation.

use super::template::{json_variables, variables};
use serde_json::{Map, Value};
use std::{collections::BTreeSet, fs, path::Path};

type Result<T> = std::result::Result<T, String>;

/// Variables the harness defines before the first step.
pub(crate) const BUILTIN_VARS: [&str; 6] = [
    "project",
    "bucket",
    "new_bucket",
    "missing_bucket",
    "object",
    "object2",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BodyMode {
    #[default]
    Auto,
    Json,
    Text,
    Ignore,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CompareOpts {
    pub(crate) ignore_headers: Vec<String>,
    pub(crate) only_headers: Option<Vec<String>>,
    pub(crate) ignore_body_paths: Vec<String>,
    pub(crate) body: BodyMode,
}

#[derive(Debug, Clone)]
pub(crate) enum Body {
    None,
    Json(Value),
    Text(String),
    /// Raw bytes from `body_base64`, for binary uploads such as gzip.
    Bytes(Vec<u8>),
}

/// How a step reaches the service. `Http` is the JSON API; `Xml` is the same
/// HTTP transport against the path-style XML API (`/{bucket}/{object}`), whose
/// XML bodies are compared as ordered trees; `Grpc` goes through `grpcurl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Protocol {
    #[default]
    Http,
    Xml,
    Grpc,
}

#[derive(Debug, Clone)]
pub(crate) struct Step {
    pub(crate) name: String,
    pub(crate) protocol: Protocol,
    /// The HTTP verb, or the full RPC name (`google.storage.v2.Storage/GetBucket`)
    /// for a gRPC step.
    pub(crate) method: String,
    /// Empty for a gRPC step.
    pub(crate) path: String,
    pub(crate) query: Vec<(String, String)>,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Body,
    pub(crate) expect_status: Option<u16>,
    /// Runs even after an earlier step failed the case (restoring versioning,
    /// releasing a hold), so the bucket can still be deleted.
    pub(crate) always: bool,
    /// Why this step may reset public access prevention to its default (a
    /// bucket PUT without `iamConfiguration`, a PATCH nulling it). The reason
    /// is required so such a step is a deliberate, reviewed exception.
    pub(crate) pap_reset: Option<String>,
    pub(crate) capture: Vec<(String, String)>,
    /// `None` runs the step without comparing it (setup or probe steps).
    pub(crate) compare: Option<CompareOpts>,
}

/// Which bucket `{bucket}` names. `Shared` is the one bucket every such case
/// uses, so a case there must leave no bucket-level state behind; `Own` gives
/// the case a bucket of its own (created before and deleted after it) for
/// anything that changes bucket metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum BucketMode {
    #[default]
    Shared,
    None,
    Own,
}

impl BucketMode {
    /// The name recorded with a case's outcome.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::None => "none",
            Self::Own => "own",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Case {
    pub(crate) id: String,
    pub(crate) description: String,

    pub(crate) bucket: BucketMode,
    pub(crate) steps: Vec<Step>,
}

impl Step {
    /// The request as the case file wrote it (variables unexpanded): method or
    /// RPC, path template, query, headers, body and ordering flags.
    pub(crate) fn mentions(&self, needle: &str) -> bool {
        self.fingerprint_text().contains(needle)
    }

    fn fingerprint_text(&self) -> String {
        let body = match &self.body {
            Body::None => String::new(),
            Body::Json(value) => format!("json:{value}"),
            Body::Text(text) => format!("text:{text}"),
            Body::Bytes(bytes) => format!("bytes:{}", super::base64::encode(bytes)),
        };
        format!(
            "{:?}|{}|{}|{:?}|{:?}|{body}|{}|{:?}",
            self.protocol,
            self.method,
            self.path,
            self.query,
            self.headers,
            self.always,
            self.capture
        )
    }
}

/// FNV-1a over the step's request definition, as 16 hex digits. A recording
/// stores one per step, so replay can tell that the case changed since the
/// reference was taken.
pub(crate) fn fingerprint(step: &Step) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in step.fingerprint_text().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

impl Case {
    pub(crate) fn uses_grpc(&self) -> bool {
        self.steps.iter().any(|s| s.protocol == Protocol::Grpc)
    }

    pub(crate) fn uses_variable(&self, name: &str) -> bool {
        self.steps.iter().any(|step| {
            step_variables(step)
                .map(|vars| vars.iter().any(|v| v == name))
                .unwrap_or(false)
        })
    }
}

pub(crate) fn step_variables(step: &Step) -> Result<Vec<String>> {
    let mut all = variables(&step.path)?;
    for (name, value) in step.query.iter().chain(&step.headers) {
        all.extend(variables(name)?);
        all.extend(variables(value)?);
    }
    match &step.body {
        Body::None => {}
        Body::Json(value) => all.extend(json_variables(value)?),
        Body::Text(text) => all.extend(variables(text)?),
        Body::Bytes(_) => {}
    }
    Ok(all)
}

fn reject_unknown(map: &Map<String, Value>, allowed: &[&str], context: &str) -> Result<()> {
    match map.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(format!("{context}: unknown field {key:?}")),
        None => Ok(()),
    }
}

fn string_list(value: &Value, context: &str) -> Result<Vec<String>> {
    value
        .as_array()
        .ok_or_else(|| format!("{context}: expected an array of strings"))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{context}: expected an array of strings"))
        })
        .collect()
}

fn string_pairs(value: &Value, context: &str) -> Result<Vec<(String, String)>> {
    value
        .as_object()
        .ok_or_else(|| format!("{context}: expected an object of strings"))?
        .iter()
        .map(|(key, item)| {
            item.as_str()
                .map(|text| (key.clone(), text.to_owned()))
                .ok_or_else(|| format!("{context}.{key}: expected a string"))
        })
        .collect()
}

fn parse_compare(value: &Value, base: &CompareOpts, context: &str) -> Result<CompareOpts> {
    let map = value
        .as_object()
        .ok_or_else(|| format!("{context}: expected an object"))?;
    reject_unknown(
        map,
        &[
            "ignore_headers",
            "only_headers",
            "ignore_body_paths",
            "body",
        ],
        context,
    )?;
    let mut opts = base.clone();
    if let Some(list) = map.get("ignore_headers") {
        opts.ignore_headers.extend(string_list(list, context)?);
    }
    if let Some(list) = map.get("only_headers") {
        opts.only_headers = Some(string_list(list, context)?);
    }
    if let Some(list) = map.get("ignore_body_paths") {
        opts.ignore_body_paths.extend(string_list(list, context)?);
    }
    if let Some(mode) = map.get("body") {
        opts.body = match mode.as_str() {
            Some("auto") => BodyMode::Auto,
            Some("json") => BodyMode::Json,
            Some("text") => BodyMode::Text,
            Some("ignore") => BodyMode::Ignore,
            _ => {
                return Err(format!(
                    "{context}.body: expected auto, json, text or ignore"
                ));
            }
        };
    }
    Ok(opts)
}

/// Resolves `protocol` with its target: `method` and `path` for HTTP and XML,
/// `rpc` for gRPC.
fn parse_target(map: &Map<String, Value>, context: &str) -> Result<(Protocol, String, String)> {
    let protocol = match map.get("protocol").map(Value::as_str) {
        None | Some(Some("http")) => Protocol::Http,
        Some(Some("xml")) => Protocol::Xml,
        Some(Some("grpc")) => Protocol::Grpc,
        Some(_) => {
            return Err(format!(
                "{context}.protocol: expected \"http\", \"xml\" or \"grpc\""
            ));
        }
    };
    let text = |key: &str| -> Result<String> {
        map.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("{context}.{key}: expected a nonempty string"))
    };
    if protocol == Protocol::Grpc {
        if let Some(field) = ["method", "path", "query"]
            .into_iter()
            .find(|f| map.contains_key(*f))
        {
            return Err(format!("{context}.{field}: not valid for a gRPC step"));
        }
        let rpc = text("rpc")?;
        let well_formed = rpc.split_once('/').is_some_and(|(service, method)| {
            !service.is_empty() && !method.is_empty() && !method.contains('/')
        });
        if !well_formed {
            return Err(format!("{context}.rpc: expected package.Service/Method"));
        }
        return Ok((protocol, rpc, String::new()));
    }
    if map.contains_key("rpc") {
        return Err(format!("{context}.rpc: only valid for a gRPC step"));
    }
    let method = text("method")?;
    if !["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"].contains(&method.as_str()) {
        return Err(format!("{context}.method: unsupported method {method}"));
    }
    let path = text("path")?;
    if !path.starts_with('/') {
        return Err(format!("{context}.path: must start with /"));
    }
    Ok((protocol, method, path))
}

/// A gRPC request is one JSON message, or an array of messages for a
/// client-streaming call; there is no text body.
fn check_grpc_step(body: &Body, context: &str) -> Result<()> {
    match body {
        Body::None => Ok(()),
        Body::Json(Value::Object(_)) => Ok(()),
        Body::Json(Value::Array(items)) if items.iter().all(Value::is_object) => Ok(()),
        Body::Json(_) => Err(format!(
            "{context}.body: expected a message object or an array of message objects"
        )),
        Body::Text(_) | Body::Bytes(_) => Err(format!(
            "{context}: a text or binary body is not valid for a gRPC step"
        )),
    }
}

fn parse_step(value: &Value, defaults: &CompareOpts, context: &str) -> Result<Step> {
    let map = value
        .as_object()
        .ok_or_else(|| format!("{context}: expected an object"))?;
    reject_unknown(
        map,
        &[
            "name",
            "protocol",
            "rpc",
            "method",
            "path",
            "query",
            "headers",
            "body",
            "body_text",
            "body_base64",
            "always",
            "pap_reset",
            "expect_status",
            "capture",
            "compare",
        ],
        context,
    )?;
    let text = |key: &str| -> Result<String> {
        map.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| format!("{context}.{key}: expected a nonempty string"))
    };
    let (protocol, method, path) = parse_target(map, context)?;
    let body = match (
        map.get("body"),
        map.get("body_text"),
        map.get("body_base64"),
    ) {
        (Some(json), None, None) => Body::Json(json.clone()),
        (None, Some(Value::String(text)), None) => Body::Text(text.clone()),
        (None, Some(_), None) => return Err(format!("{context}.body_text: expected a string")),
        (None, None, Some(Value::String(encoded))) => Body::Bytes(
            super::base64::decode(encoded).map_err(|e| format!("{context}.body_base64: {e}"))?,
        ),
        (None, None, Some(_)) => return Err(format!("{context}.body_base64: expected a string")),
        (None, None, None) => Body::None,
        _ => {
            return Err(format!(
                "{context}: body, body_text and body_base64 are exclusive"
            ));
        }
    };
    if protocol == Protocol::Grpc {
        check_grpc_step(&body, context)?;
        if let Some(headers) = map.get("headers") {
            for (name, value) in string_pairs(headers, context)? {
                if name.contains("${") || value.contains("${") {
                    return Err(format!(
                        "{context}.headers: \"${{\" is not allowed in gRPC metadata"
                    ));
                }
            }
        }
    }
    let expect_status = match map.get("expect_status") {
        None => None,
        Some(status) => Some(
            status
                .as_u64()
                .and_then(|s| u16::try_from(s).ok())
                .ok_or_else(|| format!("{context}.expect_status: expected an HTTP status"))?,
        ),
    };
    let compare = match map.get("compare") {
        None => Some(defaults.clone()),
        Some(Value::Bool(false)) => None,
        Some(Value::Bool(true)) => Some(defaults.clone()),
        Some(other) => Some(parse_compare(
            other,
            defaults,
            &format!("{context}.compare"),
        )?),
    };
    Ok(Step {
        name: text("name")?,
        protocol,
        method,
        path,
        query: map
            .get("query")
            .map(|v| string_pairs(v, &format!("{context}.query")))
            .transpose()?
            .unwrap_or_default(),
        headers: map
            .get("headers")
            .map(|v| string_pairs(v, &format!("{context}.headers")))
            .transpose()?
            .unwrap_or_default(),
        body,
        expect_status,
        pap_reset: map
            .get("pap_reset")
            .map(|_| text("pap_reset"))
            .transpose()?,
        always: match map.get("always") {
            None => false,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => return Err(format!("{context}.always: expected a boolean")),
        },
        capture: map
            .get("capture")
            .map(|v| string_pairs(v, &format!("{context}.capture")))
            .transpose()?
            .unwrap_or_default(),
        compare,
    })
}

fn is_case_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub(crate) fn parse_case(value: &Value, file_stem: &str) -> Result<Case> {
    let map = value
        .as_object()
        .ok_or_else(|| "case file must be a JSON object".to_owned())?;
    reject_unknown(
        map,
        &["id", "description", "bucket", "compare", "steps"],
        "case",
    )?;
    let id = map
        .get("id")
        .and_then(Value::as_str)
        .ok_or("case.id: expected a string")?;
    if !is_case_id(id) || id != file_stem {
        return Err(format!(
            "case.id {id:?}: must be lowercase letters, digits and hyphens, and equal the file name {file_stem:?}"
        ));
    }
    let description = map
        .get("description")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("case.description: expected a nonempty string")?;
    let bucket = match map.get("bucket") {
        None | Some(Value::Bool(true)) => BucketMode::Shared,
        Some(Value::Bool(false)) => BucketMode::None,
        Some(Value::String(mode)) if mode == "own" => BucketMode::Own,
        Some(_) => return Err("case.bucket: expected true, false or \"own\"".into()),
    };
    let defaults = match map.get("compare") {
        None => CompareOpts::default(),
        Some(value) => parse_compare(value, &CompareOpts::default(), "case.compare")?,
    };
    let steps: Vec<Step> = map
        .get("steps")
        .and_then(Value::as_array)
        .filter(|steps| !steps.is_empty())
        .ok_or("case.steps: expected a nonempty array")?
        .iter()
        .enumerate()
        .map(|(index, step)| parse_step(step, &defaults, &format!("steps[{index}]")))
        .collect::<Result<_>>()?;
    let case = Case {
        id: id.to_owned(),
        description: description.to_owned(),
        bucket,
        steps,
    };
    validate(&case)?;
    Ok(case)
}

/// Checks names, variable use and the live-bucket safety rules before any
/// request is sent.
fn validate(case: &Case) -> Result<()> {
    // First, so a lock hidden behind an undefined variable is still named as a lock.
    super::rules::check(case)?;
    let mut names = BTreeSet::new();
    let mut known: BTreeSet<String> = BUILTIN_VARS.iter().map(|v| (*v).to_owned()).collect();
    if case.bucket == BucketMode::None {
        known.remove("bucket");
    }
    for step in &case.steps {
        if !names.insert(step.name.as_str()) {
            return Err(format!("duplicate step name {:?}", step.name));
        }
        for var in step_variables(step)? {
            if !known.contains(&var) {
                return Err(format!(
                    "step {:?}: variable {{{var}}} is not defined before this step",
                    step.name
                ));
            }
        }
        for (var, _) in &step.capture {
            if BUILTIN_VARS.contains(&var.as_str()) {
                return Err(format!(
                    "step {:?}: capture {var:?} shadows a built-in",
                    step.name
                ));
            }
            known.insert(var.clone());
        }
    }
    Ok(())
}

pub(crate) fn load_cases(dir: &Path) -> Result<Vec<Case>> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    let mut cases = Vec::new();
    for path in files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_owned();
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let value: Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        cases.push(parse_case(&value, &stem).map_err(|e| format!("{}: {e}", path.display()))?);
    }
    if cases.is_empty() {
        return Err(format!("{}: no case files", dir.display()));
    }
    Ok(cases)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn case_with(step: Value) -> Value {
        json!({"id": "x", "description": "d", "steps": [step]})
    }

    fn get(path: &str) -> Value {
        json!({"name": "s", "method": "GET", "path": path})
    }

    #[test]
    fn parses_a_minimal_case_with_defaults() {
        let case = parse_case(&case_with(get("/storage/v1/b/{bucket}")), "x").unwrap();
        assert_eq!(case.bucket, BucketMode::Shared);
        assert!(case.steps[0].compare.is_some());
        assert!(case.uses_variable("bucket"));
        assert!(!case.uses_variable("new_bucket"));
    }

    #[test]
    fn rejects_unknown_fields_and_mismatched_ids() {
        let mut step = get("/p");
        step["typo"] = json!(1);
        assert!(
            parse_case(&case_with(step), "x")
                .unwrap_err()
                .contains("unknown field")
        );
        assert!(parse_case(&case_with(get("/p")), "y").is_err());
    }

    #[test]
    fn rejects_undefined_variables_but_accepts_earlier_captures() {
        let err = parse_case(&case_with(get("/o/{gen}")), "x").unwrap_err();
        assert!(err.contains("{gen}"));
        let value = json!({"id": "x", "description": "d", "steps": [
            {"name": "a", "method": "GET", "path": "/p", "capture": {"gen": "body:generation"}},
            {"name": "b", "method": "GET", "path": "/o/{gen}"}
        ]});
        assert!(parse_case(&value, "x").is_ok());
        let no_bucket = json!({"id": "x", "description": "d", "bucket": false,
            "steps": [get("/b/{bucket}")]});
        assert!(parse_case(&no_bucket, "x").is_err());
    }

    #[test]
    fn step_compare_merges_with_case_defaults() {
        let value = json!({"id": "x", "description": "d",
        "compare": {"ignore_headers": ["vary"], "body": "text"},
        "steps": [
            {"name": "a", "method": "GET", "path": "/p",
             "compare": {"ignore_headers": ["etag"]}},
            {"name": "b", "method": "GET", "path": "/p", "compare": false}
        ]});
        let case = parse_case(&value, "x").unwrap();
        let opts = case.steps[0].compare.as_ref().unwrap();
        assert_eq!(opts.ignore_headers, ["vary", "etag"]);
        assert_eq!(opts.body, BodyMode::Text);
        assert!(case.steps[1].compare.is_none());
    }

    #[test]
    fn parses_xml_and_grpc_steps_and_rejects_mixed_fields() {
        let xml =
            json!({"name": "s", "protocol": "xml", "method": "GET", "path": "/{bucket}/{object}"});
        let case = parse_case(&case_with(xml), "x").unwrap();
        assert_eq!(case.steps[0].protocol, Protocol::Xml);
        assert!(!case.uses_grpc());
        let grpc = json!({"name": "s", "protocol": "grpc", "rpc": "google.storage.v2.Storage/GetBucket",
            "headers": {"x-goog-request-params": "bucket=projects/_/buckets/{bucket}"},
            "body": {"name": "projects/_/buckets/{bucket}"}});
        let case = parse_case(&case_with(grpc), "x").unwrap();
        assert!(case.uses_grpc());
        assert!(case.uses_variable("bucket"));
        let rejected = [
            json!({"name": "s", "protocol": "grpc", "rpc": "NoSlash"}),
            json!({"name": "s", "protocol": "grpc", "rpc": "a.B/C", "path": "/p"}),
            json!({"name": "s", "protocol": "grpc", "rpc": "a.B/C", "body_text": "x"}),
            json!({"name": "s", "protocol": "grpc", "rpc": "a.B/C", "body": [1]}),
            json!({"name": "s", "protocol": "grpc", "method": "GET", "path": "/p"}),
            json!({"name": "s", "protocol": "xml", "rpc": "a.B/C", "method": "GET", "path": "/p"}),
            json!({"name": "s", "protocol": "ftp", "method": "GET", "path": "/p"}),
        ];
        for step in rejected {
            assert!(parse_case(&case_with(step.clone()), "x").is_err(), "{step}");
        }
    }

    #[test]
    fn rejects_locked_retention_in_grpc_spellings() {
        let forbidden = [
            json!({"name": "s", "protocol": "grpc", "rpc": "google.storage.v2.Storage/LockBucketRetentionPolicy"}),
            json!({"name": "s", "protocol": "grpc", "rpc": "a.B/C",
                   "body": {"bucket": {"retention_policy": {"is_locked": true}}}}),
        ];
        for step in forbidden {
            let err = parse_case(&case_with(step.clone()), "x").unwrap_err();
            assert!(err.contains("forbidden locked retention"), "{step}: {err}");
        }
    }

    #[test]
    fn rejects_numeric_named_and_templated_lock_modes() {
        let forbidden = [
            json!({"retention": {"mode": 2}}),
            json!({"retention": {"mode": "LOCKED"}}),
            json!({"retention": {"mode": "{mode}"}}),
            json!({"retention": {"mode": null}}),
            json!({"retentionPolicy": {"isLocked": "{flag}"}}),
            json!({"retention_policy": {"is_locked": 1}}),
        ];
        for body in forbidden {
            let step = json!({"name": "s", "protocol": "grpc", "rpc": "a.B/C", "body": body});
            let err = parse_case(&case_with(step), "x").unwrap_err();
            assert!(err.contains("forbidden locked retention"), "{body}: {err}");
        }
    }

    #[test]
    fn rejects_environment_expansion_in_grpc_metadata() {
        let step = json!({"name": "s", "protocol": "grpc", "rpc": "a.B/C",
            "headers": {"x-a": "${HOME}"}});
        assert!(
            parse_case(&case_with(step), "x")
                .unwrap_err()
                .contains("gRPC metadata")
        );
    }

    #[test]
    fn rejects_locked_retention_in_every_position() {
        let forbidden = [
            json!({"name": "s", "method": "POST", "path": "/storage/v1/b/{bucket}/lockRetentionPolicy"}),
            json!({"name": "s", "method": "PATCH", "path": "/p",
                   "body": {"retentionPolicy": {"retentionPeriod": "5", "isLocked": true}}}),
            json!({"name": "s", "method": "PATCH", "path": "/p",
                   "body": {"retention": {"mode": "locked"}}}),
            json!({"name": "s", "method": "POST", "path": "/p", "body_text": "{{\"isLocked\":\"TRUE\"}}"}),
            json!({"name": "s", "method": "POST", "path": "/p", "query": {"x": "lockRetentionPolicy"}}),
        ];
        for step in forbidden {
            let err = parse_case(&case_with(step.clone()), "x").unwrap_err();
            assert!(err.contains("forbidden locked retention"), "{step}: {err}");
        }
        let allowed = json!({"name": "s", "method": "PATCH", "path": "/p",
            "body": {"retention": {"mode": "Unlocked"}, "retentionPolicy": {"isLocked": false}}});
        assert!(parse_case(&case_with(allowed), "x").is_ok());
    }
}
