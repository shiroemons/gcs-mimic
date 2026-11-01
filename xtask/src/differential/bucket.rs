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

/// Names of the project's buckets that start with `prefix`.
pub(crate) fn list_prefixed(
    side: &Side<'_>,
    norm: &Normalizer,
    prefix: &str,
) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut page: Option<String> = None;
    loop {
        let mut target = format!(
            "/storage/v1/b?project={}&prefix={}&fields=items(name),nextPageToken",
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
        names.extend(
            listing["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|item| item["name"].as_str())
                .filter(|name| name.starts_with(prefix))
                .map(str::to_owned),
        );
        match listing["nextPageToken"].as_str() {
            Some(token) => page = Some(token.to_owned()),
            None => return Ok(names),
        }
    }
}

/// One paged GET of a listing; the items under `key`, or `None` when the
/// bucket or the resource is gone or not offered (404, 400).
fn list_all(
    side: &Side<'_>,
    norm: &Normalizer,
    target: &str,
    key: &str,
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
                problems.push(format!("list {key}: {}", norm.text(&error)));
                return items;
            }
        };
        match response.status {
            200 => {}
            400 | 404 => return items,
            _ => {
                problems.push(format!("list {key}: {}", describe(norm, &response)));
                return items;
            }
        }
        let Ok(listing) = serde_json::from_slice::<Value>(&response.body) else {
            problems.push(format!("list {key}: not JSON"));
            return items;
        };
        items.extend(listing[key].as_array().cloned().unwrap_or_default());
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
fn clear_bucket_settings(
    side: &Side<'_>,
    norm: &Normalizer,
    name: &str,
    problems: &mut Vec<String>,
) {
    let patch = request(
        "PATCH",
        format!("/storage/v1/b/{name}"),
        Some(json!({
            "retentionPolicy": null,
            "defaultEventBasedHold": false,
            "billing": {"requesterPays": false},
        })),
    );
    expect(
        side,
        norm,
        "clear bucket retention and requester pays",
        &patch,
        &[200],
        problems,
    );
    let target = format!("/storage/v1/b/{name}/notificationConfigs?");
    for item in list_all(side, norm, &target, "items", problems) {
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
    for item in list_all(side, norm, &target, "items", problems) {
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
    let probe = request("GET", format!("/storage/v1/b/{name}?fields=name"), None);
    if let Ok(response) = send_retrying(side, &probe)
        && response.status == 404
    {
        return Ok("already absent (404)".into());
    }
    let mut problems = Vec::new();
    clear_bucket_settings(side, norm, name, &mut problems);
    let listing =
        format!("/storage/v1/b/{name}/o?versions=true&fields=items(name,generation),nextPageToken");
    let versions = list_all(side, norm, &listing, "items", &mut problems);
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
    match send_retrying(side, &check) {
        Ok(response) if response.status == 404 => {}
        Ok(response) => problems.push(format!(
            "bucket still answers: {}",
            describe(norm, &response)
        )),
        Err(error) => problems.push(format!("confirm deletion: {}", norm.text(&error))),
    }
    if problems.is_empty() {
        Ok(format!(
            "emptied ({deleted} object versions), deleted, GET confirmed 404"
        ))
    } else {
        Err(problems.join("; "))
    }
}

/// Lists the buckets whose names start with `prefix` and cleans each one up.
/// Returns one line per bucket (never its name) and whether any failed.
pub(crate) fn sweep(side: &Side<'_>, norm: &Normalizer, prefix: &str) -> (Vec<String>, bool) {
    let names = match list_prefixed(side, norm, prefix) {
        Ok(names) => names,
        Err(error) => return (vec![format!("sweep: CLEANUP FAILED: {error}")], true),
    };
    let mut lines = vec![format!("sweep: {} leftover bucket(s) found", names.len())];
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
                ("GET", "/storage/v1/b/bk") => response(200, "{\"name\":\"bk\"}"),
                ("PATCH", "/storage/v1/b/bk") => response(200, "{}"),
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
        Side {
            transport,
            grpc: None,
            project: "proj",
            hosts: Vec::new(),
            server_error_wait: Duration::ZERO,
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
