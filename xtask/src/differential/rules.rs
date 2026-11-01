//! Static safety rules for a case file, checked before any request is sent.
//! They keep a case from touching what a live project must never see (locked
//! retention, public access, stray buckets, shared-bucket metadata). The
//! expanded request is checked again by [`super::guard`] right before it is
//! sent, because a captured variable can change what a template becomes.

use super::case::{Body, BucketMode, Case, Protocol, Step, step_variables};
use super::guard;
use super::runner::build_request;
use super::template::Vars;
use serde_json::Value;

type Result<T> = std::result::Result<T, String>;

/// RPCs that change bucket-level state of the bucket they name.
const MUTATING_BUCKET_RPCS: [&str; 6] = [
    "UpdateBucket",
    "DeleteBucket",
    "SetIamPolicy",
    "CreateNotificationConfig",
    "DeleteNotificationConfig",
    "LockBucketRetentionPolicy",
];

/// JSON API sub-resources whose writes change the bucket itself.
const BUCKET_SUBRESOURCES: [&str; 5] = [
    "/iam",
    "/acl",
    "/defaultObjectAcl",
    "/notificationConfigs",
    "/managedFolders",
];

pub(crate) fn check(case: &Case) -> Result<()> {
    for step in &case.steps {
        check_step(case, step).map_err(|e| format!("step {:?}: {e}", step.name))?;
    }
    Ok(())
}

fn check_step(case: &Case, step: &Step) -> Result<()> {
    check_unexpanded_lock(step)?;
    check_expanded_request(step)?;
    check_bucket_creation(step)?;
    check_pap_reset(step)?;
    check_iam_keeps_admin(step)?;
    if case.bucket == BucketMode::Shared && changes_shared_bucket(step) {
        return Err(
            "changes the metadata of the shared {bucket}; the case needs \"bucket\": \"own\""
                .into(),
        );
    }
    Ok(())
}

/// A variable could resolve to a lock at run time, so a mode or `isLocked`
/// that is not a literal is refused here, on the template.
fn check_unexpanded_lock(step: &Step) -> Result<()> {
    let reason = match &step.body {
        Body::Json(value) => guard::json_locks(value),
        Body::Text(text) => {
            serde_json::from_str::<Value>(&text.replace("{{", "{").replace("}}", "}"))
                .ok()
                .and_then(|value| guard::json_locks(&value))
        }
        Body::None | Body::Bytes(_) => None,
    };
    match reason {
        Some(reason) => Err(format!("forbidden locked retention: {reason}")),
        None => Ok(()),
    }
}

/// Expands every variable to a placeholder and runs the live guard on the
/// result, so the same rules apply to the template and to the sent request.
fn check_expanded_request(step: &Step) -> Result<()> {
    let vars: Vars = step_variables(step)?
        .into_iter()
        .map(|name| (name, "x1".to_owned()))
        .collect();
    let request = build_request(step, &vars)?;
    guard::check_request(&request).map_err(|e| format!("forbidden {e}"))
}

/// Whether the literal is a name the service must reject, so a step creating
/// it cannot create anything.
fn invalid_bucket_name(name: &str) -> bool {
    !name.contains('{')
        && (name.len() < 3
            || name.chars().any(|c| c.is_ascii_uppercase() || c == '_')
            || name.starts_with(['-', '.'])
            || name.contains("goog"))
}

fn strip_query(path: &str) -> &str {
    path.split_once('?').map_or(path, |(head, _)| head)
}

fn check_bucket_creation(step: &Step) -> Result<()> {
    match step.protocol {
        Protocol::Grpc => {
            let rpc = step.method.rsplit('/').next().unwrap_or_default();
            if rpc == "CreateBucket" || rpc == "SetIamPolicy" {
                return Err(format!("forbidden: the {rpc} RPC is not used by cases"));
            }
        }
        Protocol::Xml => {
            let segments = strip_query(&step.path).trim_matches('/').split('/').count();
            if step.method == "PUT" && segments == 1 {
                return Err("forbidden: an XML PUT on a bucket path would create a bucket".into());
            }
        }
        Protocol::Http => {
            if step.method == "POST" && strip_query(&step.path) == "/storage/v1/b" {
                check_json_bucket_creation(step)?;
            }
        }
    }
    Ok(())
}

