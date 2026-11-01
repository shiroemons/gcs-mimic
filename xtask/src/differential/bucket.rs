//! Creation and cleanup of the disposable buckets the harness owns.
//!
//! Cleanup never stops at the first failure: it records the problem, goes on
//! with the rest and reports all of them at the end, because an early return
//! would leave the bucket (and its objects, holds and notifications) behind.

use super::normalize::{Normalizer, project_numbers};
use super::runner::Side;
use super::template::percent_encode;
use super::transport::{Request, Response};
use serde_json::{Value, json};
use std::{thread::sleep, time::Duration};

type Result<T> = std::result::Result<T, String>;

/// Every bucket the harness creates carries this label, so a leftover can be
/// recognized as the harness's.
pub(crate) const LABEL: &str = "gcs-mimic-differential";
/// The start of every bucket name the harness creates.
pub(crate) const NAME_PREFIX: &str = "gcsmimic-diff-";

fn send_retrying(side: &Side<'_>, request: &Request) -> Result<Response> {
    let mut attempt = 0;
    loop {
        let response = side.transport.send(request)?;
        if !matches!(response.status, 500 | 502 | 503 | 504) || attempt == 3 {
            return Ok(response);
        }
        attempt += 1;
        sleep(Duration::from_secs(3 * attempt).min(side.server_error_wait));
    }
}

fn request(method: &str, target: String, body: Option<Value>) -> Request {
    Request {
        method: method.into(),
        target,
        headers: body
            .iter()
            .map(|_| ("Content-Type".to_owned(), "application/json".to_owned()))
            .collect(),
        body: body.map(|b| b.to_string().into_bytes()),
    }
}

/// A status and a short, redacted excerpt: a 403 body can name the caller.
fn describe(norm: &Normalizer, response: &Response) -> String {
    let body = String::from_utf8_lossy(&response.body);
    let excerpt: String = norm.text(body.trim()).chars().take(300).collect();
    format!("HTTP {}: {excerpt}", response.status)
}

/// Creates a private us-central1 STANDARD bucket with soft delete off, labelled
/// as the harness's. Returns the project numbers found in the response so they
/// can be redacted later.
pub(crate) fn create(side: &Side<'_>, norm: &Normalizer, name: &str) -> Result<Vec<String>> {
    let body = json!({
        "name": name,
        "location": "us-central1",
        "storageClass": "STANDARD",
        "labels": {LABEL: "1"},
        "iamConfiguration": {"publicAccessPrevention": "enforced"},
        "softDeletePolicy": {"retentionDurationSeconds": "0"},
    });
    let target = format!("/storage/v1/b?project={}", percent_encode(side.project));
    let response = send_retrying(side, &request("POST", target, Some(body)))?;
    if response.status != 200 {
        return Err(describe(norm, &response));
    }
    let mut numbers = Vec::new();
    if let Ok(json) = serde_json::from_slice(&response.body) {
        project_numbers(&json, &mut numbers);
    }
    Ok(numbers)
}

/// Adds the harness label to the body of a request that creates `new_bucket`,
/// so a sweep can tell the bucket is the harness's. The case file stays as it
/// is (its recording's fingerprints keep matching); the label is hidden again
/// by [`strip_label`]. Returns whether the label was added. A body that is
/// not a JSON object, or whose `labels` is not an object or is already full,
/// is left alone: those steps test exactly that.
pub(crate) fn add_label(request: &mut Request, new_bucket: &str) -> bool {
    let path = request.target.split('?').next().unwrap_or_default();
    if request.method != "POST" || path != "/storage/v1/b" {
        return false;
    }
    let Some(Ok(mut body)) = request.body.as_deref().map(serde_json::from_slice::<Value>) else {
        return false;
    };
    if body["name"].as_str() != Some(new_bucket) {
        return false;
    }
    let Some(object) = body.as_object_mut() else {
        return false;
    };
    let labels = object
        .entry("labels")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(labels) = labels.as_object_mut() else {
        return false;
    };
    // 64 labels is the service's limit; one more would turn the step into an error.
    if labels.len() >= 64 || labels.contains_key(LABEL) {
        return false;
    }
    labels.insert(LABEL.into(), json!("1"));
    request.body = Some(body.to_string().into_bytes());
    true
}

