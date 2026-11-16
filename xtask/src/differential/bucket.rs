//! Creation and cleanup of the disposable buckets the harness owns.
//!
//! Cleanup never stops at the first failure: it records the problem, goes on
//! with the rest and reports all of them at the end, because an early return
//! would leave the bucket (and its objects, holds and notifications) behind.

use super::normalize::{Normalizer, project_numbers};
use super::runner::Service;
use super::template::percent_encode;
use super::transport::{Request, Response, Wire};
use super::xml;
use serde_json::{Value, json};
use std::{str::FromStr, thread::sleep, time::Duration};

type Result<T> = std::result::Result<T, String>;

/// Every bucket the harness creates carries this label, so a leftover can be
/// recognized as the harness's.
pub(crate) const LABEL: &str = "gcs-mimic-differential";
/// The start of every bucket name the harness creates.
pub(crate) const NAME_PREFIX: &str = "gcsmimic-diff-";

fn send_retrying(side: &Service<'_>, request: &Request) -> Result<Response> {
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
        wire: Wire::Http,
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
pub(crate) fn create(side: &Service<'_>, norm: &Normalizer, name: &str) -> Result<Vec<String>> {
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
    side: &Service<'_>,
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
    side: &Service<'_>,
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
    side: &Service<'_>,
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
    side: &Service<'_>,
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
    side: &Service<'_>,
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

/// An open XML multipart upload, which is also the position a listing resumes after.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenUpload {
    key: String,
    id: String,
}

impl OpenUpload {
    /// Reads the child elements that name an upload (`Key` and `UploadId`, or the next
    /// markers); `None` when either is absent or empty.
    fn from_children(node: &Value, key: &str, id: &str) -> Option<Self> {
        let text = |element: &str| {
            xml::children(node)
                .find(|child| child["name"] == element)
                .and_then(xml::text)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        Some(Self {
            key: text(key)?,
            id: text(id)?,
        })
    }

    /// The path-style abort: every segment of the name encoded, the separators kept.
    fn abort_request(&self, bucket: &str) -> Request {
        let segments: Vec<String> = self.key.split('/').map(percent_encode).collect();
        let target = format!(
            "/{bucket}/{}?uploadId={}",
            segments.join("/"),
            percent_encode(&self.id)
        );
        request("DELETE", target, None)
    }
}

/// One page of a bucket's XML multipart upload listing.
struct UploadPage {
    uploads: Vec<OpenUpload>,
    /// Where the next page starts; `None` on the last page.
    next: Option<OpenUpload>,
}

impl UploadPage {
    fn request(bucket: &str, after: Option<&OpenUpload>) -> Request {
        let mut target = format!("/{bucket}?uploads");
        if let Some(after) = after {
            target.push_str(&format!(
                "&key-marker={}&upload-id-marker={}",
                percent_encode(&after.key),
                percent_encode(&after.id)
            ));
        }
        request("GET", target, None)
    }
}

impl FromStr for UploadPage {
    type Err = String;

    fn from_str(body: &str) -> Result<Self> {
        let root = xml::parse(body).map_err(|e| format!("not XML: {e}"))?;
        let uploads = xml::children(&root)
            .filter(|child| child["name"] == "Upload")
            .map(|upload| {
                OpenUpload::from_children(upload, "Key", "UploadId")
                    .ok_or("an Upload without Key or UploadId")
            })
            .collect::<std::result::Result<_, _>>()?;
        let truncated = xml::children(&root)
            .any(|child| child["name"] == "IsTruncated" && xml::text(child) == Some("true"));
        let next = truncated
            .then(|| {
                OpenUpload::from_children(&root, "NextKeyMarker", "NextUploadIdMarker")
                    .ok_or("a truncated page without next markers")
            })
            .transpose()?;
        Ok(Self { uploads, next })
    }
}

/// One listing page; `Ok(None)` when there is no listing to read (the bucket is gone).
fn list_uploads(
    side: &Service<'_>,
    norm: &Normalizer,
    name: &str,
    after: Option<&OpenUpload>,
) -> Result<Option<UploadPage>> {
    let response =
        send_retrying(side, &UploadPage::request(name, after)).map_err(|e| norm.text(&e))?;
    match response.status {
        200 => String::from_utf8_lossy(&response.body).parse().map(Some),
        status if ABSENT.contains(&status) => Ok(None),
        _ => Err(describe(norm, &response)),
    }
}

/// Aborts every XML multipart upload left open in the bucket, as a case that fails
/// between starting and finishing one leaves it. Returns how many were aborted.
fn abort_uploads(
    side: &Service<'_>,
    norm: &Normalizer,
    name: &str,
    problems: &mut Vec<String>,
) -> usize {
    let mut aborted = 0;
    let mut after = None;
    loop {
        let page = match list_uploads(side, norm, name, after.as_ref()) {
            Ok(Some(page)) => page,
            Ok(None) => return aborted,
            Err(error) => {
                problems.push(format!("list multipart uploads: {error}"));
                return aborted;
            }
        };
        for upload in &page.uploads {
            let abort = upload.abort_request(name);
            let answered = expect(
                side,
                norm,
                "abort multipart upload",
                &abort,
                &[204, 404],
                problems,
            );
            if answered.is_some() {
                aborted += 1;
            }
        }
        match page.next {
            None => return aborted,
            // A page that does not move the markers forward would be listed again forever.
            Some(next) if after.as_ref() == Some(&next) => {
                problems
                    .push("list multipart uploads: a truncated page repeats its markers".into());
                return aborted;
            }
            next => after = next,
        }
    }
}

/// Empties and deletes a bucket (open multipart uploads included), then confirms
/// it answers 404. A bucket that is already gone is not an error. Every failure
/// is collected; the error holds all of them.
pub(crate) fn cleanup(side: &Service<'_>, norm: &Normalizer, name: &str) -> Result<String> {
    let probe = request(
        "GET",
        format!("/storage/v1/b/{name}?fields=name,retentionPolicy,defaultEventBasedHold,billing"),
        None,
    );
    // Without the settings, holds and retention stay set and the deletion below can
    // fail; the probe's failure is then the reason, so it leads the report.
    let (bucket, probe_failure) = match send_retrying(side, &probe) {
        Ok(response) if response.status == 404 => return Ok("already absent (404)".into()),
        Ok(response) if !(200..300).contains(&response.status) => (
            Value::Null,
            Some(format!(
                "read bucket settings: {}",
                describe(norm, &response)
            )),
        ),
        Ok(response) => match serde_json::from_slice(&response.body) {
            Ok(bucket) => (bucket, None),
            Err(error) => (Value::Null, Some(format!("read bucket settings: {error}"))),
        },
        Err(error) => (
            Value::Null,
            Some(format!("read bucket settings: {}", norm.text(&error))),
        ),
    };
    let mut problems = Vec::new();
    clear_bucket_settings(side, norm, name, &bucket, &mut problems);
    let uploads = abort_uploads(side, norm, name, &mut problems);
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
        // Named only when there were any, so the usual line stays as it was.
        let aborted = if uploads > 0 {
            format!(", {uploads} multipart uploads aborted")
        } else {
            String::new()
        };
        Ok(format!(
            "emptied ({deleted} object versions{aborted}), deleted, GET confirmed 404"
        ))
    } else {
        if let Some(failure) = probe_failure {
            problems.insert(0, failure);
        }
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
/// Returns one line per cleaned bucket (never its name), a line naming the labelless buckets it
/// left alone, and whether any failed.
pub(crate) fn sweep(
    side: &Service<'_>,
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
    use crate::differential::runner::NewBuckets;
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
        /// Open multipart uploads as (key, upload ID), listed one per page.
        uploads: RefCell<BTreeSet<(String, String)>>,
        /// Answer every multipart listing as truncated with the same markers.
        uploads_stuck: bool,
    }

    impl FakeBucket {
        /// One upload per page after the request's markers, as an XML document sent as
        /// text/html like Cloud Storage does.
        fn uploads_page(&self, target: &str) -> Result<Response> {
            if self.uploads_stuck {
                return response(
                    200,
                    "<?xml version='1.0'?><ListMultipartUploadsResult><NextKeyMarker>k</NextKeyMarker>\
                     <NextUploadIdMarker>U1</NextUploadIdMarker><IsTruncated>true</IsTruncated>\
                     </ListMultipartUploadsResult>",
                );
            }
            let query = target.split_once('?').map_or("", |(_, q)| q);
            let param = |name: &str| {
                query
                    .split('&')
                    .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
                    .map(crate::differential::template::percent_decode)
            };
            let after = (
                param("key-marker").unwrap_or_default(),
                param("upload-id-marker").unwrap_or_default(),
            );
            let uploads = self.uploads.borrow();
            let mut rest = uploads.iter().filter(|upload| **upload > after);
            let page = rest.next();
            let truncated = rest.next().is_some();
            let (next_key, next_id) = page.cloned().unwrap_or_default();
            let entry = page.map_or_else(String::new, |(key, id)| {
                format!("<Upload><Key>{key}</Key><UploadId>{id}</UploadId></Upload>")
            });
            let body = format!(
                "<?xml version='1.0' encoding='UTF-8'?><ListMultipartUploadsResult>\
                 <NextKeyMarker>{next_key}</NextKeyMarker><NextUploadIdMarker>{next_id}</NextUploadIdMarker>\
                 <IsTruncated>{truncated}</IsTruncated>{entry}</ListMultipartUploadsResult>"
            );
            response(200, &body)
        }
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
                ("GET", "/bk") if req.target.starts_with("/bk?uploads") => {
                    self.uploads_page(&req.target)
                }
                ("DELETE", p) if p.starts_with("/bk/") => {
                    let key = crate::differential::template::percent_decode(&p["/bk/".len()..]);
                    let id = req.target.split_once("?uploadId=").map(|(_, id)| id);
                    let removed = id
                        .is_some_and(|id| self.uploads.borrow_mut().remove(&(key, id.to_owned())));
                    response(if removed { 204 } else { 404 }, "")
                }
                _ => response(500, &format!("unexpected {line}")),
            }
        }

        fn requests(&self) -> u64 {
            self.sent.borrow().len() as u64
        }
    }

    fn side(transport: &FakeBucket) -> Service<'_> {
        side_of(transport)
    }

    fn side_of(transport: &dyn Transport) -> Service<'_> {
        Service {
            transport,
            grpc: None,
            project: "proj",
            hosts: Vec::new(),
            server_error_wait: Duration::ZERO,
            new_buckets: NewBuckets::AsIs,
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

    #[test]
    fn cleanup_aborts_every_open_multipart_upload_page_by_page() {
        let fake = FakeBucket::default();
        for (key, id) in [("d/mp.bin", "U1"), ("f g.bin", "U2")] {
            fake.uploads
                .borrow_mut()
                .insert((key.to_owned(), id.to_owned()));
        }
        let note = cleanup(&side(&fake), &Normalizer::default(), "bk").unwrap();
        assert_eq!(
            note,
            "emptied (1 object versions, 2 multipart uploads aborted), deleted, GET confirmed 404"
        );
        assert!(fake.uploads.borrow().is_empty());
        let sent = fake.sent.borrow().join("\n");
        for needle in [
            "GET /bk?uploads\n",
            "DELETE /bk/d/mp.bin?uploadId=U1\n",
            "GET /bk?uploads&key-marker=d%2Fmp.bin&upload-id-marker=U1\n",
            "DELETE /bk/f%20g.bin?uploadId=U2\n",
        ] {
            assert!(sent.contains(needle), "missing {needle:?} in\n{sent}");
        }
        // The uploads go before the bucket, whose delete they would otherwise block.
        let aborted = sent.find("DELETE /bk/f%20g.bin").unwrap();
        assert!(aborted < sent.find("DELETE /storage/v1/b/bk\n").unwrap());
    }

    #[test]
    fn an_upload_page_needs_named_uploads_and_next_markers_when_truncated() {
        let page = |inner: &str| {
            format!("<?xml version='1.0'?><ListMultipartUploadsResult>{inner}</ListMultipartUploadsResult>")
                .parse::<UploadPage>()
        };
        let last = page(
            "<NextKeyMarker></NextKeyMarker><IsTruncated>false</IsTruncated>\
             <Upload><Key>a</Key><UploadId>U1</UploadId></Upload>",
        )
        .unwrap();
        let upload = OpenUpload {
            key: "a".into(),
            id: "U1".into(),
        };
        assert_eq!((last.uploads, last.next), (vec![upload.clone()], None));
        let more = page(
            "<NextKeyMarker>a</NextKeyMarker><NextUploadIdMarker>U1</NextUploadIdMarker>\
             <IsTruncated>true</IsTruncated>",
        )
        .unwrap();
        assert_eq!(more.next, Some(upload));
        for (inner, error) in [
            (
                "<Upload><Key>a</Key><UploadId></UploadId></Upload>",
                "an Upload without Key or UploadId",
            ),
            (
                "<IsTruncated>true</IsTruncated><NextKeyMarker>a</NextKeyMarker>",
                "a truncated page without next markers",
            ),
        ] {
            assert_eq!(page(inner).err().as_deref(), Some(error));
        }
        assert!("Not Found".parse::<UploadPage>().is_err());
    }

    #[test]
    fn a_multipart_listing_that_does_not_advance_is_reported_not_repeated() {
        let fake = FakeBucket {
            uploads_stuck: true,
            ..FakeBucket::default()
        };
        let error = cleanup(&side(&fake), &Normalizer::default(), "bk").unwrap_err();
        assert!(
            error.contains("a truncated page repeats its markers"),
            "{error}"
        );
        assert!(error.contains("the bucket itself is deleted"), "{error}");
        let lists = fake
            .sent
            .borrow()
            .iter()
            .filter(|line| line.starts_with("GET /bk?uploads"))
            .count();
        assert_eq!(lists, 2, "the second page repeats the markers and stops");
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

    /// Answers the settings probe with `probe`, refuses the bucket delete when
    /// `delete_fails`, and passes everything else to a [`FakeBucket`].
    struct ProbeFails {
        bucket: FakeBucket,
        probe: Result<Response>,
        delete_fails: bool,
    }

    impl Transport for ProbeFails {
        fn send(&self, req: &Request) -> Result<Response> {
            if req.method == "GET" && req.target.contains("fields=name,retentionPolicy") {
                return self.probe.clone();
            }
            if self.delete_fails && req.method == "DELETE" && req.target == "/storage/v1/b/bk" {
                return response(409, "{\"error\":\"retention policy not met\"}");
            }
            self.bucket.send(req)
        }
        fn requests(&self) -> u64 {
            self.bucket.requests()
        }
    }

    #[test]
    fn a_failed_settings_probe_leads_the_report_only_when_cleanup_fails() {
        let probes = [
            (
                Err("connection reset for someone@example.com".to_owned()),
                "read bucket settings: connection reset for <email>",
            ),
            (response(200, "<html>"), "read bucket settings: "),
            (
                response(403, "{\"error\":\"denied for someone@example.com\"}"),
                "read bucket settings: HTTP 403",
            ),
        ];
        for (probe, start) in probes {
            let failing = ProbeFails {
                bucket: FakeBucket::default(),
                probe: probe.clone(),
                delete_fails: true,
            };
            let error = cleanup(&side_of(&failing), &Normalizer::default(), "bk").unwrap_err();
            assert!(error.starts_with(start), "{error}");
            assert!(error.contains("delete bucket: HTTP 409"), "{error}");
            assert!(!error.contains("someone@"), "{error}");
            // A cleanup that succeeds without the settings reports as before.
            let passing = ProbeFails {
                bucket: FakeBucket::default(),
                probe,
                delete_fails: false,
            };
            let note = cleanup(&side_of(&passing), &Normalizer::default(), "bk").unwrap();
            assert_eq!(
                note,
                "emptied (1 object versions), deleted, GET confirmed 404"
            );
        }
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
            &Service {
                transport: &held,
                grpc: None,
                project: "proj",
                hosts: Vec::new(),
                server_error_wait: Duration::ZERO,
                new_buckets: NewBuckets::AsIs,
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