fn check_json_bucket_creation(step: &Step) -> Result<()> {
    let body = match &step.body {
        Body::Json(value) => value.clone(),
        // A body that is not JSON cannot name a bucket.
        Body::Text(text) if serde_json::from_str::<Value>(text).is_err() => return Ok(()),
        Body::Text(text) => serde_json::from_str(text).map_err(|e| e.to_string())?,
        Body::None | Body::Bytes(_) => return Ok(()),
    };
    let name = body["name"].as_str().unwrap_or_default();
    if invalid_bucket_name(name) {
        return Ok(());
    }
    if name != "{new_bucket}" {
        return Err("forbidden: a bucket the case creates must be named {new_bucket}".into());
    }
    let pap = &body["iamConfiguration"]["publicAccessPrevention"];
    if pap.as_str() != Some("enforced") {
        return Err(
            "a created bucket needs iamConfiguration.publicAccessPrevention \"enforced\"".into(),
        );
    }
    if body["softDeletePolicy"]["retentionDurationSeconds"].is_null() {
        return Err(
            "a created bucket needs an explicit softDeletePolicy.retentionDurationSeconds".into(),
        );
    }
    Ok(())
}

fn json_body(step: &Step) -> Option<Value> {
    match &step.body {
        Body::Json(value) => Some(value.clone()),
        Body::Text(text) => serde_json::from_str(&text.replace("{{", "{").replace("}}", "}")).ok(),
        Body::None | Body::Bytes(_) => None,
    }
}

/// A bucket PUT replaces the whole resource and a PATCH nulling
/// `iamConfiguration` removes it, so either can reset public access prevention
/// to its default even though no step names the setting. The live guard cannot
/// see that, so the case must say why the step is acceptable (`pap_reset`).
fn check_pap_reset(step: &Step) -> Result<()> {
    // `expect_status` is no exemption: an expectation of a refusal can be wrong,
    // and then the step resets the setting.
    if step.protocol != Protocol::Http || step.pap_reset.is_some() {
        return Ok(());
    }
    let path = strip_query(&step.path);
    let on_bucket = path
        .strip_prefix("/storage/v1/b/")
        .is_some_and(|rest| !rest.is_empty() && !rest.contains('/'));
    if !on_bucket || !matches!(step.method.as_str(), "PUT" | "PATCH") {
        return Ok(());
    }
    let Some(body) = json_body(step) else {
        return Ok(());
    };
    let config = &body["iamConfiguration"];
    let enforced = config["publicAccessPrevention"].as_str() == Some("enforced");
    let resets = match step.method.as_str() {
        "PUT" => !enforced,
        _ => body.get("iamConfiguration").is_some() && !config.is_object(),
    };
    if resets {
        return Err(
            "could reset public access prevention (a PUT without it, or a PATCH \
                    nulling iamConfiguration); say why in \"pap_reset\" or set it to enforced"
                .into(),
        );
    }
    Ok(())
}

/// Roles that let the measuring account keep managing the bucket.
const ADMIN_ROLES: [&str; 2] = ["roles/storage.admin", "roles/storage.legacyBucketOwner"];

/// A `setIamPolicy` body replaces the whole policy. The creator's access comes
/// from the project owner and editor bindings, so a body that drops them locks
/// the measuring account out and every later step answers 403.
fn check_iam_keeps_admin(step: &Step) -> Result<()> {
    if step.protocol != Protocol::Http
        || step.method != "PUT"
        || !strip_query(&step.path).ends_with("/iam")
    {
        return Ok(());
    }
    let Some(body) = json_body(step) else {
        return Ok(());
    };
    // A body without literal bindings is the subject of the step itself.
    let Some(bindings) = body["bindings"].as_array() else {
        return Ok(());
    };
    let keeps_admin = bindings.iter().any(|binding| {
        binding["role"]
            .as_str()
            .is_some_and(|role| ADMIN_ROLES.contains(&role))
            && binding["members"].as_array().is_some_and(|members| {
                members.iter().filter_map(Value::as_str).any(|member| {
                    member.starts_with("projectOwner:") || member.starts_with("projectEditor:")
                })
            })
    });
    // The service rejects a policy it refuses (invalid member or role, stale
    // etag) before applying it, so such a step cannot lock anyone out.
    let applied = step.expect_status.is_none_or(|status| status == 200);
    if keeps_admin || !applied {
        return Ok(());
    }
    Err("a setIamPolicy body must keep roles/storage.admin or \
         roles/storage.legacyBucketOwner for projectOwner: or projectEditor:, or the \
         measuring account loses access to the bucket"
        .into())
}