/// Removes the harness label from every `labels` object of a response (and the
/// `labels` object itself when that leaves it empty, as the service omits
/// empty labels), so a bucket labelled by [`add_label`] compares like one
/// that was not.
pub(crate) fn strip_label(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if let Some(Value::Object(labels)) = map.get_mut("labels") {
                labels.remove(LABEL);
                if labels.is_empty() {
                    map.remove("labels");
                }
            }
            map.values_mut().for_each(strip_label);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_label),
        _ => {}
    }
}

/// Whether a leftover bucket must carry the harness label to be cleaned up.
/// A sweep of everything with the shared prefix requires it, so a bucket of
/// someone else that happens to share the prefix is left alone; the end of a
/// run lists only its own random run id and does not need it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LabelFilter {
    Required,
    Ignored,
}

/// The project's buckets that start with `prefix`: the names to clean up, and
/// the names left alone for lacking the label. A bucket whose labels a PUT or
/// a PATCH replaced, as a case may do, looks like this too.
pub(crate) fn list_prefixed(
    side: &Side<'_>,
    norm: &Normalizer,
    prefix: &str,
    filter: LabelFilter,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut names = Vec::new();
    let mut unlabelled = Vec::new();
    let mut page: Option<String> = None;
    loop {
        let mut target = format!(
            "/storage/v1/b?project={}&prefix={}&fields=items(name,labels),nextPageToken",
            percent_encode(side.project),
            percent_encode(prefix)
        );
        if let Some(token) = &page {
            target.push_str(&format!("&pageToken={}", percent_encode(token)));
        }
        let response = send_retrying(side, &request("GET", target, None))?;
        if response.status != 200 {
            return Err(format!("list buckets: {}", describe(norm, &response)));
        }
        let listing: Value = serde_json::from_slice(&response.body).map_err(|e| e.to_string())?;
        for item in listing["items"].as_array().into_iter().flatten() {
            let Some(name) = item["name"].as_str().filter(|n| n.starts_with(prefix)) else {
                continue;
            };
            if filter == LabelFilter::Required && item["labels"][LABEL].as_str() != Some("1") {
                unlabelled.push(name.to_owned());
            } else {
                names.push(name.to_owned());
            }
        }
        match listing["nextPageToken"].as_str() {
            Some(token) => page = Some(token.to_owned()),
            None => return Ok((names, unlabelled)),
        }
    }
}

/// Statuses that mean a listing of `what` does not exist for this bucket (it is
/// gone, or the feature is not offered), so there is nothing to clean up.
const ABSENT: [u16; 2] = [400, 404];
/// A managed folders listing is also refused with 412 when uniform bucket-level
/// access is off and with 403 without permission: neither means a folder is left.
const FOLDERS_ABSENT: [u16; 4] = [400, 403, 404, 412];

/// One paged GET of a listing; its `items`. A status in `absent` yields what
/// was read so far without a problem.
fn list_all(
    side: &Side<'_>,
    norm: &Normalizer,
    target: &str,
    what: &str,
    absent: &[u16],
    problems: &mut Vec<String>,
) -> Vec<Value> {
    let mut items = Vec::new();
    let mut page: Option<String> = None;
    loop {
        let mut url = target.to_owned();
        if let Some(token) = &page {
            url.push_str(&format!("&pageToken={}", percent_encode(token)));
        }
        let response = match send_retrying(side, &request("GET", url, None)) {
            Ok(response) => response,
            Err(error) => {
                problems.push(format!("list {what}: {}", norm.text(&error)));
                return items;
            }
        };
        match response.status {
            200 => {}
            status if absent.contains(&status) => return items,
            _ => {
                problems.push(format!("list {what}: {}", describe(norm, &response)));
                return items;
            }
        }
        let Ok(listing) = serde_json::from_slice::<Value>(&response.body) else {
            problems.push(format!("list {what}: not JSON"));
            return items;
        };
        items.extend(listing["items"].as_array().cloned().unwrap_or_default());
        match listing["nextPageToken"].as_str() {
            Some(token) => page = Some(token.to_owned()),
            None => return items,
        }
    }
}

/// Sends a request and records a failure unless the status is one of `ok`.
fn expect(
    side: &Side<'_>,
    norm: &Normalizer,
    what: &str,
    req: &Request,
    ok: &[u16],
    problems: &mut Vec<String>,
) -> Option<Response> {
    match send_retrying(side, req) {
        Ok(response) if ok.contains(&response.status) => Some(response),
        Ok(response) => {
            problems.push(format!("{what}: {}", describe(norm, &response)));
            None
        }
        Err(error) => {
            problems.push(format!("{what}: {}", norm.text(&error)));
            None
        }
    }
}

/// Deletes one object version, first releasing a hold or an unlocked
/// retention when the delete is refused.
fn delete_version(
    side: &Side<'_>,
    norm: &Normalizer,
    name: &str,
    object: &str,
    generation: &str,
    problems: &mut Vec<String>,
) {
    let path = format!("/storage/v1/b/{name}/o/{}", percent_encode(object));
    let delete = request("DELETE", format!("{path}?generation={generation}"), None);
    let first = match send_retrying(side, &delete) {
        Ok(response) => response,
        Err(error) => {
            problems.push(format!("delete object: {}", norm.text(&error)));
            return;
        }
    };
    if matches!(first.status, 204 | 404) {
        return;
    }
    if first.status == 403 {
        let release = request(
            "PATCH",
            format!("{path}?generation={generation}&overrideUnlockedRetention=true"),
            Some(json!({"temporaryHold": false, "eventBasedHold": false, "retention": null})),
        );
        if expect(
            side,
            norm,
            "release object hold",
            &release,
            &[200, 404],
            problems,
        )
        .is_none()
        {
            return;
        }
        let again = request("DELETE", format!("{path}?generation={generation}"), None);
        expect(
            side,
            norm,
            "delete object after release",
            &again,
            &[204, 404],
            problems,
        );
        return;
    }
    problems.push(format!("delete object: {}", describe(norm, &first)));
}

/// Removes what blocks or outlives a bucket delete: the retention policy,
/// requester pays, notifications and managed folders.
/// The bucket settings that block a delete or cost a retry, from the bucket's
/// own metadata: only what is set is sent, because a service may refuse a
/// setting it does not support even when the value only switches it off.
fn blocking_settings(bucket: &Value) -> Option<Value> {
    let mut patch = serde_json::Map::new();
    if !bucket["retentionPolicy"].is_null() {
        patch.insert("retentionPolicy".into(), Value::Null);
    }
    if bucket["defaultEventBasedHold"].as_bool() == Some(true) {
        patch.insert("defaultEventBasedHold".into(), json!(false));
    }
    if bucket["billing"]["requesterPays"].as_bool() == Some(true) {
        patch.insert("billing".into(), json!({"requesterPays": false}));
    }
    (!patch.is_empty()).then_some(Value::Object(patch))
}

fn clear_bucket_settings(
    side: &Side<'_>,
    norm: &Normalizer,
    name: &str,
    bucket: &Value,
    problems: &mut Vec<String>,
) {
    if let Some(body) = blocking_settings(bucket) {
        let patch = request("PATCH", format!("/storage/v1/b/{name}"), Some(body));
        expect(
            side,
            norm,
            "clear bucket retention, hold and requester pays",
            &patch,
            &[200],
            problems,
        );
    }
    let target = format!("/storage/v1/b/{name}/notificationConfigs?");
    for item in list_all(side, norm, &target, "notifications", &ABSENT, problems) {
        if let Some(id) = item["id"].as_str() {
            let delete = request(
                "DELETE",
                format!(
                    "/storage/v1/b/{name}/notificationConfigs/{}",
                    percent_encode(id)
                ),
                None,
            );
            expect(
                side,
                norm,
                "delete notification",
                &delete,
                &[204, 404],
                problems,
            );
        }
    }
    let target = format!("/storage/v1/b/{name}/managedFolders?");
    for item in list_all(
        side,
        norm,
        &target,
        "managed folders",
        &FOLDERS_ABSENT,
        problems,
    ) {
        if let Some(folder) = item["name"].as_str() {
            let delete = request(
                "DELETE",
                format!(
                    "/storage/v1/b/{name}/managedFolders/{}?allowNonEmpty=true",
                    percent_encode(folder)
                ),
                None,
            );
            expect(
                side,
                norm,
                "delete managed folder",
                &delete,
                &[204, 404],
                problems,
            );
        }
    }
}