/// A step that writes bucket-level state of the shared bucket: it would leak
/// into every case that runs after it, and into the next live run.
fn changes_shared_bucket(step: &Step) -> bool {
    if matches!(step.method.as_str(), "GET" | "HEAD") {
        return false;
    }
    match step.protocol {
        Protocol::Grpc => {
            let rpc = step.method.rsplit('/').next().unwrap_or_default();
            MUTATING_BUCKET_RPCS.contains(&rpc) && step.mentions("{bucket}")
        }
        Protocol::Xml => {
            let path = strip_query(&step.path);
            path == "/{bucket}" || path == "/{bucket}/"
        }
        Protocol::Http => {
            let path = strip_query(&step.path);
            let Some(rest) = path.strip_prefix("/storage/v1/b/{bucket}") else {
                return false;
            };
            rest.is_empty()
                || rest == "/"
                || (BUCKET_SUBRESOURCES.iter().any(|sub| rest.starts_with(sub))
                    && !rest.starts_with("/iam/testPermissions"))
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::differential::case::parse_case;
    use serde_json::{Value, json};

    fn case_of(bucket: Value, step: Value) -> Value {
        json!({"id": "x", "description": "d", "bucket": bucket, "steps": [step]})
    }

    fn rejects(bucket: Value, step: Value, needle: &str) {
        let err = parse_case(&case_of(bucket, step.clone()), "x").unwrap_err();
        assert!(err.contains(needle), "{step}: {err}");
    }

    #[test]
    fn shared_bucket_metadata_writes_are_refused_but_reads_and_objects_pass() {
        let patch =
            json!({"name": "s", "method": "PATCH", "path": "/storage/v1/b/{bucket}", "body": {}});
        rejects(json!(true), patch.clone(), "shared {bucket}");
        assert!(parse_case(&case_of(json!("own"), patch), "x").is_ok());
        let ok = [
            json!({"name": "s", "method": "GET", "path": "/storage/v1/b/{bucket}"}),
            json!({"name": "s", "method": "PATCH", "path": "/storage/v1/b/{bucket}/o/{object}", "body": {}}),
            json!({"name": "s", "method": "POST", "path": "/storage/v1/b/{bucket}/iam/testPermissions"}),
            json!({"name": "s", "method": "PUT", "path": "/{bucket}/{object}", "protocol": "xml", "headers": {"Content-Type": "text/plain"}}),
        ];
        for step in ok {
            assert!(
                parse_case(&case_of(json!(true), step.clone()), "x").is_ok(),
                "{step}"
            );
        }
        for step in [
            json!({"name": "s", "method": "PUT", "path": "/storage/v1/b/{bucket}/iam", "body": {}}),
            json!({"name": "s", "method": "POST", "path": "/storage/v1/b/{bucket}/notificationConfigs", "body": {}}),
            json!({"name": "s", "method": "DELETE", "path": "/{bucket}", "protocol": "xml"}),
            json!({"name": "s", "protocol": "grpc", "rpc": "google.storage.v2.Storage/UpdateBucket", "body": {"bucket": {"name": "projects/_/buckets/{bucket}"}}}),
        ] {
            rejects(json!(true), step, "shared {bucket}");
        }
    }

    #[test]
    fn created_buckets_are_named_new_bucket_and_made_private() {
        let create = |body: Value| json!({"name": "s", "method": "POST", "path": "/storage/v1/b", "query": {"project": "{project}"}, "body": body});
        let good = json!({"name": "{new_bucket}", "iamConfiguration": {"publicAccessPrevention": "enforced"},
            "softDeletePolicy": {"retentionDurationSeconds": "0"}});
        assert!(parse_case(&case_of(json!(false), create(good)), "x").is_ok());
        rejects(
            json!(false),
            create(
                json!({"name": "{new_bucket}", "softDeletePolicy": {"retentionDurationSeconds": "0"}}),
            ),
            "publicAccessPrevention",
        );
        rejects(
            json!(false),
            create(
                json!({"name": "{new_bucket}", "iamConfiguration": {"publicAccessPrevention": "enforced"}}),
            ),
            "softDeletePolicy",
        );
        rejects(
            json!(false),
            create(json!({"name": "{bucket}-other"})),
            "named {new_bucket}",
        );
        assert!(
            parse_case(
                &case_of(json!(false), create(json!({"name": "Invalid_Name"}))),
                "x"
            )
            .is_ok()
        );
        rejects(
            json!(false),
            json!({"name": "s", "protocol": "xml", "method": "PUT", "path": "/{new_bucket}"}),
            "create a bucket",
        );
        rejects(
            json!(false),
            json!({"name": "s", "protocol": "grpc", "rpc": "google.storage.v2.Storage/CreateBucket"}),
            "CreateBucket",
        );
    }

    #[test]
    fn bucket_writes_that_could_reset_public_access_prevention_need_a_reason() {
        let put = json!({"name": "s", "method": "PUT", "path": "/storage/v1/b/{new_bucket}",
                         "body": {"name": "{new_bucket}"}, "expect_status": 400});
        rejects(json!(false), put.clone(), "pap_reset");
        let patch_null = json!({"name": "s", "method": "PATCH", "path": "/storage/v1/b/{new_bucket}",
                                "body": {"iamConfiguration": null}});
        rejects(json!(false), patch_null.clone(), "pap_reset");
        for step in [put, patch_null] {
            let mut with_reason = step;
            with_reason["pap_reset"] = json!("measures the reset");
            assert!(parse_case(&case_of(json!(false), with_reason), "x").is_ok());
        }
        let ok = [
            json!({"name": "s", "method": "PUT", "path": "/storage/v1/b/{new_bucket}",
                   "body": {"name": "{new_bucket}", "iamConfiguration": {"publicAccessPrevention": "enforced"}}}),
            json!({"name": "s", "method": "PATCH", "path": "/storage/v1/b/{new_bucket}", "body": {"iamConfiguration": {}}}),
            json!({"name": "s", "method": "PUT", "path": "/storage/v1/b/{new_bucket}/o/x", "body": {}}),
        ];
        for step in ok {
            assert!(
                parse_case(&case_of(json!(false), step.clone()), "x").is_ok(),
                "{step}"
            );
        }
    }

    #[test]
    fn an_applied_iam_policy_must_keep_an_admin_role_for_project_owners_or_editors() {
        let set = |status: Option<u16>, bindings: Value| {
            let mut step = json!({"name": "s", "method": "PUT", "path": "/storage/v1/b/{new_bucket}/iam",
                                  "body": {"version": 1, "bindings": bindings}});
            if let Some(status) = status {
                step["expect_status"] = json!(status);
            }
            step
        };
        let viewer =
            json!([{"role": "roles/storage.objectViewer", "members": ["projectViewer:{project}"]}]);
        rejects(json!(false), set(Some(200), viewer.clone()), "must keep");
        rejects(json!(false), set(None, viewer.clone()), "must keep");
        // A refused policy is never applied, so it cannot lock anyone out.
        assert!(parse_case(&case_of(json!(false), set(Some(400), viewer)), "x").is_ok());
        let admin = json!([{"role": "roles/storage.admin", "members": ["projectOwner:{project}"]}]);
        assert!(parse_case(&case_of(json!(false), set(Some(200), admin)), "x").is_ok());
        let owner = json!([{"role": "roles/storage.legacyBucketOwner", "members": ["projectEditor:{project}"]}]);
        assert!(parse_case(&case_of(json!(false), set(Some(200), owner)), "x").is_ok());
        let wrong_member =
            json!([{"role": "roles/storage.admin", "members": ["projectViewer:{project}"]}]);
        rejects(json!(false), set(Some(200), wrong_member), "must keep");
    }

    #[test]
    fn expanded_requests_are_refused_for_public_and_locked_input() {
        rejects(
            json!(false),
            json!({"name": "s", "method": "PUT", "path": "/storage/v1/b/{new_bucket}/iam",
                   "body": {"bindings": [{"members": ["allUsers"]}]}}),
            "allUsers",
        );
        rejects(
            json!(false),
            json!({"name": "s", "protocol": "xml", "method": "PUT", "path": "/{new_bucket}/{object}",
                   "headers": {"x-goog-acl": "public-read"}}),
            "public canned ACL",
        );
        rejects(
            json!(false),
            json!({"name": "s", "method": "PUT", "path": "/b", "body_text": "<Mode>Locked</Mode>"}),
            "forbidden locked retention",
        );
    }
}