/// Empties and deletes a bucket, then confirms it answers 404. A bucket that is
/// already gone is not an error. Every failure is collected; the error holds
/// all of them.
pub(crate) fn cleanup(side: &Side<'_>, norm: &Normalizer, name: &str) -> Result<String> {
    let probe = request(
        "GET",
        format!("/storage/v1/b/{name}?fields=name,retentionPolicy,defaultEventBasedHold,billing"),
        None,
    );
    let mut bucket = Value::Null;
    if let Ok(response) = send_retrying(side, &probe) {
        if response.status == 404 {
            return Ok("already absent (404)".into());
        }
        bucket = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
    }
    let mut problems = Vec::new();
    clear_bucket_settings(side, norm, name, &bucket, &mut problems);
    let listing =
        format!("/storage/v1/b/{name}/o?versions=true&fields=items(name,generation),nextPageToken");
    let versions = list_all(
        side,
        norm,
        &listing,
        "object versions",
        &ABSENT,
        &mut problems,
    );
    let mut deleted = 0;
    for item in &versions {
        match (item["name"].as_str(), item["generation"].as_str()) {
            (Some(object), Some(generation)) => {
                delete_version(side, norm, name, object, generation, &mut problems);
                deleted += 1;
            }
            _ => problems.push("object without name or generation".into()),
        }
    }
    let delete = request("DELETE", format!("/storage/v1/b/{name}"), None);
    expect(
        side,
        norm,
        "delete bucket",
        &delete,
        &[204, 404],
        &mut problems,
    );
    let check = request("GET", format!("/storage/v1/b/{name}?fields=name"), None);
    let gone = match send_retrying(side, &check) {
        Ok(response) if response.status == 404 => true,
        Ok(response) => {
            problems.push(format!(
                "bucket still answers: {}",
                describe(norm, &response)
            ));
            false
        }
        Err(error) => {
            problems.push(format!("confirm deletion: {}", norm.text(&error)));
            false
        }
    };
    if problems.is_empty() {
        Ok(format!(
            "emptied ({deleted} object versions), deleted, GET confirmed 404"
        ))
    } else {
        // Say what state the bucket is in, so a failed step is not read as a leftover.
        let state = if gone {
            "the bucket itself is deleted (GET confirmed 404)"
        } else {
            "the bucket may still exist"
        };
        Err(format!("{}; {state}", problems.join("; ")))
    }
}

/// The report line for buckets left alone. They are named: a case that replaces a bucket's
/// labels leaves its own bucket looking like this after a crash, and the person running the
/// sweep has to decide.
fn left_alone_line(norm: &Normalizer, unlabelled: &[String]) -> Option<String> {
    if unlabelled.is_empty() {
        return None;
    }
    let listed: Vec<String> = unlabelled.iter().map(|name| norm.text(name)).collect();
    Some(format!(
        "sweep: {} bucket(s) with the prefix but without the {LABEL} label were left alone (delete them by hand if they are leftovers): {}",
        listed.len(),
        listed.join(", ")
    ))
}

/// Lists the buckets whose names start with `prefix` and cleans each one up.
/// Returns one line per bucket (never its name) and whether any failed.
pub(crate) fn sweep(
    side: &Side<'_>,
    norm: &Normalizer,
    prefix: &str,
    filter: LabelFilter,
) -> (Vec<String>, bool) {
    let (names, unlabelled) = match list_prefixed(side, norm, prefix, filter) {
        Ok(found) => found,
        Err(error) => return (vec![format!("sweep: CLEANUP FAILED: {error}")], true),
    };
    let mut lines = vec![format!("sweep: {} leftover bucket(s) found", names.len())];
    lines.extend(left_alone_line(norm, &unlabelled));
    let mut failed = false;
    for (index, name) in names.iter().enumerate() {
        match cleanup(side, norm, name) {
            Ok(note) => lines.push(format!("sweep leftover #{}: {note}", index + 1)),
            Err(error) => {
                failed = true;
                lines.push(format!(
                    "sweep leftover #{}: CLEANUP FAILED: {error}",
                    index + 1
                ));
            }
        }
    }
    (lines, failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::differential::transport::Transport;
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::time::Duration;

    /// A fake bucket holding a protected object, a notification, a managed
    /// folder and a retention policy, which answers like the real service:
    /// the object cannot be deleted while a hold is set.
    #[derive(Default)]
    struct FakeBucket {
        sent: RefCell<Vec<String>>,
        held: RefCell<bool>,
        notifications: RefCell<BTreeSet<String>>,
        folders: RefCell<BTreeSet<String>>,
        object_gone: RefCell<bool>,
        bucket_gone: RefCell<bool>,
        /// Make the managed folder delete fail, to prove cleanup goes on.
        folder_delete_fails: bool,
        /// Status of the managed folders listing when not 0 (412 without uniform
        /// bucket-level access, 403 without permission).
        folder_list_status: u16,
    }

    fn response(status: u16, body: &str) -> Result<Response> {
        Ok(Response {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        })
    }

    impl Transport for FakeBucket {
        fn send(&self, req: &Request) -> Result<Response> {
            let line = format!("{} {}", req.method, req.target);
            self.sent.borrow_mut().push(line.clone());
            let path = req.target.split('?').next().unwrap_or_default();
            let body =
                String::from_utf8_lossy(req.body.as_deref().unwrap_or_default()).into_owned();
            match (req.method.as_str(), path) {
                ("GET", "/storage/v1/b/bk") if *self.bucket_gone.borrow() => response(404, "{}"),
                ("GET", "/storage/v1/b/bk") => response(
                    200,
                    "{\"name\":\"bk\",\"retentionPolicy\":{\"retentionPeriod\":\"5\"},\"billing\":{\"requesterPays\":true}}",
                ),
                ("PATCH", "/storage/v1/b/bk") => {
                    assert!(
                        body.contains("\"retentionPolicy\":null")
                            && body.contains("\"requesterPays\":false"),
                        "{body}"
                    );
                    response(200, "{}")
                }
                ("GET", "/storage/v1/b/bk/notificationConfigs") => {
                    let items: Vec<String> = self
                        .notifications
                        .borrow()
                        .iter()
                        .map(|id| format!("{{\"id\":\"{id}\"}}"))
                        .collect();
                    response(200, &format!("{{\"items\":[{}]}}", items.join(",")))
                }
                ("DELETE", p) if p.starts_with("/storage/v1/b/bk/notificationConfigs/") => {
                    self.notifications.borrow_mut().clear();
                    response(204, "")
                }
                ("GET", "/storage/v1/b/bk/managedFolders") if self.folder_list_status != 0 => {
                    response(self.folder_list_status, "{\"error\":\"not offered\"}")
                }
                ("GET", "/storage/v1/b/bk/managedFolders") => {
                    let items: Vec<String> = self
                        .folders
                        .borrow()
                        .iter()
                        .map(|f| format!("{{\"name\":\"{f}\"}}"))
                        .collect();
                    response(200, &format!("{{\"items\":[{}]}}", items.join(",")))
                }
                ("DELETE", p) if p.starts_with("/storage/v1/b/bk/managedFolders/") => {
                    if self.folder_delete_fails {
                        return response(500, "boom");
                    }
                    self.folders.borrow_mut().clear();
                    response(204, "")
                }
                ("GET", "/storage/v1/b/bk/o") if *self.object_gone.borrow() => response(200, "{}"),
                ("GET", "/storage/v1/b/bk/o") => {
                    response(200, "{\"items\":[{\"name\":\"o1\",\"generation\":\"7\"}]}")
                }
                ("PATCH", "/storage/v1/b/bk/o/o1") => {
                    assert!(
                        req.target.contains("overrideUnlockedRetention=true"),
                        "{line}"
                    );
                    assert!(
                        body.contains("\"temporaryHold\":false")
                            && body.contains("\"retention\":null")
                    );
                    *self.held.borrow_mut() = false;
                    response(200, "{}")
                }
                ("DELETE", "/storage/v1/b/bk/o/o1") if *self.held.borrow() => {
                    response(403, "{\"error\":\"held by someone@example.com\"}")
                }
                ("DELETE", "/storage/v1/b/bk/o/o1") => {
                    *self.object_gone.borrow_mut() = true;
                    response(204, "")
                }
                ("DELETE", "/storage/v1/b/bk") => {
                    *self.bucket_gone.borrow_mut() = true;
                    response(204, "")
                }
                _ => response(500, &format!("unexpected {line}")),
            }
        }

        fn requests(&self) -> u64 {
            self.sent.borrow().len() as u64
        }
    }

    fn side(transport: &FakeBucket) -> Side<'_> {
        side_of(transport)
    }

    fn side_of(transport: &dyn Transport) -> Side<'_> {
        Side {
            transport,
            grpc: None,
            project: "proj",
            hosts: Vec::new(),
            server_error_wait: Duration::ZERO,
            label_new_buckets: false,
        }
    }

    #[test]
    fn cleanup_releases_holds_and_removes_notifications_folders_and_policies() {
        let fake = FakeBucket {
            held: RefCell::new(true),
            ..FakeBucket::default()
        };
        fake.notifications.borrow_mut().insert("1".into());
        fake.folders.borrow_mut().insert("folder/".into());
        let note = cleanup(&side(&fake), &Normalizer::default(), "bk").unwrap();
        assert!(note.contains("GET confirmed 404"), "{note}");
        let sent = fake.sent.borrow().join("\n");
        for needle in [
            "PATCH /storage/v1/b/bk\n",
            "DELETE /storage/v1/b/bk/notificationConfigs/1",
            "DELETE /storage/v1/b/bk/managedFolders/folder%2F?allowNonEmpty=true",
            "PATCH /storage/v1/b/bk/o/o1?generation=7&overrideUnlockedRetention=true",
        ] {
            assert!(sent.contains(needle), "missing {needle:?} in\n{sent}");
        }
        assert!(fake.notifications.borrow().is_empty() && fake.folders.borrow().is_empty());
    }

    fn create_request(body: Value) -> Request {
        request("POST", "/storage/v1/b?project=p".into(), Some(body))
    }

    #[test]
    fn created_buckets_get_the_label_and_the_response_hides_it_again() {
        let mut req = create_request(json!({"name": "nb", "labels": {"a": "1"}}));
        assert!(add_label(&mut req, "nb"));
        let sent: Value = serde_json::from_slice(req.body.as_deref().unwrap()).unwrap();
        assert_eq!(sent["labels"], json!({"a": "1", (LABEL): "1"}));
        let mut bare = create_request(json!({"name": "nb"}));
        assert!(add_label(&mut bare, "nb"));
        // Not a creation of the case's bucket, already labelled, or not an object: untouched.
        for mut other in [
            create_request(json!({"name": "other"})),
            create_request(json!({"name": "nb", "labels": "x"})),
            create_request(json!({"name": "nb", "labels": {(LABEL): "2"}})),
            request("PATCH", "/storage/v1/b".into(), Some(json!({"name": "nb"}))),
        ] {
            let before = other.body.clone();
            assert!(!add_label(&mut other, "nb"));
            assert_eq!(other.body, before);
        }
        let mut response =
            json!({"labels": {(LABEL): "1", "a": "1"}, "items": [{"labels": {(LABEL): "1"}}]});
        strip_label(&mut response);
        assert_eq!(response, json!({"labels": {"a": "1"}, "items": [{}]}));
    }

    /// Lists two buckets with the prefix, only one of them labelled.
    struct Listing;

    impl Transport for Listing {
        fn send(&self, req: &Request) -> Result<Response> {
            assert!(
                req.target.contains("fields=items(name,labels)"),
                "{}",
                req.target
            );
            response(
                200,
                &json!({"items": [
                    {"name": "gcsmimic-diff-a", "labels": {(LABEL): "1"}},
                    {"name": "gcsmimic-diff-b"},
                    {"name": "other", "labels": {(LABEL): "1"}},
                ]})
                .to_string(),
            )
        }
        fn requests(&self) -> u64 {
            0
        }
    }

    #[test]
    fn a_sweep_leaves_prefixed_buckets_without_the_label_alone() {
        let side = side_of(&Listing);
        let norm = Normalizer::default();
        let (names, left) =
            list_prefixed(&side, &norm, NAME_PREFIX, LabelFilter::Required).unwrap();
        assert_eq!(
            (names, left),
            (
                vec!["gcsmimic-diff-a".to_owned()],
                vec!["gcsmimic-diff-b".to_owned()]
            )
        );
        let (names, left) = list_prefixed(&side, &norm, NAME_PREFIX, LabelFilter::Ignored).unwrap();
        assert_eq!((names.len(), left.len()), (2, 0));

        // The report names the bucket it left alone.
        let line = left_alone_line(&norm, &["gcsmimic-diff-b".to_owned()]).unwrap();
        assert!(
            line.contains("1 bucket(s)") && line.contains("gcsmimic-diff-b"),
            "{line}"
        );
        assert!(left_alone_line(&norm, &[]).is_none());
    }

    #[test]
    fn a_refused_managed_folders_listing_means_no_folders_and_cleanup_goes_on() {
        for status in [412, 403] {
            let fake = FakeBucket {
                folder_list_status: status,
                ..FakeBucket::default()
            };
            let note = cleanup(&side(&fake), &Normalizer::default(), "bk")
                .unwrap_or_else(|e| panic!("{status}: {e}"));
            assert!(note.contains("GET confirmed 404"), "{note}");
        }
        // Any other status is still a problem, and the report says the bucket is gone.
        let fake = FakeBucket {
            folder_list_status: 500,
            ..FakeBucket::default()
        };
        let error = cleanup(&side(&fake), &Normalizer::default(), "bk").unwrap_err();
        assert!(error.contains("list managed folders"), "{error}");
        assert!(error.contains("the bucket itself is deleted"), "{error}");
    }

    #[test]
    fn a_failure_does_not_stop_the_rest_and_all_failures_are_reported_redacted() {
        let fake = FakeBucket {
            held: RefCell::new(true),
            folder_delete_fails: true,
            ..FakeBucket::default()
        };
        fake.folders.borrow_mut().insert("folder/".into());
        let error = cleanup(&side(&fake), &Normalizer::default(), "bk").unwrap_err();
        assert!(error.contains("delete managed folder"), "{error}");
        let sent = fake.sent.borrow().join("\n");
        assert!(
            sent.contains("DELETE /storage/v1/b/bk/o/o1?generation=7\n")
                || sent.contains("DELETE /storage/v1/b/bk/o/o1")
        );
        assert!(sent.ends_with("GET /storage/v1/b/bk?fields=name"), "{sent}");
        assert!(
            fake.bucket_gone.borrow().to_owned(),
            "the bucket delete must still run"
        );

        // An object delete that stays refused is reported without the caller's email.
        struct AlwaysForbidden(FakeBucket);
        impl Transport for AlwaysForbidden {
            fn send(&self, req: &Request) -> Result<Response> {
                if req.method == "PATCH" && req.target.contains("/o/") {
                    return response(403, "denied for someone@example.com");
                }
                self.0.send(req)
            }
            fn requests(&self) -> u64 {
                self.0.requests()
            }
        }
        let held = AlwaysForbidden(FakeBucket {
            held: RefCell::new(true),
            ..FakeBucket::default()
        });
        let error = cleanup(
            &Side {
                transport: &held,
                grpc: None,
                project: "proj",
                hosts: Vec::new(),
                server_error_wait: Duration::ZERO,
                label_new_buckets: false,
            },
            &Normalizer::default(),
            "bk",
        )
        .unwrap_err();
        assert!(
            error.contains("release object hold") && error.contains("<email>"),
            "{error}"
        );
        assert!(!error.contains("someone@"), "{error}");
        assert!(held.0.bucket_gone.borrow().to_owned());
    }
}
